//! Bounded local tag discovery, deliberately narrower than wire ingestion.
//!
//! Definite-length, known-field maps only. Unknown/duplicate fields, CBOR tags,
//! indefinite containers, excessive nesting and invalid/noncanonical labels are
//! unsupported, not evidence that a post has no tags. This policy does not
//! change ingestion or the effective legacy fallback.

use std::collections::BTreeSet;

use super::SocialPost;
use crate::event::PersonaTag;

/// Maximum complete CBOR payload admitted to discovery.
pub const PAYLOAD_BYTES_MAX: usize = 64 * 1024;
/// Maximum CBOR items visited before ordinary decoding, including map keys.
pub const ITEMS_MAX: usize = 1024;
/// Maximum declared tag elements, including duplicates.
pub const TAG_ELEMENTS_MAX: usize = 256;
/// Maximum container nesting admitted before ordinary decoding.
pub const DEPTH_MAX: usize = 8;

/// Why a retained post cannot supply labels to bounded discovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TagDiscoveryIssue {
    /// Payload bytes exceed the local discovery policy.
    Oversized,
    /// Unsupported shape, excessive work, malformed data or invalid labels.
    Unsupported,
}

/// Decode effective tags after allocation-free shape and work checks.
///
/// The full payload is checked before constructing any strings or collections.
/// Declared tag strings must be canonical `PersonaTag::new` values, at most 32
/// UTF-8 bytes, without trimming or splitting. Empty declared tags retain the
/// ordinary legacy fallback; absent/unmapped legacy personas remain empty.
/// No partial set is returned on failure. Limits bound input and work, not RSS
/// or a CPU deadline.
pub fn decode(bytes: &[u8]) -> Result<BTreeSet<PersonaTag>, TagDiscoveryIssue> {
    if bytes.len() > PAYLOAD_BYTES_MAX {
        return Err(TagDiscoveryIssue::Oversized);
    }
    preflight(bytes).ok_or(TagDiscoveryIssue::Unsupported)?;
    let post: SocialPost =
        cbor4ii::serde::from_slice(bytes).map_err(|_| TagDiscoveryIssue::Unsupported)?;
    let tags = post.persona_tags();
    for tag in &tags {
        if !PersonaTag::new(tag.as_str()).is_ok_and(|valid| valid == *tag) {
            return Err(TagDiscoveryIssue::Unsupported);
        }
    }
    Ok(tags)
}

/// Allocation-free cursor with a monotonically decreasing item allowance.
struct Input<'a> {
    /// Unconsumed encoded input.
    bytes: &'a [u8],
    /// Remaining CBOR items, charged before processing.
    remaining: usize,
}

impl<'a> Input<'a> {
    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let (value, rest) = self.bytes.split_at_checked(len)?;
        self.bytes = rest;
        Some(value)
    }

    fn head(&mut self) -> Option<(u8, u64)> {
        self.remaining = self.remaining.checked_sub(1)?;
        let byte = *self.take(1)?.first()?;
        let additional = byte & 31;
        let value = match additional {
            0..=23 => u64::from(additional),
            24..=27 => {
                let mut value = 0u64;
                for byte in self.take(1 << (additional - 24))? {
                    value = (value << 8) | u64::from(*byte);
                }
                value
            }
            _ => return None,
        };
        Some((byte >> 5, value))
    }

    fn text(&mut self) -> Option<&'a str> {
        let (major, len) = self.head()?;
        if major != 3 {
            return None;
        }
        std::str::from_utf8(self.take(usize::try_from(len).ok()?)?).ok()
    }

    fn value(&mut self, depth: usize) -> Option<()> {
        if depth > DEPTH_MAX {
            return None;
        }
        let (major, value) = self.head()?;
        match major {
            0 | 1 => {}
            2 | 3 => {
                let bytes = self.take(usize::try_from(value).ok()?)?;
                if major == 3 {
                    std::str::from_utf8(bytes).ok()?;
                }
            }
            4 | 5 => {
                let count = value.checked_mul(if major == 5 { 2 } else { 1 })?;
                if count > self.remaining as u64 {
                    return None;
                }
                for _ in 0..count {
                    self.value(depth + 1)?;
                }
            }
            7 if matches!(value, 20..=22) => {}
            _ => return None,
        }
        Some(())
    }
}

fn preflight(bytes: &[u8]) -> Option<()> {
    let mut input = Input {
        bytes,
        remaining: ITEMS_MAX,
    };
    let (major, fields) = input.head()?;
    if major != 5 || fields > 8 {
        return None;
    }
    let mut seen = 0u16;
    for _ in 0..fields {
        let field = input.text()?;
        let index = ["p", "c", "r", "e", "u", "h", "n", "t"]
            .iter()
            .position(|known| *known == field)?;
        let bit = 1 << index;
        if seen & bit != 0 {
            return None;
        }
        seen |= bit;
        if field == "t" {
            let (major, count) = input.head()?;
            if major != 4 || count > TAG_ELEMENTS_MAX as u64 {
                return None;
            }
            for _ in 0..count {
                let tag = input.text()?;
                if tag.is_empty() || tag.len() > 32 {
                    return None;
                }
            }
        } else {
            input.value(1)?;
        }
    }
    input.bytes.is_empty().then_some(())
}
