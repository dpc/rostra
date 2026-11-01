//! Bounded semantic presentation, independent of HTML and host authorization.

use serde::Serialize;

use super::{NativeImageDestination, NativeLink, NativeRenderError, NativeWarning};

mod builder;
#[cfg(test)]
mod tests;

/// Maximum nodes across the complete semantic tree, including text leaves.
pub const MAX_NODES: usize = 8192;
/// Maximum table rows across the complete document.
pub const MAX_TABLE_ROWS: usize = 256;
/// Maximum table cells across the complete document.
pub const MAX_TABLE_CELLS: usize = 1024;

/// An ordered semantic tree containing no HTML, fetched data or authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SemanticDocument {
    /// Wire shape version; consumers must reject unsupported versions
    /// atomically.
    pub version: u32,
    /// Ordered top-level blocks; parser-only sections are transparent.
    pub nodes: Vec<SemanticNode>,
    /// Distinct notices that hosts must present visibly.
    pub warnings: Vec<NativeWarning>,
}

/// An owned semantic node; child order is source presentation order.
///
/// Block containers contain blocks, text-bearing blocks contain inlines, and
/// inline containers contain inlines. Lists contain items; tables contain rows
/// and an optional caption, and rows contain cells. Leaves have no children.
/// Code, raw and math containers contain literal text only. Image children are
/// inert alternative text (possibly marked), never links or nested media
/// actions. Hosts must stop inherited link actions at code, raw, math and image
/// boundaries, even when those nodes occur inside a link's authored label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SemanticNode {
    /// Node semantics, flattened into the JSON object beside `children`.
    #[serde(flatten)]
    pub kind: SemanticKind,
    /// Ordered children, empty for text, breaks, rules and references.
    pub children: Vec<SemanticNode>,
}

/// Semantic node tags; source attributes and generated HTML IDs are not copied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SemanticKind {
    /// A paragraph, including paragraphs inside tight list items.
    Paragraph,
    /// A heading retaining the parser's exact level, not a literal prefix.
    Heading {
        /// Djot heading level; hosts should cap visual size, not trust it as an
        /// index.
        level: u16,
    },
    /// A quotation containing its full block structure.
    Quote,
    /// A list retaining ordering and tight/loose layout.
    List {
        /// Marker semantics, independent of child position.
        list_kind: SemanticListKind,
        /// Whether the source list uses compact paragraph spacing.
        tight: bool,
    },
    /// A list item with an optional read-only task checkbox.
    ListItem {
        /// None for ordinary items; never an editable published control.
        checked: Option<bool>,
    },
    /// Literal fenced code; no highlighter is invoked.
    CodeBlock {
        /// Authored language hint, not an executable or CSS class.
        language: String,
    },
    /// A thematic separator.
    Rule,
    /// A table with rows/cells and an optional caption.
    Table,
    /// A table row, retaining header semantics.
    TableRow {
        /// Whether this is a header row.
        head: bool,
    },
    /// A table cell containing inlines.
    TableCell {
        /// Whether this is a header cell.
        head: bool,
        /// Authored alignment, without arbitrary style attributes.
        alignment: SemanticAlignment,
    },
    /// A table caption containing inlines.
    Caption,
    /// An inline formatting container.
    Mark {
        /// The formatting applied to the children.
        mark: SemanticMark,
    },
    /// A link retaining authored labels and explicit autolink origin.
    Link {
        /// Validated syntactic destination; None means visibly inert.
        destination: Option<NativeLink>,
        /// Hosts may enrich autolinks, but must preserve authored labels.
        origin: LinkOrigin,
    },
    /// An inert image reference at its inline source position.
    Image {
        /// No retrieval authority; bind retained media to the signed source
        /// author.
        destination: NativeImageDestination,
    },
    /// Literal owned text, never markup.
    Text {
        /// Text in UTF-8; hosts derive any platform-specific offsets
        /// themselves.
        text: String,
    },
    /// A soft line break, normally presented as a wrapping space.
    SoftBreak,
    /// An explicit line break.
    HardBreak,
    /// Raw source rendered as literal code, never executed.
    Raw {
        /// Whether the source construct was a block.
        block: bool,
        /// Authored raw format, only a display hint.
        format: String,
    },
    /// Unsupported rich presentation retained explicitly with a visible
    /// warning.
    Deferred {
        /// The construct whose readable children need a fallback presentation.
        feature: DeferredKind,
    },
}

/// List numbering independent of parser implementation types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ListNumbering {
    /// Decimal digits.
    Decimal,
    /// Lowercase letters.
    AlphaLower,
    /// Uppercase letters.
    AlphaUpper,
    /// Lowercase Roman numerals.
    RomanLower,
    /// Uppercase Roman numerals.
    RomanUpper,
}

/// Punctuation surrounding ordered list markers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ListStyle {
    /// A trailing period.
    Period,
    /// A trailing parenthesis.
    Paren,
    /// Parentheses on both sides.
    ParenParen,
}

/// List marker metadata; counts are not truncated to platform integer widths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SemanticListKind {
    /// Ordinary bullet list.
    Unordered,
    /// Read-only task list.
    Task,
    /// Ordered list with its original start, numbering and delimiter.
    Ordered {
        /// First marker value, including zero.
        start: u64,
        /// Numeric or alphabetic marker system.
        numbering: ListNumbering,
        /// Marker punctuation.
        style: ListStyle,
    },
}

/// Safe table alignment, not arbitrary CSS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticAlignment {
    /// No authored alignment.
    Unspecified,
    /// Left alignment.
    Left,
    /// Center alignment.
    Center,
    /// Right alignment.
    Right,
}

/// Supported inline marks; nesting preserves combinations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticMark {
    /// Strong emphasis.
    Strong,
    /// Ordinary emphasis.
    Emphasis,
    /// Literal inline code.
    Code,
    /// Highlighted text.
    Highlight,
    /// Inserted text.
    Insert,
    /// Deleted text.
    Delete,
    /// Subscript.
    Subscript,
    /// Superscript.
    Superscript,
}

/// Source link syntax, never inferred from the displayed label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkOrigin {
    /// Explicit inline or reference link with an authored label.
    Authored,
    /// An angle-bracket autolink, eligible for bounded local label enrichment.
    Autolink,
    /// Email autolink, currently inert under native destination policy.
    Email,
}

/// Explicit deferred features; hosts must not silently flatten these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeferredKind {
    /// Literal TeX source, without MathJax or remote rendering.
    Math {
        /// Whether the author requested display math.
        display: bool,
    },
    /// A footnote reference without navigation.
    FootnoteReference {
        /// Authored label, not a link destination.
        label: String,
    },
    /// A footnote definition kept at parser position, without relocation.
    FootnoteDefinition {
        /// Authored definition label.
        label: String,
    },
    /// Description-list structure without a mandated native layout.
    DescriptionList,
    /// A description term.
    DescriptionTerm,
    /// Description details.
    DescriptionDetails,
    /// A generic block division; authored classes are discarded.
    Division,
}

/// Render a bounded semantic tree atomically, preserving the legacy APIs.
///
/// Run off the UI executor with bounded concurrency. All source, parser, depth,
/// output, text-segment and media guards apply, plus node and aggregate table
/// limits. On any error discard the result and show labelled, noninteractive
/// source; no partial tree is returned. Image references are always inert,
/// including previews. Hosts retain stricter action validation, exact local
/// identity resolution, signed-author media binding and bounded name
/// enrichment.
pub fn render_semantic(source: &str) -> Result<SemanticDocument, NativeRenderError> {
    crate::parse_guard::check(source)?;
    builder::render(source)
}
