use snakecase::ascii::to_snakecase;
use std::fmt::Display;
use std::fmt::Formatter;

use zbus::xml::{Arg, Interface};

pub struct GenTrait<'i>(pub &'i Interface);

impl<'i> Display for GenTrait<'i> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let iface = self.0;
        let idx = iface.name().rfind('.').unwrap() + 1;
        let name = &iface.name()[idx..];

        writeln!(f, "#[dbus_proxy(interface = \"{}\")]", iface.name())?;
        writeln!(f, "trait {} {{", name)?;

        let mut methods = iface.methods().to_vec();
        methods.sort_by(|a, b| a.name().partial_cmp(b.name()).unwrap());
        for m in &methods {
            let (inputs, output) = inputs_output_from_args(&m.args());
            writeln!(f)?;
            writeln!(f, "    /// {} method", m.name())?;
            writeln!(
                f,
                "    fn {name}({inputs}){output};",
                name = to_snakecase(m.name()),
                inputs = inputs,
                output = output
            )?;
        }

        let mut props = iface.properties().to_vec();
        props.sort_by(|a, b| a.name().partial_cmp(b.name()).unwrap());
        for p in props {
            let (read, write) = read_write_from_access(p.access());

            writeln!(f)?;
            writeln!(f, "    /// {} property", p.name())?;

            if read {
                let output = to_rust_type(p.ty(), false);
                writeln!(f, "    #[dbus_proxy(property)]")?;
                writeln!(
                    f,
                    "    fn {name}(&self) -> zbus::Result<{output}>;",
                    name = to_snakecase(p.name()),
                    output = output,
                )?;
            }

            if write {
                let input = to_rust_type(p.ty(), true);
                writeln!(f, "    #[DBusProxy(property)]")?;
                writeln!(
                    f,
                    "    fn set_{name}(&self, value: {input}) -> zbus::Result<()>;",
                    name = to_snakecase(p.name()),
                    input = input,
                )?;
            }
        }
        writeln!(f, "}}")
    }
}

fn read_write_from_access(access: &str) -> (bool, bool) {
    match access {
        "read" => (true, false),
        "write" => (false, true),
        "readwrite" => (true, true),
        _ => panic!(),
    }
}

fn inputs_output_from_args(args: &[&Arg]) -> (String, String) {
    let mut inputs = vec!["&self".to_string()];
    let mut output = vec![];
    let mut n = 0;
    let mut gen_name = || {
        n += 1;
        format!("arg_{}", n)
    };

    for a in args {
        match a.direction().as_deref() {
            Some("in") => {
                let ty = to_rust_type(a.ty(), true);
                let arg = if let Some(name) = a.name() {
                    name.into()
                } else {
                    gen_name()
                };
                inputs.push(format!("{}: {}", arg, ty));
            }
            Some("out") => {
                let ty = to_rust_type(a.ty(), false);
                output.push(ty);
            }
            _ => unimplemented!(),
        }
    }

    let output = match output.len() {
        0 => "()".to_string(),
        1 => output[0].to_string(),
        _ => format!("({})", output.join(", ")),
    };

    (inputs.join(", "), format!(" -> zbus::Result<{}>", output))
}

fn to_rust_type(ty: &str, input: bool) -> String {
    // can't haz recursive closure, yet
    fn iter_to_rust_type(
        it: &mut std::iter::Peekable<std::slice::Iter<u8>>,
        input: bool,
        as_ref: bool,
    ) -> String {
        let c = it.next().unwrap();
        match *c as char {
            'y' => "u8".into(),
            'b' => "bool".into(),
            'n' => "i16".into(),
            'q' => "u16".into(),
            'i' => "i32".into(),
            'u' => "u32".into(),
            'x' => "i64".into(),
            't' => "u64".into(),
            'd' => "f64".into(),
            'h' => "std::os::unix::io::RawFd".into(),
            's' | 'o' | 'g' => (if input || as_ref { "&str" } else { "String" }).into(),
            'v' => (if input {
                if as_ref {
                    "&zvariant::Value"
                } else {
                    "zvariant::Value"
                }
            } else {
                "zvariant::OwnedValue"
            })
            .into(),
            'a' => {
                let c = it.peek().unwrap();
                match **c as char {
                    '{' => format!(
                        "std::collections::HashMap<{}>",
                        iter_to_rust_type(it, input, false)
                    ),
                    _ => {
                        let ty = iter_to_rust_type(it, input, false);
                        if input {
                            format!("{}[{}]", if as_ref { "&" } else { "" }, ty)
                        } else {
                            format!("{}Vec<{}>", if as_ref { "&" } else { "" }, ty)
                        }
                    }
                }
            }
            c @ '(' | c @ '{' => {
                let dict = c == '{';
                let mut vec = vec![];
                loop {
                    let c = it.peek().unwrap();
                    match **c as char {
                        ')' | '}' => break,
                        _ => vec.push(iter_to_rust_type(it, input, false)),
                    }
                }
                if dict {
                    vec.join(", ")
                } else if vec.len() > 1 {
                    format!("{}({})", if as_ref { "&" } else { "" }, vec.join(", "))
                } else {
                    vec[0].to_string()
                }
            }
            _ => unimplemented!(),
        }
    }

    let mut it = ty.as_bytes().iter().peekable();
    iter_to_rust_type(&mut it, input, input)
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::result::Result;

    use super::GenTrait;
    use zbus::xml::Node;

    static EXAMPLE: &str = r##"
<!DOCTYPE node PUBLIC "-//freedesktop//DTD D-BUS Object Introspection 1.0//EN"
  "http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd">
 <node name="/com/example/sample_object0">
   <interface name="com.example.SampleInterface0">
     <method name="Frobate">
       <arg name="foo" type="i" direction="in"/>
       <arg name="bar" type="s" direction="out"/>
       <arg name="baz" type="a{us}" direction="out"/>
       <annotation name="org.freedesktop.DBus.Deprecated" value="true"/>
     </method>
     <method name="Bazify">
       <arg name="bar" type="(iiu)" direction="in"/>
       <arg name="bar" type="v" direction="out"/>
     </method>
     <method name="MogrifyMe">
       <arg name="bar" type="(iiav)" direction="in"/>
     </method>
     <signal name="Changed">
       <arg name="new_value" type="b"/>
     </signal>
     <property name="Bar" type="y" access="readwrite"/>
   </interface>
   <node name="child_of_sample_object"/>
   <node name="another_child_of_sample_object"/>
</node>
"##;

    #[test]
    fn gen() -> Result<(), Box<dyn Error>> {
        let node = Node::from_reader(EXAMPLE.as_bytes())?;
        let t = format!("{}", GenTrait(&node.interfaces()[0]));
        println!("{}", t);
        Ok(())
    }
}
