use byteorder::ByteOrder;
use std::str;

use crate::utils::padding_for_n_bytes;
use crate::{SimpleVariantType, VariantError, VariantType};

impl<'a> VariantType<'a> for &'a str {
    const SIGNATURE: char = 's';
    const SIGNATURE_STR: &'static str = "s";
    const ALIGNMENT: u32 = 4;

    fn encode(&self, n_bytes_before: usize) -> Vec<u8> {
        let len = self.len();
        let padding = padding_for_n_bytes(n_bytes_before as u32, Self::ALIGNMENT);
        let mut bytes = Vec::with_capacity(padding as usize + 5 + len);

        bytes.extend(std::iter::repeat(0).take(padding as usize));

        bytes.extend(&(len as u32).to_ne_bytes());
        bytes.extend(self.as_bytes());
        bytes.push(b'\0');

        bytes
    }

    fn extract_slice<'b>(bytes: &'b [u8], signature: &str) -> Result<&'b [u8], VariantError> {
        Self::ensure_correct_signature(signature)?;
        crate::ensure_sufficient_bytes(bytes, 4)?;

        let last_index = byteorder::NativeEndian::read_u32(bytes) as usize + 5;
        if bytes.len() < last_index {
            return Err(VariantError::InsufficientData);
        }

        Ok(&bytes[0..last_index])
    }

    fn decode(bytes: &'a [u8], signature: &str) -> Result<Self, VariantError> {
        Self::ensure_correct_signature(signature)?;
        crate::ensure_sufficient_bytes(bytes, 4)?;

        let last_index = bytes.len() - 1;
        str::from_utf8(&bytes[4..last_index]).map_err(|_| VariantError::InvalidUtf8)
    }
}
impl<'a> SimpleVariantType<'a> for &'a str {}

pub struct ObjectPath<'a>(&'a str);

impl<'a> ObjectPath<'a> {
    pub fn new(path: &'a str) -> Self {
        Self(path)
    }

    pub fn as_str(&self) -> &str {
        self.0
    }
}

// FIXME: Find a way to share code with &str implementation above
impl<'a> VariantType<'a> for ObjectPath<'a> {
    const SIGNATURE: char = 'o';
    const SIGNATURE_STR: &'static str = "o";
    const ALIGNMENT: u32 = 4;

    fn encode(&self, n_bytes_before: usize) -> Vec<u8> {
        self.0.encode(n_bytes_before)
    }

    fn extract_slice<'b>(bytes: &'b [u8], signature: &str) -> Result<&'b [u8], VariantError> {
        Self::ensure_correct_signature(signature)?;
        <(&str)>::extract_slice_simple(bytes)
    }

    fn decode(bytes: &'a [u8], signature: &str) -> Result<Self, VariantError> {
        Self::ensure_correct_signature(signature)?;
        <(&str)>::decode(bytes, <(&str)>::SIGNATURE_STR).map(|s| Self(s))
    }
}
impl<'a> SimpleVariantType<'a> for ObjectPath<'a> {}

pub struct Signature<'a>(&'a str);

impl<'a> Signature<'a> {
    pub fn new(signature: &'a str) -> Self {
        Self(signature)
    }

    pub fn as_str(&self) -> &str {
        self.0
    }
}

// FIXME: Find a way to share code with &str implementation in `variant_type.rs`
impl<'a> VariantType<'a> for Signature<'a> {
    const SIGNATURE: char = 'g';
    const SIGNATURE_STR: &'static str = "g";
    const ALIGNMENT: u32 = 1;

    // No padding needed because of 1-byte alignment and hence number of bytes before don't matter
    fn encode(&self, _n_bytes_before: usize) -> Vec<u8> {
        let len = self.0.len();
        let mut bytes = Vec::with_capacity(2 + len);

        bytes.push(len as u8);
        bytes.extend(self.0.as_bytes());
        bytes.push(b'\0');

        bytes
    }

    fn extract_slice<'b>(bytes: &'b [u8], signature: &str) -> Result<&'b [u8], VariantError> {
        Self::ensure_correct_signature(signature)?;
        if bytes.len() < 1 {
            return Err(VariantError::InsufficientData);
        }

        let last_index = bytes[0] as usize + 2;
        if bytes.len() < last_index {
            return Err(VariantError::InsufficientData);
        }

        Ok(&bytes[0..last_index])
    }

    fn decode(bytes: &'a [u8], signature: &str) -> Result<Self, VariantError> {
        Self::ensure_correct_signature(signature)?;
        if bytes.len() < 1 {
            return Err(VariantError::InsufficientData);
        }

        let last_index = bytes.len() - 1;
        str::from_utf8(&bytes[1..last_index])
            .map(|s| Self(s))
            .map_err(|_| VariantError::InvalidUtf8)
    }
}
impl<'a> SimpleVariantType<'a> for Signature<'a> {}
