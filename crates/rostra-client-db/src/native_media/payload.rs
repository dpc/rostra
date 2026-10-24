//! Allocation-free preflight of the native SocialMedia CBOR subset.
//!
//! Existing serde SocialMedia emits a definite map with `m` text and `d` an
//! integer array, not a byte string. Accept either key order. Unknown fields,
//! nonminimal/indefinite encoding, tags and alternative representations are
//! unsupported. No recursive skip or general-purpose deserializer is used.

use rostra_core::avatar::MAX_AVATAR_BYTES;

use super::{MAX_NATIVE_MEDIA_BODY_BYTES, NativeMediaRead};

/// Complete shape validation precedes the only owned media allocation.
pub(super) fn decode(body: &[u8]) -> Result<(&'static str, Vec<u8>), NativeMediaRead> {
    if body.len() > MAX_NATIVE_MEDIA_BODY_BYTES {
        return Err(NativeMediaRead::TooLarge);
    }
    let mut input = Input { bytes: body };
    let (major, count) = input.head()?;
    if major != 5 || count != 2 {
        return Err(NativeMediaRead::Unsupported);
    }
    let mut mime = None;
    let mut data = None;
    for _ in 0..2 {
        let key = input.text(1)?;
        match key {
            "m" => {
                if mime.is_some() {
                    return Err(NativeMediaRead::Invalid);
                }
                mime = Some(input.text(64)?);
            }
            "d" => {
                if data.is_some() {
                    return Err(NativeMediaRead::Invalid);
                }
                let (major, len) = input.head()?;
                if major != 4 {
                    return Err(NativeMediaRead::Unsupported);
                }
                if len > MAX_AVATAR_BYTES as u64 {
                    return Err(NativeMediaRead::TooLarge);
                }
                let start = input.bytes;
                for _ in 0..len {
                    input.byte()?;
                }
                data = Some((&start[..start.len() - input.bytes.len()], len as usize));
            }
            _ => return Err(NativeMediaRead::Unsupported),
        }
    }
    if !input.bytes.is_empty() {
        return Err(NativeMediaRead::Invalid);
    }
    let mime = match mime.ok_or(NativeMediaRead::Invalid)? {
        "image/png" => "image/png",
        "image/webp" => "image/webp",
        _ => return Err(NativeMediaRead::Unsupported),
    };
    let (bytes, len) = data.ok_or(NativeMediaRead::Invalid)?;
    let mut input = Input { bytes };
    let mut data = Vec::with_capacity(len);
    for _ in 0..len {
        data.push(input.byte()?);
    }
    Ok((mime, data))
}

/// A forward-only cursor, with no recursive descent or owned strings.
struct Input<'a> {
    /// Remaining borrowed bytes within the admitted body.
    bytes: &'a [u8],
}

impl<'a> Input<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], NativeMediaRead> {
        let (value, rest) = self
            .bytes
            .split_at_checked(len)
            .ok_or(NativeMediaRead::Invalid)?;
        self.bytes = rest;
        Ok(value)
    }

    fn head(&mut self) -> Result<(u8, u64), NativeMediaRead> {
        let byte = self.take(1)?[0];
        let additional = byte & 31;
        let (value, minimum) = match additional {
            0..=23 => (u64::from(additional), 0),
            24 => (u64::from(self.take(1)?[0]), 24),
            25 => (
                u64::from(u16::from_be_bytes(
                    self.take(2)?.try_into().expect("length"),
                )),
                256,
            ),
            26 => (
                u64::from(u32::from_be_bytes(
                    self.take(4)?.try_into().expect("length"),
                )),
                65_536,
            ),
            27 => (
                u64::from_be_bytes(self.take(8)?.try_into().expect("length")),
                1 << 32,
            ),
            28..=30 => return Err(NativeMediaRead::Invalid),
            _ => return Err(NativeMediaRead::Unsupported),
        };
        if value < minimum {
            return Err(NativeMediaRead::Unsupported);
        }
        Ok((byte >> 5, value))
    }

    fn text(&mut self, cap: usize) -> Result<&'a str, NativeMediaRead> {
        let (major, len) = self.head()?;
        if major != 3 || len > cap as u64 {
            return Err(NativeMediaRead::Unsupported);
        }
        std::str::from_utf8(self.take(len as usize)?).map_err(|_| NativeMediaRead::Invalid)
    }

    fn byte(&mut self) -> Result<u8, NativeMediaRead> {
        let (major, value) = self.head()?;
        if major != 0 || value > 255 {
            return Err(NativeMediaRead::Unsupported);
        }
        Ok(value as u8)
    }
}
