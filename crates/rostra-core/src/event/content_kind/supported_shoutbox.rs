//! Bounded native presentation policy, separate from permissive wire ingestion.

use cbor4ii::core::dec::Read as _;
use serde::Deserialize;

use super::Shoutbox;

/// Flat known fields prevent recursive unknown-value parsing.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SupportedShoutbox {
    /// Exact historical body, without normalization or local composer
    /// validation.
    c: String,
}

impl Shoutbox {
    /// Decode a known flat map of at most 1 MB, rejecting trailing bytes.
    ///
    /// Unknown fields and non-map forms are unsupported by this native policy,
    /// not invalid signed protocol data. Historical whitespace is preserved.
    pub fn decode_supported(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > 1_000_000 || bytes.first()? >> 5 != 5 {
            return None;
        }
        let mut decoder =
            cbor4ii::serde::Deserializer::new(cbor4ii::core::utils::SliceReader::new(bytes));
        let body = SupportedShoutbox::deserialize(&mut decoder).ok()?;
        let mut reader = decoder.into_inner();
        let remaining = match reader.fill(1).ok()? {
            cbor4ii::core::dec::Reference::Long(bytes)
            | cbor4ii::core::dec::Reference::Short(bytes) => !bytes.is_empty(),
        };
        (!remaining).then_some(Self {
            djot_content: body.c,
        })
    }
}
