//! Noninteractive source image records; these confer no retrieval authority.

use rostra_core::media::MediaEventReference;
use serde::Serialize;

/// An image positioned between literal text spans.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NativeImage {
    /// Insert before this span, or at the end when equal to the span count.
    pub span_index: usize,
    /// Literal alternative text, never markup or authority.
    pub alt: String,
    /// Syntactic destination only; no bytes have been read or verified.
    pub destination: NativeImageDestination,
}

/// Image destinations are deliberately separate from ordinary navigation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NativeImageDestination {
    /// Source-author-scoped signed media requiring a checked retained read.
    Rostra {
        /// Preserve full versus short identity rather than shortening on parse.
        #[serde(serialize_with = "serialize_reference")]
        reference: MediaEventReference,
    },
    /// Inert external image; disclose its destination but never fetch it.
    External {
        /// Canonical credential-free HTTP(S) destination.
        url: String,
    },
    /// Unsupported, unresolved, or malformed destination; no navigation.
    Invalid,
}

fn serialize_reference<S: serde::Serializer>(
    reference: &MediaEventReference,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    // Tagged full/short forms prevent consumers from treating prefixes as IDs.
    use serde::ser::SerializeStruct as _;
    let mut value = serializer.serialize_struct("MediaEventReference", 2)?;
    let (kind, id) = match reference {
        MediaEventReference::Full(id) => ("full", id.to_string()),
        MediaEventReference::Short(id) => ("short", id.to_string()),
    };
    value.serialize_field("kind", kind)?;
    value.serialize_field("id", &id)?;
    value.end()
}
