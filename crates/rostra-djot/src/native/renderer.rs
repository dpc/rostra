use jotup::{Container, Event};

use super::{
    MAX_DEPTH, MAX_EVENTS, MAX_IMAGES, MAX_LINK_BYTES, MAX_OUTPUT_BYTES, MAX_SPANS, NativeDocument,
    NativeImage, NativeImageDestination, NativeLink, NativeRenderError, NativeSpan, NativeWarning,
};
use crate::links::extract_rostra_id_link_reference;

/// Current inherited presentation and navigation suppression.
#[derive(Clone, Default)]
struct State {
    /// Formatting flags and link; text is always empty in this template.
    span: NativeSpan,
    /// Literal code and image alternatives never contain interactive links.
    inert: bool,
    /// Link definitions are not visible content.
    hidden: bool,
    /// Alternative text currently being collected for an opt-in image record.
    image: Option<usize>,
}

/// Bounded output builder with deferred separators to avoid trailing newlines.
#[derive(Default)]
struct Builder {
    /// Returned document under construction.
    document: NativeDocument,
    /// Aggregate owned text and destinations, including duplicated
    /// destinations.
    bytes: usize,
    /// A block boundary waiting for the next nonempty text.
    separator: bool,
    /// A visible block prefix waiting for its first content.
    prefix: bool,
}

impl Builder {
    fn warn(&mut self, warning: NativeWarning) {
        if !self.document.warnings.contains(&warning) {
            self.document.warnings.push(warning);
        }
    }

    fn text(&mut self, text: &str, state: &State) -> Result<(), NativeRenderError> {
        if text.is_empty() || state.hidden {
            return Ok(());
        }
        if let Some(index) = state.image {
            self.bytes += text.len();
            if self.bytes > MAX_OUTPUT_BYTES {
                return Err(NativeRenderError::OutputLimit);
            }
            self.document.images[index].alt.push_str(text);
            return Ok(());
        }
        self.prefix = false;
        if std::mem::take(&mut self.separator) && !self.document.spans.is_empty() {
            self.push("\n", &NativeSpan::default())?;
        }
        self.push(text, &state.span)
    }

    fn prefix(&mut self, text: &str, state: &State) -> Result<(), NativeRenderError> {
        self.text(text, state)?;
        self.prefix = true;
        Ok(())
    }

    fn push(&mut self, text: &str, template: &NativeSpan) -> Result<(), NativeRenderError> {
        let merge = self.document.spans.last().is_some_and(|last| {
            !self
                .document
                .images
                .last()
                .is_some_and(|image| image.span_index == self.document.spans.len())
                && last.strong == template.strong
                && last.emphasis == template.emphasis
                && last.code == template.code
                && last.link == template.link
        });
        let destination_bytes = if merge {
            0
        } else {
            match &template.link {
                Some(NativeLink::External { url }) => url.len(),
                // A conservative bound on either canonical identity encoding.
                Some(NativeLink::Profile { .. }) => 128,
                // Two components and their serialized full/short tags.
                Some(NativeLink::Post { .. }) => 256,
                None => 0,
            }
        };
        let bytes = self.bytes + text.len() + destination_bytes;
        if bytes > MAX_OUTPUT_BYTES {
            return Err(NativeRenderError::OutputLimit);
        }
        if merge {
            self.document
                .spans
                .last_mut()
                .expect("checked")
                .text
                .push_str(text);
        } else {
            if self.document.spans.len() == MAX_SPANS {
                return Err(NativeRenderError::SpanLimit);
            }
            let mut span = template.clone();
            span.text = text.to_owned();
            self.document.spans.push(span);
        }
        self.bytes = bytes;
        Ok(())
    }

    fn image(&mut self, destination: NativeImageDestination) -> Result<usize, NativeRenderError> {
        if self.document.images.len() == MAX_IMAGES {
            return Err(NativeRenderError::ImageLimit);
        }
        if std::mem::take(&mut self.separator) && !self.document.spans.is_empty() {
            self.push("\n", &NativeSpan::default())?;
        }
        self.prefix = false;
        self.bytes += match &destination {
            NativeImageDestination::Rostra { .. } => 128,
            NativeImageDestination::External { url } => url.len(),
            NativeImageDestination::Invalid => 0,
        };
        if self.bytes > MAX_OUTPUT_BYTES {
            return Err(NativeRenderError::OutputLimit);
        }
        let index = self.document.images.len();
        self.document.images.push(NativeImage {
            span_index: self.document.spans.len(),
            alt: String::new(),
            destination,
        });
        Ok(index)
    }
}

pub(super) fn render(source: &str, media: bool) -> Result<NativeDocument, NativeRenderError> {
    let mut output = Builder::default();
    let mut state = State::default();
    let mut stack = Vec::new();
    for (index, event) in jotup::Parser::new(source).enumerate() {
        if index == MAX_EVENTS {
            return Err(NativeRenderError::EventLimit);
        }
        match event {
            Event::Start(container, attributes) => {
                if stack.len() == MAX_DEPTH {
                    return Err(NativeRenderError::DepthLimit);
                }
                if !attributes.is_empty() {
                    output.warn(NativeWarning::AttributesIgnored);
                }
                let block = container.is_block();
                stack.push((state.clone(), block));
                if block && !state.hidden && !output.prefix {
                    output.separator = true;
                }
                match container {
                    Container::Strong => state.span.strong = true,
                    Container::Emphasis => state.span.emphasis = true,
                    Container::Verbatim | Container::CodeBlock { .. } => {
                        state.span.code = true;
                        state.span.link = None;
                        state.inert = true;
                    }
                    Container::RawBlock { .. } | Container::RawInline { .. } => {
                        output.warn(NativeWarning::RawMarkup);
                        state.span.code = true;
                        state.span.link = None;
                        state.inert = true;
                    }
                    Container::Link(destination, kind) => {
                        state.span.link = None;
                        if !state.inert {
                            if !matches!(
                                kind,
                                jotup::LinkType::Span(jotup::SpanLinkType::Unresolved)
                            ) {
                                state.span.link = link(&destination);
                            }
                            if state.span.link.is_none() {
                                output.warn(NativeWarning::InvalidLink);
                            }
                        }
                    }
                    Container::Image(destination, kind) => {
                        state.span.link = None;
                        if media && !state.hidden && state.image.is_none() && !state.inert {
                            let destination = if matches!(kind, jotup::SpanLinkType::Unresolved) {
                                NativeImageDestination::Invalid
                            } else {
                                image_destination(&destination)
                            };
                            if destination == NativeImageDestination::Invalid {
                                output.warn(NativeWarning::InvalidLink);
                            }
                            state.image = Some(output.image(destination)?);
                        } else {
                            output.warn(NativeWarning::MediaOmitted);
                            output.text("[image] ", &state)?;
                        }
                        state.inert = true;
                    }
                    Container::LinkDefinition { .. } => state.hidden = true,
                    Container::Blockquote => output.prefix("> ", &state)?,
                    Container::ListItem => output.prefix("• ", &state)?,
                    Container::TaskListItem { checked } => {
                        output.prefix(if checked { "[x] " } else { "[ ] " }, &state)?;
                    }
                    Container::Heading { level, .. } => {
                        if level != 1 {
                            output.warn(NativeWarning::UnsupportedMarkup);
                        }
                        output.prefix("# ", &state)?;
                    }
                    Container::List {
                        kind: jotup::ListKind::Ordered { .. },
                        ..
                    } => output.warn(NativeWarning::UnsupportedMarkup),
                    Container::Paragraph
                    | Container::Section { .. }
                    | Container::Document
                    | Container::List { .. } => {}
                    _ => output.warn(NativeWarning::UnsupportedMarkup),
                }
            }
            Event::End => {
                if let Some((previous, block)) = stack.pop() {
                    if block && !state.hidden {
                        output.separator = true;
                    }
                    state = previous;
                }
            }
            Event::Str(text) => output.text(&text, &state)?,
            Event::Softbreak => output.text(" ", &state)?,
            Event::Hardbreak => output.text("\n", &state)?,
            Event::NonBreakingSpace => output.text("\u{a0}", &state)?,
            Event::LeftSingleQuote => output.text("‘", &state)?,
            Event::RightSingleQuote => output.text("’", &state)?,
            Event::LeftDoubleQuote => output.text("“", &state)?,
            Event::RightDoubleQuote => output.text("”", &state)?,
            Event::Ellipsis => output.text("…", &state)?,
            Event::EnDash => output.text("–", &state)?,
            Event::EmDash => output.text("—", &state)?,
            Event::Symbol(text) => {
                output.text(":", &state)?;
                output.text(&text, &state)?;
                output.text(":", &state)?;
            }
            Event::FootnoteReference(text) => {
                output.warn(NativeWarning::UnsupportedMarkup);
                output.text("[^", &state)?;
                output.text(&text, &state)?;
                output.text("]", &state)?;
            }
            Event::ThematicBreak(attributes) => {
                if !attributes.is_empty() {
                    output.warn(NativeWarning::AttributesIgnored);
                }
                output.separator = true;
                output.text("—", &state)?;
                output.separator = true;
            }
            Event::Attributes(_) => output.warn(NativeWarning::AttributesIgnored),
            Event::Blankline | Event::Escape => {}
        }
    }
    Ok(output.document)
}

pub(super) fn image_destination(destination: &str) -> NativeImageDestination {
    if destination.len() <= MAX_LINK_BYTES
        && let Some(reference) = crate::links::extract_rostra_media_reference(destination)
    {
        return NativeImageDestination::Rostra { reference };
    }
    if let Some(NativeLink::External { url }) = link(destination) {
        NativeImageDestination::External { url }
    } else {
        NativeImageDestination::Invalid
    }
}

pub(super) fn link(destination: &str) -> Option<NativeLink> {
    if destination.len() > MAX_LINK_BYTES
        || destination
            .chars()
            .any(|c| c.is_control() || c.is_whitespace())
        || destination.contains('\\')
    {
        return None;
    }
    if let Some(reference) = extract_rostra_id_link_reference(destination) {
        return Some(NativeLink::Profile { reference });
    }
    if let Ok(reference) = destination.parse() {
        return Some(NativeLink::Post { reference });
    }
    // Require an explicit authority, not URL-parser recovery of `https:foo`.
    let (scheme, _) = destination.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("https") && !scheme.eq_ignore_ascii_case("http") {
        return None;
    }
    let url = url::Url::parse(destination).ok()?;
    if url.host_str().is_none() || !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    let url = url.to_string();
    (url.len() <= MAX_LINK_BYTES).then_some(NativeLink::External { url })
}
