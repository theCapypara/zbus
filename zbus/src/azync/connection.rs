use async_io::Async;
use std::{
    io::{self, ErrorKind},
    os::unix::{io::AsRawFd, net::UnixStream},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use futures::{
    sink::{Sink, SinkExt},
    stream::{Stream, TryStreamExt},
};

use crate::{
    azync::Authenticated, raw::Socket, ConnectionCommon, Error, Guid, Message, MessageType, Result,
};

/// The asynchronous sibling of [`zbus::Connection`].
///
/// Most of the API is very similar to [`zbus::Connection`], except it's asynchronous. However,
/// there are a few differences:
///
/// ### Generic over Socket
///
/// This type is generic over [`zbus::raw::Socket`] so that support for new socket types can be
/// added with the same type easily later on.
///
/// ### No Clone implementation
///
/// Unlike [`zbus::Connection`], this type does not implement [`std::clone::Clone`]. The reason is
/// that implementation will be very difficult (and still prone to deadlocks) if connection is
/// owned by multiple tasks/threads. Create separate connection instances or use
/// [`futures::stream::StreamExt::split`] to split reading and writing between two separate async
/// tasks.
///
/// ### Sending Messages
///
/// For sending messages you can either use [`Connection::send_message`] method or make use of the
/// [`Sink`] implementation. For latter, you might find [`SinkExt`] API very useful. Keep in mind
/// that [`Connection`] will not manage the serial numbers (cookies) on the messages for you when
/// they are sent through the [`Sink`] implementation. You can manually assign unique serial numbers
/// to them using the [`Connection::assign_serial_num`] method before sending them off, if needed.
/// Having said that, [`Sink`] is mainly useful for sending out signals, as they do not expect a
/// reply, and serial numbers are not very useful for signals either for the same reason.
///
/// ### Receiving Messages
///
/// Unlike [`zbus::Connection`], there is no direct async equivalent of
/// [`zbus::Connection::receive_message`] method provided. This is because the `futures` crate
/// already provides already provides a nice rich API that makes use of the  [`Stream`]
/// implementation.
///
/// ### Examples
///
/// #### Get the session bus ID
///
/// ```
///# use zvariant::Type;
///
/// futures::executor::block_on(async {
///     let connection = zbus::azync::Connection::new_session()
///         .await
///         .unwrap();
///
///     let reply = connection
///         .call_method(
///             Some("org.freedesktop.DBus"),
///                  "/org/freedesktop/DBus",
///                  Some("org.freedesktop.DBus"),
///                  "GetId",
///                  &(),
///         )
///         .await
///         .unwrap();
///
///     assert!(reply
///         .body_signature()
///         .map(|s| s == <&str>::signature())
///         .unwrap());
///     let id: &str = reply.body().unwrap();
///     println!("Unique ID of the bus: {}", id);
/// });
/// ```
///
/// #### Monitoring all messages
///
/// Let's eavesdrop on the session bus 😈 using the [Monitor] interface:
///
/// ```rust,no_run
/// futures::executor::block_on(async {
///     use futures::TryStreamExt;
///
///     let mut connection = zbus::azync::Connection::new_session().await?;
///
///     connection
///         .call_method(
///             Some("org.freedesktop.DBus"),
///                  "/org/freedesktop/DBus",
///                  Some("org.freedesktop.DBus.Monitoring"),
///                  "BecomeMonitor",
///                  &(&[] as &[&str], 0u32),
///             )
///             .await?;
///
///     while let Some(msg) = connection.try_next().await? {
///         println!("Got message: {}", msg);
///     }
///
///     Ok::<(), zbus::Error>(())
/// });
/// ```
///
/// This should print something like:
///
/// ```console
/// Got message: Signal NameAcquired from org.freedesktop.DBus
/// Got message: Signal NameLost from org.freedesktop.DBus
/// Got message: Method call GetConnectionUnixProcessID from :1.1324
/// Got message: Error org.freedesktop.DBus.Error.NameHasNoOwner:
///              Could not get PID of name ':1.1332': no such name from org.freedesktop.DBus
/// Got message: Method call AddMatch from :1.918
/// Got message: Method return from org.freedesktop.DBus
/// ```
///
/// [Monitor]: https://dbus.freedesktop.org/doc/dbus-specification.html#bus-messages-become-monitor
#[derive(Debug)]
pub struct Connection<S>(Arc<ConnectionCommon<Async<S>>>);

impl<S> Connection<S>
where
    S: AsRawFd + std::fmt::Debug + Unpin + Socket,
    Async<S>: Socket,
{
    /// Create and open a D-Bus connection from a `UnixStream`.
    ///
    /// The connection may either be set up for a *bus* connection, or not (for peer-to-peer
    /// communications).
    ///
    /// Upon successful return, the connection is fully established and negotiated: D-Bus messages
    /// can be sent and received.
    pub async fn new_client(stream: S, bus_connection: bool) -> Result<Self> {
        // SASL Handshake
        let auth = Authenticated::client(Async::new(stream)?).await?;

        if bus_connection {
            Connection::new_authenticated_bus(auth).await
        } else {
            Ok(Connection::new_authenticated(auth))
        }
    }

    /// Create a server `Connection` for the given `UnixStream` and the server `guid`.
    ///
    /// The connection will wait for incoming client authentication handshake & negotiation messages,
    /// for peer-to-peer communications.
    ///
    /// Upon successful return, the connection is fully established and negotiated: D-Bus messages
    /// can be sent and received.
    pub async fn new_server(stream: S, guid: &Guid) -> Result<Self> {
        use nix::sys::socket::{getsockopt, sockopt::PeerCredentials};

        // FIXME: Could and should this be async?
        let creds = getsockopt(stream.as_raw_fd(), PeerCredentials)
            .map_err(|e| Error::Handshake(format!("Failed to get peer credentials: {}", e)))?;

        let auth = Authenticated::server(Async::new(stream)?, guid.clone(), creds.uid()).await?;

        Ok(Self::new_authenticated(auth))
    }

    /// Create a `Connection` from an already authenticated unix socket.
    ///
    /// This method can be used in conjunction with [`crate::azync::Authenticated`] to handle
    /// the initial handshake of the D-Bus connection asynchronously.
    ///
    /// If the aim is to initialize a client *bus* connection, you need to send the client hello and assign
    /// the resulting unique name using [`set_unique_name`] before doing anything else.
    ///
    /// [`set_unique_name`]: struct.Connection.html#method.set_unique_name
    pub fn new_authenticated(auth: Authenticated<Async<S>>) -> Self {
        Self(Arc::new(ConnectionCommon::new_authenticated(
            auth.into_inner(),
        )))
    }

    /// Send `msg` to the peer.
    ///
    /// Unlike [`Sink`] implementation, this method sets a unique (to this connection) serial
    /// number on the message before sending it off, for you.
    ///
    /// On successfully sending off `msg`, the assigned serial number is returned.
    pub async fn send_message(&self, mut msg: Message) -> Result<u32> {
        let serial = self.assign_serial_num(&mut msg)?;

        (&mut &*self).send(msg).await?;

        Ok(serial)
    }

    /// Send a method call.
    ///
    /// Create a method-call message, send it over the connection, then wait for the reply.
    ///
    /// On succesful reply, an `Ok(Message)` is returned. On error, an `Err` is returned. D-Bus
    /// error replies are returned as [`Error::MethodError`].
    pub async fn call_method<B>(
        &self,
        destination: Option<&str>,
        path: &str,
        iface: Option<&str>,
        method_name: &str,
        body: &B,
    ) -> Result<Message>
    where
        B: serde::ser::Serialize + zvariant::Type,
    {
        let m = Message::method(
            self.unique_name(),
            destination,
            path,
            iface,
            method_name,
            body,
        )?;
        let serial = self.send_message(m).await?;

        let mut tmp_queue = vec![];

        while let Some(m) = (&mut &*self).try_next().await? {
            let h = m.header()?;

            if h.reply_serial()? != Some(serial) {
                let queue = self.0.in_queue_lock();
                if queue.len() + tmp_queue.len() < self.max_queued() {
                    // We first push to a temporary queue as otherwise it'll create an infinite loop
                    // since subsequent `receive_message` call will pick up the message from the main
                    // queue.
                    tmp_queue.push(m);
                }

                continue;
            } else {
                self.0.in_queue_lock().append(&mut tmp_queue);
            }

            match h.message_type()? {
                MessageType::Error => return Err(m.into()),
                MessageType::MethodReturn => return Ok(m),
                _ => (),
            }
        }

        // If Stream gives us None, that means the socket was closed
        Err(Error::Io(io::Error::new(
            ErrorKind::BrokenPipe,
            "socket closed",
        )))
    }

    /// Emit a signal.
    ///
    /// Create a signal message, and send it over the connection.
    pub async fn emit_signal<B>(
        &self,
        destination: Option<&str>,
        path: &str,
        iface: &str,
        signal_name: &str,
        body: &B,
    ) -> Result<()>
    where
        B: serde::ser::Serialize + zvariant::Type,
    {
        let m = Message::signal(
            self.unique_name(),
            destination,
            path,
            iface,
            signal_name,
            body,
        )?;

        self.send_message(m).await.map(|_| ())
    }

    /// Reply to a message.
    ///
    /// Given an existing message (likely a method call), send a reply back to the caller with the
    /// given `body`.
    ///
    /// Returns the message serial number.
    pub async fn reply<B>(&self, call: &Message, body: &B) -> Result<u32>
    where
        B: serde::ser::Serialize + zvariant::Type,
    {
        let m = Message::method_reply(self.unique_name(), call, body)?;
        self.send_message(m).await
    }

    /// Reply an error to a message.
    ///
    /// Given an existing message (likely a method call), send an error reply back to the caller
    /// with the given `error_name` and `body`.
    ///
    /// Returns the message serial number.
    pub async fn reply_error<B>(&self, call: &Message, error_name: &str, body: &B) -> Result<u32>
    where
        B: serde::ser::Serialize + zvariant::Type,
    {
        let m = Message::method_error(self.unique_name(), call, error_name, body)?;
        self.send_message(m).await
    }

    /// Sets the unique name for this connection.
    ///
    /// This method should only be used when initializing a client *bus* connection with
    /// [`Connection::new_authenticated`]. Setting the unique name to anything other than the return
    /// value of the bus hello is a protocol violation.
    ///
    /// Returns and error if the name has already been set.
    pub fn set_unique_name(self, name: String) -> std::result::Result<Self, String> {
        self.0.set_unique_name(name).map(|_| self)
    }

    /// Assigns a serial number to `msg` that is unique to this connection.
    ///
    /// This method can fail if `msg` is corrupt.
    pub fn assign_serial_num(&self, msg: &mut Message) -> Result<u32> {
        let serial = self.next_serial();
        msg.modify_primary_header(|primary| {
            primary.set_serial_num(serial);

            Ok(())
        })?;

        Ok(serial)
    }

    /// The unique name as assigned by the message bus or `None` if not a message bus connection.
    pub fn unique_name(&self) -> Option<&str> {
        self.0.unique_name()
    }

    /// Max number of messages to queue.
    pub fn max_queued(&self) -> usize {
        self.0.max_queued()
    }

    /// Set the max number of messages to queue.
    ///
    /// Since typically you'd want to set this at instantiation time, this method takes ownership
    /// of `self` and returns an owned `Connection` instance so you can use the builder pattern to
    /// set the value.
    ///
    /// # Example
    ///
    /// ```
    ///# use std::error::Error;
    ///# use zbus::azync::Connection;
    /// use futures::executor::block_on;
    ///
    /// let conn = block_on(Connection::new_session())?.set_max_queued(30);
    /// assert_eq!(conn.max_queued(), 30);
    ///
    /// // Do something usefull with `conn`..
    ///# Ok::<_, Box<dyn Error + Send + Sync>>(())
    /// ```
    pub fn set_max_queued(self, max: usize) -> Self {
        self.0.set_max_queued(max);

        self
    }

    /// The server's GUID.
    pub fn server_guid(&self) -> &str {
        self.0.server_guid()
    }

    async fn new_authenticated_bus(auth: Authenticated<Async<S>>) -> Result<Self> {
        let connection = Connection::new_authenticated(auth);

        // Now that the server has approved us, we must send the bus Hello, as per specs
        // TODO: Use fdo module once it's async.
        let name: String = connection
            .call_method(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                Some("org.freedesktop.DBus"),
                "Hello",
                &(),
            )
            .await?
            .body()?;

        Ok(connection
            .set_unique_name(name)
            // programmer (probably our) error if this fails.
            .expect("Attempted to set unique_name twice"))
    }

    fn next_serial(&self) -> u32 {
        self.0.next_serial()
    }
}

impl Connection<UnixStream> {
    /// Create a `Connection` to the session/user message bus.
    pub async fn new_session() -> Result<Self> {
        Self::new_authenticated_bus(Authenticated::session().await?).await
    }

    /// Create a `Connection` to the system-wide message bus.
    pub async fn new_system() -> Result<Self> {
        Self::new_authenticated_bus(Authenticated::system().await?).await
    }

    /// Create a `Connection` for the given [D-Bus address].
    ///
    /// [D-Bus address]: https://dbus.freedesktop.org/doc/dbus-specification.html#addresses
    pub async fn new_for_address(address: &str, bus_connection: bool) -> Result<Self> {
        let auth = Authenticated::for_address(address).await?;

        if bus_connection {
            Connection::new_authenticated_bus(auth).await
        } else {
            Ok(Connection::new_authenticated(auth))
        }
    }
}

impl<S> Sink<Message> for Connection<S>
where
    S: Socket,
    Async<S>: Socket,
{
    type Error = Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        Pin::new(&mut &*self).poll_ready(cx)
    }

    fn start_send(self: Pin<&mut Self>, msg: Message) -> Result<()> {
        Pin::new(&mut &*self).start_send(msg)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        Pin::new(&mut &*self).poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        Pin::new(&mut &*self).poll_close(cx)
    }
}

impl<S> Sink<Message> for &Connection<S>
where
    S: Socket,
    Async<S>: Socket,
{
    type Error = Error;

    fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<()>> {
        // TODO: We should have a max queue length in raw::Socket for outgoing messages.
        Poll::Ready(Ok(()))
    }

    fn start_send(self: Pin<&mut Self>, msg: Message) -> Result<()> {
        if !msg.fds().is_empty() && !self.0.cap_unix_fd() {
            return Err(Error::Unsupported);
        }

        let mut conn = self.0.raw_conn_write();
        conn.enqueue_message(msg);

        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        let mut raw_conn = self.0.raw_conn_write();

        loop {
            match raw_conn.try_flush() {
                Ok(()) => return Poll::Ready(Ok(())),
                Err(e) => {
                    if e.kind() == ErrorKind::WouldBlock {
                        let poll = raw_conn.socket().poll_writable(cx);

                        match poll {
                            Poll::Pending => return Poll::Pending,
                            // Guess socket became ready already so let's try it again.
                            Poll::Ready(Ok(_)) => continue,
                            Poll::Ready(Err(e)) => return Poll::Ready(Err(e.into())),
                        }
                    } else {
                        return Poll::Ready(Err(Error::Io(e)));
                    }
                }
            }
        }
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        let raw_conn = self.0.raw_conn_read();

        match self.poll_flush(cx) {
            Poll::Ready(Ok(_)) => (),
            Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
            Poll::Pending => return Poll::Pending,
        }

        Poll::Ready((*raw_conn).close())
    }
}

impl<S> Stream for Connection<S>
where
    S: Socket,
    Async<S>: Socket,
{
    type Item = Result<Message>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut &*self).poll_next(cx)
    }
}

impl<S> Stream for &Connection<S>
where
    S: Socket,
    Async<S>: Socket,
{
    type Item = Result<Message>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut queue = self.0.in_queue_lock();
        if let Some(msg) = queue.pop() {
            return Poll::Ready(Some(Ok(msg)));
        }

        let mut raw_conn = self.0.raw_conn_write();
        loop {
            match raw_conn.try_receive_message() {
                Ok(m) => return Poll::Ready(Some(Ok(m))),
                Err(Error::Io(e)) if e.kind() == ErrorKind::WouldBlock => {
                    let poll = raw_conn.socket().poll_readable(cx);

                    match poll {
                        Poll::Pending => return Poll::Pending,
                        // Guess socket became ready already so let's try it again.
                        Poll::Ready(Ok(_)) => continue,
                        Poll::Ready(Err(e)) => return Poll::Ready(Some(Err(e.into()))),
                    }
                }
                Err(Error::Io(e)) if e.kind() == ErrorKind::BrokenPipe => return Poll::Ready(None),
                Err(e) => return Poll::Ready(Some(Err(e))),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixStream;

    use super::*;

    #[test]
    fn unix_p2p() {
        futures::executor::block_on(test_unix_p2p()).unwrap();
    }

    async fn test_unix_p2p() -> Result<()> {
        let guid = Guid::generate();

        let (p0, p1) = UnixStream::pair().unwrap();

        let server = Connection::new_server(p0, &guid);
        let client = Connection::new_client(p1, false);

        let (client_conn, mut server_conn) = futures::try_join!(client, server)?;

        let server_future = async {
            let m1 = server_conn.try_next().await?.unwrap();
            let m2 = server_conn.try_next().await?.unwrap();

            // Reply in the opposite order to the client calls to test the client side queue.
            if m1.to_string() == "Method call Test1" {
                server_conn.reply(&m2, &("nay")).await?;
                server_conn.reply(&m1, &("yay")).await
            } else {
                server_conn.reply(&m1, &("yay")).await?;
                server_conn.reply(&m2, &("nay")).await
            }
        };

        let client_future1 = async {
            let reply = client_conn
                .call_method(None, "/", Some("org.zbus.p2p"), "Test1", &())
                .await?;
            assert_eq!(reply.to_string(), "Method return");
            reply.body::<String>().map_err(|e| e.into())
        };
        let client_future2 = async {
            let reply = client_conn
                .call_method(None, "/", Some("org.zbus.p2p"), "Test2", &())
                .await?;
            assert_eq!(reply.to_string(), "Method return");
            reply.body::<String>().map_err(|e| e.into())
        };

        let (val, _, _) = futures::try_join!(client_future1, client_future2, server_future)?;
        assert_eq!(val, "yay");

        Ok(())
    }
}