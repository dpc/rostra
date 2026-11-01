use jotup::{Container, Event};

use super::{
    DeferredKind, LinkOrigin, ListNumbering, ListStyle, MAX_NODES, MAX_TABLE_CELLS, MAX_TABLE_ROWS,
    SemanticAlignment, SemanticDocument, SemanticKind, SemanticListKind, SemanticMark,
    SemanticNode,
};
use crate::native::{
    MAX_DEPTH, MAX_EVENTS, MAX_IMAGES, MAX_OUTPUT_BYTES, MAX_SPANS, NativeImageDestination,
    NativeLink, NativeRenderError, NativeWarning, renderer,
};

/// An open parser container; transparent containers still consume depth.
#[derive(Default)]
struct Frame {
    /// None for parser-only wrappers and suppressed navigation.
    kind: Option<SemanticKind>,
    /// Completed children in source order.
    children: Vec<SemanticNode>,
    /// Definitions are not visible or interactive.
    hidden: bool,
    /// Literal and image-alternative contexts suppress descendant actions.
    inert: bool,
}

/// Whole-document accounting, including metadata and empty containers.
#[derive(Default)]
struct Builder {
    /// Visible, deduplicated notices.
    warnings: Vec<NativeWarning>,
    /// Aggregate owned text and destination bytes.
    bytes: usize,
    /// All emitted nodes, including empty containers.
    nodes: usize,
    /// Text leaves, bounded independently of containers.
    spans: usize,
    /// Image records across the tree.
    images: usize,
    /// Aggregate table rows.
    rows: usize,
    /// Aggregate table cells.
    cells: usize,
}

impl Builder {
    fn warn(&mut self, warning: NativeWarning) {
        if !self.warnings.contains(&warning) {
            self.warnings.push(warning);
        }
    }

    fn bytes(&mut self, bytes: usize) -> Result<(), NativeRenderError> {
        self.bytes += bytes;
        if self.bytes > MAX_OUTPUT_BYTES {
            return Err(NativeRenderError::OutputLimit);
        }
        Ok(())
    }

    fn owned(&mut self, text: &str) -> Result<String, NativeRenderError> {
        self.bytes(text.len())?;
        Ok(text.to_owned())
    }

    fn node(&mut self) -> Result<(), NativeRenderError> {
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            return Err(NativeRenderError::NodeLimit);
        }
        Ok(())
    }

    fn text(&mut self, frame: &mut Frame, text: &str) -> Result<(), NativeRenderError> {
        if frame.hidden || text.is_empty() {
            return Ok(());
        }
        self.bytes(text.len())?;
        if let Some(SemanticNode {
            kind: SemanticKind::Text { text: previous },
            ..
        }) = frame.children.last_mut()
        {
            previous.push_str(text);
        } else {
            self.node()?;
            self.spans += 1;
            if self.spans > MAX_SPANS {
                return Err(NativeRenderError::SpanLimit);
            }
            frame.children.push(SemanticNode {
                kind: SemanticKind::Text {
                    text: text.to_owned(),
                },
                children: Vec::new(),
            });
        }
        Ok(())
    }

    fn leaf(&mut self, frame: &mut Frame, kind: SemanticKind) -> Result<(), NativeRenderError> {
        if !frame.hidden {
            self.node()?;
            frame.children.push(SemanticNode {
                kind,
                children: Vec::new(),
            });
        }
        Ok(())
    }

    fn deferred(&mut self, feature: DeferredKind) -> SemanticKind {
        self.warn(NativeWarning::UnsupportedMarkup);
        SemanticKind::Deferred { feature }
    }

    fn start(
        &mut self,
        container: Container<'_>,
        parent: &Frame,
    ) -> Result<Frame, NativeRenderError> {
        let mut frame = Frame {
            hidden: parent.hidden || matches!(container, Container::LinkDefinition { .. }),
            inert: parent.inert,
            ..Frame::default()
        };
        if frame.hidden {
            return Ok(frame);
        }
        let block = container.is_block();
        let kind = match container {
            Container::Document | Container::Section { .. } | Container::Span => None,
            Container::LinkDefinition { .. } => unreachable!("handled above"),
            Container::Paragraph => Some(SemanticKind::Paragraph),
            Container::Heading { level, .. } => Some(SemanticKind::Heading { level }),
            Container::Blockquote => Some(SemanticKind::Quote),
            Container::List { kind, tight } => Some(SemanticKind::List {
                list_kind: list_kind(kind),
                tight,
            }),
            Container::ListItem => Some(SemanticKind::ListItem { checked: None }),
            Container::TaskListItem { checked } => Some(SemanticKind::ListItem {
                checked: Some(checked),
            }),
            Container::CodeBlock { language } => {
                frame.inert = true;
                Some(SemanticKind::CodeBlock {
                    language: self.owned(&language)?,
                })
            }
            Container::RawBlock { format } | Container::RawInline { format } => {
                frame.inert = true;
                self.warn(NativeWarning::RawMarkup);
                Some(SemanticKind::Raw {
                    block,
                    format: self.owned(&format)?,
                })
            }
            Container::Table => Some(SemanticKind::Table),
            Container::TableRow { head } => {
                self.rows += 1;
                if self.rows > MAX_TABLE_ROWS {
                    return Err(NativeRenderError::TableLimit);
                }
                Some(SemanticKind::TableRow { head })
            }
            Container::TableCell { alignment, head } => {
                self.cells += 1;
                if self.cells > MAX_TABLE_CELLS {
                    return Err(NativeRenderError::TableLimit);
                }
                Some(SemanticKind::TableCell {
                    head,
                    alignment: match alignment {
                        jotup::Alignment::Unspecified => SemanticAlignment::Unspecified,
                        jotup::Alignment::Left => SemanticAlignment::Left,
                        jotup::Alignment::Center => SemanticAlignment::Center,
                        jotup::Alignment::Right => SemanticAlignment::Right,
                    },
                })
            }
            Container::Caption => Some(SemanticKind::Caption),
            Container::Strong => Some(mark(SemanticMark::Strong)),
            Container::Emphasis => Some(mark(SemanticMark::Emphasis)),
            Container::Verbatim => {
                frame.inert = true;
                Some(mark(SemanticMark::Code))
            }
            Container::Mark => Some(mark(SemanticMark::Highlight)),
            Container::Insert => Some(mark(SemanticMark::Insert)),
            Container::Delete => Some(mark(SemanticMark::Delete)),
            Container::Subscript => Some(mark(SemanticMark::Subscript)),
            Container::Superscript => Some(mark(SemanticMark::Superscript)),
            Container::Link(destination, kind) if !frame.inert => {
                let destination =
                    if matches!(kind, jotup::LinkType::Span(jotup::SpanLinkType::Unresolved)) {
                        None
                    } else {
                        renderer::link(&destination)
                    };
                self.bytes(match &destination {
                    Some(NativeLink::External { url }) => url.len(),
                    Some(NativeLink::Profile { .. }) => 128,
                    Some(NativeLink::Post { .. }) => 256,
                    None => 0,
                })?;
                if destination.is_none() {
                    self.warn(NativeWarning::InvalidLink);
                }
                Some(SemanticKind::Link {
                    destination,
                    origin: match kind {
                        jotup::LinkType::AutoLink => LinkOrigin::Autolink,
                        jotup::LinkType::Email => LinkOrigin::Email,
                        jotup::LinkType::Span(_) => LinkOrigin::Authored,
                    },
                })
            }
            Container::Link(..) => None,
            Container::Image(destination, kind) if !frame.inert => {
                self.images += 1;
                if self.images > MAX_IMAGES {
                    return Err(NativeRenderError::ImageLimit);
                }
                let destination = if matches!(kind, jotup::SpanLinkType::Unresolved) {
                    NativeImageDestination::Invalid
                } else {
                    renderer::image_destination(&destination)
                };
                self.bytes(match &destination {
                    NativeImageDestination::External { url } => url.len(),
                    NativeImageDestination::Rostra { .. } => 128,
                    NativeImageDestination::Invalid => 0,
                })?;
                if destination == NativeImageDestination::Invalid {
                    self.warn(NativeWarning::InvalidLink);
                }
                frame.inert = true;
                Some(SemanticKind::Image { destination })
            }
            Container::Image(..) => {
                self.warn(NativeWarning::MediaOmitted);
                None
            }
            Container::Math { display } => {
                frame.inert = true;
                Some(self.deferred(DeferredKind::Math { display }))
            }
            Container::Footnote { label } => {
                frame.inert = true;
                let label = self.owned(&label)?;
                Some(self.deferred(DeferredKind::FootnoteDefinition { label }))
            }
            Container::DescriptionList => Some(self.deferred(DeferredKind::DescriptionList)),
            Container::DescriptionTerm => Some(self.deferred(DeferredKind::DescriptionTerm)),
            Container::DescriptionDetails => Some(self.deferred(DeferredKind::DescriptionDetails)),
            Container::Div { .. } => Some(self.deferred(DeferredKind::Division)),
        };
        if kind.is_some() {
            self.node()?;
        }
        frame.kind = kind;
        Ok(frame)
    }
}

fn mark(mark: SemanticMark) -> SemanticKind {
    SemanticKind::Mark { mark }
}

fn list_kind(kind: jotup::ListKind) -> SemanticListKind {
    match kind {
        jotup::ListKind::Unordered(_) => SemanticListKind::Unordered,
        jotup::ListKind::Task(_) => SemanticListKind::Task,
        jotup::ListKind::Ordered {
            numbering,
            style,
            start,
        } => SemanticListKind::Ordered {
            start,
            numbering: match numbering {
                jotup::OrderedListNumbering::Decimal => ListNumbering::Decimal,
                jotup::OrderedListNumbering::AlphaLower => ListNumbering::AlphaLower,
                jotup::OrderedListNumbering::AlphaUpper => ListNumbering::AlphaUpper,
                jotup::OrderedListNumbering::RomanLower => ListNumbering::RomanLower,
                jotup::OrderedListNumbering::RomanUpper => ListNumbering::RomanUpper,
            },
            style: match style {
                jotup::OrderedListStyle::Period => ListStyle::Period,
                jotup::OrderedListStyle::Paren => ListStyle::Paren,
                jotup::OrderedListStyle::ParenParen => ListStyle::ParenParen,
            },
        },
    }
}

pub(super) fn render(source: &str) -> Result<SemanticDocument, NativeRenderError> {
    let mut builder = Builder::default();
    let mut frame = Frame::default();
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
                    builder.warn(NativeWarning::AttributesIgnored);
                }
                let next = builder.start(container, &frame)?;
                stack.push(frame);
                frame = next;
            }
            Event::End => {
                let mut parent = stack.pop().expect("balanced parser containers");
                if let Some(kind) = frame.kind {
                    parent.children.push(SemanticNode {
                        kind,
                        children: frame.children,
                    });
                } else {
                    parent.children.extend(frame.children);
                }
                frame = parent;
            }
            Event::Str(text) => builder.text(&mut frame, &text)?,
            Event::Softbreak => builder.leaf(&mut frame, SemanticKind::SoftBreak)?,
            Event::Hardbreak => builder.leaf(&mut frame, SemanticKind::HardBreak)?,
            Event::NonBreakingSpace => builder.text(&mut frame, "\u{a0}")?,
            Event::LeftSingleQuote => builder.text(&mut frame, "‘")?,
            Event::RightSingleQuote => builder.text(&mut frame, "’")?,
            Event::LeftDoubleQuote => builder.text(&mut frame, "“")?,
            Event::RightDoubleQuote => builder.text(&mut frame, "”")?,
            Event::Ellipsis => builder.text(&mut frame, "…")?,
            Event::EnDash => builder.text(&mut frame, "–")?,
            Event::EmDash => builder.text(&mut frame, "—")?,
            Event::Symbol(text) => {
                builder.text(&mut frame, ":")?;
                builder.text(&mut frame, &text)?;
                builder.text(&mut frame, ":")?;
            }
            Event::FootnoteReference(label) if !frame.hidden => {
                let label = builder.owned(&label)?;
                let kind = builder.deferred(DeferredKind::FootnoteReference { label });
                builder.leaf(&mut frame, kind)?;
            }
            Event::ThematicBreak(attributes) => {
                if !attributes.is_empty() {
                    builder.warn(NativeWarning::AttributesIgnored);
                }
                builder.leaf(&mut frame, SemanticKind::Rule)?;
            }
            Event::Attributes(_) => builder.warn(NativeWarning::AttributesIgnored),
            Event::FootnoteReference(_) | Event::Blankline | Event::Escape => {}
        }
    }
    Ok(SemanticDocument {
        version: 1,
        nodes: frame.children,
        warnings: builder.warnings,
    })
}
