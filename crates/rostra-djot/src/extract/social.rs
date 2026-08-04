use jotup::r#async::{AsyncRender, AsyncRenderOutput};
use jotup::html::filters::SanitizeExt as _;
use jotup::{Container, Event, Render, RenderOutput, RenderOutputExt as _};

/// Social metadata excerpts from a Djot document.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SocialExcerpt {
    /// Plain text of the first non-empty level-one or level-two heading.
    pub first_heading: Option<String>,
    /// Plain text from each non-empty top-level paragraph, in document order.
    pub paragraphs: Vec<String>,
}

/// Extract social metadata from Djot content.
pub fn extract_social_excerpt(djot_content: &str) -> SocialExcerpt {
    SocialExcerptRenderer::default()
        .sanitize()
        .render_into_document(djot_content)
        .expect("infallible")
}

#[derive(Debug, Clone, Copy)]
enum Capturing {
    Heading,
    Paragraph,
}

/// Extract a social metadata heading and normalized paragraph blocks.
#[derive(Debug, Default)]
pub struct SocialExcerptRenderer {
    excerpt: SocialExcerpt,
    /// The currently captured container, if any.
    capturing: Option<Capturing>,
    /// The nested inline-container depth within the captured container.
    depth: usize,
    /// Text accumulated from the captured container.
    buf: String,
}

impl<'s> Render<'s> for SocialExcerptRenderer {
    type Error = std::convert::Infallible;

    fn emit(&mut self, event: Event<'s>) -> Result<(), Self::Error> {
        match event {
            Event::Start(container, _attrs) => {
                if self.capturing.is_some() {
                    self.depth += 1;
                } else {
                    match container {
                        Container::Heading { level, .. }
                            if level <= 2 && self.excerpt.first_heading.is_none() =>
                        {
                            self.capturing = Some(Capturing::Heading);
                            self.depth = 0;
                            self.buf.clear();
                        }
                        Container::Paragraph => {
                            self.capturing = Some(Capturing::Paragraph);
                            self.depth = 0;
                            self.buf.clear();
                        }
                        _ => {}
                    }
                }
            }
            Event::End => {
                if let Some(capturing) = self.capturing {
                    if self.depth == 0 {
                        let text = self.buf.split_whitespace().collect::<Vec<_>>().join(" ");
                        if !text.is_empty() {
                            match capturing {
                                Capturing::Heading => self.excerpt.first_heading = Some(text),
                                Capturing::Paragraph => self.excerpt.paragraphs.push(text),
                            }
                        }
                        self.capturing = None;
                        self.buf.clear();
                    } else {
                        self.depth -= 1;
                    }
                }
            }
            Event::Str(text) if self.capturing.is_some() => self.buf.push_str(&text),
            Event::Softbreak | Event::Hardbreak | Event::NonBreakingSpace
                if self.capturing.is_some() =>
            {
                self.buf.push(' ');
            }
            Event::LeftSingleQuote if self.capturing.is_some() => self.buf.push('\u{2018}'),
            Event::RightSingleQuote if self.capturing.is_some() => self.buf.push('\u{2019}'),
            Event::LeftDoubleQuote if self.capturing.is_some() => self.buf.push('\u{201C}'),
            Event::RightDoubleQuote if self.capturing.is_some() => self.buf.push('\u{201D}'),
            Event::Ellipsis if self.capturing.is_some() => self.buf.push('\u{2026}'),
            Event::EnDash if self.capturing.is_some() => self.buf.push('\u{2013}'),
            Event::EmDash if self.capturing.is_some() => self.buf.push('\u{2014}'),
            Event::Symbol(symbol) if self.capturing.is_some() => {
                self.buf.push(':');
                self.buf.push_str(&symbol);
                self.buf.push(':');
            }
            _ => {}
        }

        Ok(())
    }
}

impl<'s> RenderOutput<'s> for SocialExcerptRenderer {
    type Output = SocialExcerpt;

    fn into_output(self) -> SocialExcerpt {
        self.excerpt
    }
}

#[async_trait::async_trait]
impl<'s> AsyncRender<'s> for SocialExcerptRenderer {
    type Error = std::convert::Infallible;

    async fn emit(&mut self, event: Event<'s>) -> Result<(), Self::Error> {
        Render::emit(self, event)
    }
}

#[async_trait::async_trait]
impl<'s> AsyncRenderOutput<'s> for SocialExcerptRenderer {
    type Output = SocialExcerpt;

    fn into_output(self) -> SocialExcerpt {
        self.excerpt
    }
}

#[cfg(test)]
mod tests;
