//! Allocation-free preflight of the native SocialMedia CBOR subset.
//!
//! Existing serde SocialMedia emits a definite map with `m` text and `d` an
//! integer array, not a byte string. Accept either key order. Unknown fields,
//! nonminimal/indefinite encoding, tags and alternative representations are
//! unsupported. No recursive skip or general-purpose deserializer is used.

use rostra_core::avatar::MAX_AVATAR_BYTES;

use super::{MAX_NATIVE_MEDIA_BODY_BYTES, OpaqueMediaRead};

/// Complete shape validation precedes the only owned media allocation.
pub(super) fn decode(body: &[u8], image_only: bool) -> Result<(&str, Vec<u8>), OpaqueMediaRead> {
    let (mime, bytes, len) = if image_only {
        preflight(body)?
    } else {
        preflight_opaque(body)?
    };
    let mut input = Input { bytes };
    let mut data = Vec::with_capacity(len);
    for _ in 0..len {
        data.push(input.byte()?);
    }
    Ok((mime, data))
}

/// Validate the complete CBOR subset while borrowing encoded image bytes.
pub(super) fn preflight(body: &[u8]) -> Result<(&'static str, &[u8], usize), OpaqueMediaRead> {
    let (mime, bytes, len) = preflight_opaque(body)?;
    let mime = match mime {
        "image/png" => "image/png",
        "image/webp" => "image/webp",
        _ => return Err(OpaqueMediaRead::Unsupported),
    };
    Ok((mime, bytes, len))
}

/// Validate structure without treating the declared MIME as a content policy.
fn preflight_opaque(body: &[u8]) -> Result<(&str, &[u8], usize), OpaqueMediaRead> {
    if body.len() > MAX_NATIVE_MEDIA_BODY_BYTES {
        return Err(OpaqueMediaRead::TooLarge);
    }
    let mut input = Input { bytes: body };
    let (major, count) = input.head()?;
    if major != 5 || count != 2 {
        return Err(OpaqueMediaRead::Unsupported);
    }
    let mut mime = None;
    let mut data = None;
    for _ in 0..2 {
        let key = input.text(1)?;
        match key {
            "m" => {
                if mime.is_some() {
                    return Err(OpaqueMediaRead::Invalid);
                }
                mime = Some(input.text(64)?);
            }
            "d" => {
                if data.is_some() {
                    return Err(OpaqueMediaRead::Invalid);
                }
                let (major, len) = input.head()?;
                if major != 4 {
                    return Err(OpaqueMediaRead::Unsupported);
                }
                if len > MAX_AVATAR_BYTES as u64 {
                    return Err(OpaqueMediaRead::TooLarge);
                }
                let start = input.bytes;
                for _ in 0..len {
                    input.byte()?;
                }
                data = Some((&start[..start.len() - input.bytes.len()], len as usize));
            }
            _ => return Err(OpaqueMediaRead::Unsupported),
        }
    }
    if !input.bytes.is_empty() {
        return Err(OpaqueMediaRead::Invalid);
    }
    let mime = mime.ok_or(OpaqueMediaRead::Invalid)?;
    let (bytes, len) = data.ok_or(OpaqueMediaRead::Invalid)?;
    Ok((mime, bytes, len))
}

/// Copy at most thirty decoded bytes for unvalidated dimension hints.
pub(super) fn prefix(bytes: &[u8], len: usize) -> Result<[u8; 30], OpaqueMediaRead> {
    let mut result = [0; 30];
    let mut input = Input { bytes };
    for byte in result.iter_mut().take(len) {
        *byte = input.byte()?;
    }
    Ok(result)
}
/// A forward-only cursor, with no recursive descent or owned strings.
struct Input<'a> {
    /// Remaining borrowed bytes within the admitted body.
    bytes: &'a [u8],
}

impl<'a> Input<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], OpaqueMediaRead> {
        let (value, rest) = self
            .bytes
            .split_at_checked(len)
            .ok_or(OpaqueMediaRead::Invalid)?;
        self.bytes = rest;
        Ok(value)
    }

    fn head(&mut self) -> Result<(u8, u64), OpaqueMediaRead> {
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
            28..=30 => return Err(OpaqueMediaRead::Invalid),
            _ => return Err(OpaqueMediaRead::Unsupported),
        };
        if value < minimum {
            return Err(OpaqueMediaRead::Unsupported);
        }
        Ok((byte >> 5, value))
    }

    fn text(&mut self, cap: usize) -> Result<&'a str, OpaqueMediaRead> {
        let (major, len) = self.head()?;
        if major != 3 || len > cap as u64 {
            return Err(OpaqueMediaRead::Unsupported);
        }
        std::str::from_utf8(self.take(len as usize)?).map_err(|_| OpaqueMediaRead::Invalid)
    }

    fn byte(&mut self) -> Result<u8, OpaqueMediaRead> {
        let (major, value) = self.head()?;
        if major != 0 || value > 255 {
            return Err(OpaqueMediaRead::Unsupported);
        }
        Ok(value as u8)
    }
}
