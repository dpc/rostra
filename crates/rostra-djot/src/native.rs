//! Bounded, non-HTML presentation for native hosts.
//!
//! This is a deliberately lossy Djot subset, independent of the web renderer.
//! Hosts must display warnings, and show source (without link annotations) when
//! rendering fails. No partial document is returned. Run off the UI executor
//! and bound concurrency: these limits are not a CPU deadline or an RSS
//! ceiling. Short identity references require the host's retained identity
//! index; an unknown short reference must remain inert. A link label is never
//! authority: disclose the actual destination before user-controlled external
//! navigation.

use std::fmt;

use rostra_core::post::PostReference;
use serde::Serialize;

use crate::links::RostraIdLink;

mod image;
mod renderer;
pub use image::{NativeImage, NativeImageDestination};
#[cfg(test)]
mod tests;

/// Maximum source size, checked before constructing the eager Djot parser.
pub const MAX_INPUT_BYTES: usize = 64 * 1024;
/// Maximum potential recursive block openers across the entire source.
pub const MAX_BLOCK_OPENERS: usize = 64;
/// Maximum nesting in the emitted event stream.
pub const MAX_DEPTH: usize = 64;
/// Maximum consumed parser events.
pub const MAX_EVENTS: usize = 65_536;
/// Maximum returned text segments.
pub const MAX_SPANS: usize = 4096;
/// Maximum aggregate owned text and repeated link destination bytes.
pub const MAX_OUTPUT_BYTES: usize = 128 * 1024;
/// Maximum accepted link destination length in UTF-8 bytes.
pub const MAX_LINK_BYTES: usize = 2048;
/// Maximum image records in opt-in media presentation.
pub const MAX_IMAGES: usize = 32;

/// A native presentation containing literal text and optional inert image
/// references, never HTML or fetch instructions.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct NativeDocument {
    /// Ordered segments; separators are ordinary newline text, not offsets.
    pub spans: Vec<NativeSpan>,
    /// Ordered image anchors, empty unless media presentation was requested.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<NativeImage>,
    /// Distinct lossy-conversion notices for visible host presentation.
    pub warnings: Vec<NativeWarning>,
}

/// An owned segment avoids UTF-8 versus UTF-16 offset ambiguity at FFI
/// boundaries.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct NativeSpan {
    /// Literal text; never interpret this as markup.
    pub text: String,
    /// Strong emphasis.
    pub strong: bool,
    /// Ordinary emphasis.
    pub emphasis: bool,
    /// Literal/code presentation.
    pub code: bool,
    /// Validated destination, distinct from the untrusted displayed text.
    pub link: Option<NativeLink>,
}

/// Syntactic navigation classes, never retrieval or signing authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NativeLink {
    /// An exact root-relative post path requiring retained DB resolution.
    Post {
        /// Preserve the requested components for destination disclosure.
        #[serde(serialize_with = "serialize_post_reference")]
        reference: PostReference,
    },
    /// A syntactically valid identity, not proof of an author or known
    /// identity.
    Profile {
        /// Full identity or unresolved short prefix, serialized as canonical
        /// text.
        #[serde(serialize_with = "serialize_reference")]
        reference: RostraIdLink,
    },
    /// A canonical, credential-free HTTP(S) URL; never fetch automatically.
    External {
        /// Actual destination to disclose, including host and path.
        url: String,
    },
}

fn serialize_post_reference<S: serde::Serializer>(
    reference: &PostReference,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    use rostra_core::post::{PostAuthorReference, PostEventReference};
    use serde::ser::SerializeStruct as _;

    #[derive(Serialize)]
    struct Component {
        /// Exact full or short form.
        kind: &'static str,
        /// Canonical component bytes.
        id: String,
    }
    let mut value = serializer.serialize_struct("PostReference", 2)?;
    value.serialize_field(
        "author",
        &Component {
            kind: match reference.author {
                PostAuthorReference::Full(_) => "full",
                PostAuthorReference::Short(_) => "short",
            },
            id: reference.author.to_string(),
        },
    )?;
    value.serialize_field(
        "event",
        &Component {
            kind: match reference.event {
                PostEventReference::Full(_) => "full",
                PostEventReference::Short(_) => "short",
            },
            id: reference.event.to_string(),
        },
    )?;
    value.end()
}

fn serialize_reference<S: serde::Serializer>(
    reference: &RostraIdLink,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match reference {
        RostraIdLink::Full(id) => serializer.collect_str(id),
        RostraIdLink::Short(id) => serializer.collect_str(id),
    }
}

/// Visible notices for intentionally reduced presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeWarning {
    /// A construct was flattened rather than faithfully formatted.
    UnsupportedMarkup,
    /// Image/media content was replaced with literal alternative text.
    MediaOmitted,
    /// Raw markup was rendered as inert code text.
    RawMarkup,
    /// An unsupported or malformed destination was left noninteractive.
    InvalidLink,
    /// User-supplied attributes were discarded.
    AttributesIgnored,
}

/// A whole-document failure; hosts may offer explicitly labelled source-only
/// text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeRenderError {
    /// Input exceeds the byte budget.
    InputLimit,
    /// Conservative pre-parser recursive-block complexity budget exceeded.
    ComplexityLimit,
    /// Parsed container depth exceeds the budget.
    DepthLimit,
    /// Parser event count exceeds the budget.
    EventLimit,
    /// Output segment count exceeds the budget.
    SpanLimit,
    /// Owned output bytes exceed the budget.
    OutputLimit,
    /// Image record count exceeds the opt-in media budget.
    ImageLimit,
}

impl fmt::Display for NativeRenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "native Djot rendering limit: {self:?}")
    }
}

impl std::error::Error for NativeRenderError {}

/// Render the safe native subset, rejecting limits without silently truncating.
///
/// The pre-parser guard conservatively counts potential recursive block
/// openers, including siblings and apparent openers inside literal code. This
/// is not a claim to accept every document of nesting depth 64. Ordinary inline
/// formatting and URL punctuation do not consume that budget. See the module
/// documentation for host obligations; successful rendering does not authorize
/// navigation.
pub fn render(source: &str) -> Result<NativeDocument, NativeRenderError> {
    crate::parse_guard::check(source)?;
    renderer::render(source, false)
}

/// Render image references and alternative text without reading any media.
///
/// Images are inserted before their span index, in record order, including
/// empty alternatives. They are not navigation links. Only a checked local
/// reader, bound to the actual source post author, can authorize presentation.
/// External images remain inert. All ordinary parser and output guards apply.
pub fn render_with_media(source: &str) -> Result<NativeDocument, NativeRenderError> {
    crate::parse_guard::check(source)?;
    renderer::render(source, true)
}
