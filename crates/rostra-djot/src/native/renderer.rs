use jotup::{Container, Event};

use super::{
    MAX_DEPTH, MAX_EVENTS, MAX_LINK_BYTES, MAX_OUTPUT_BYTES, MAX_SPANS, NativeDocument, NativeLink,
    NativeRenderError, NativeSpan, NativeWarning,
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
            last.strong == template.strong
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
}

pub(super) fn render(source: &str) -> Result<NativeDocument, NativeRenderError> {
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
                    Container::Image(..) => {
                        output.warn(NativeWarning::MediaOmitted);
                        state.span.link = None;
                        state.inert = true;
                        output.text("[image] ", &state)?;
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

fn link(destination: &str) -> Option<NativeLink> {
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
