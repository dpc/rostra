//! Djot excerpt extraction for Open Graph meta tags and similar uses.

use jotup::r#async::{AsyncRender, AsyncRenderOutput};
use jotup::html::filters::SanitizeExt as _;
use jotup::{Container, Event, Render, RenderOutput, RenderOutputExt as _};

/// Extracted text excerpts from a djot document.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DjotExcerpt {
    /// Plain text of the first top-level heading, if any.
    pub first_heading: Option<String>,
    /// Plain text of the first top-level paragraph, if any.
    pub first_paragraph: Option<String>,
}

/// Parse djot content and extract the first heading and first paragraph as
/// plain text.
///
/// The event stream is sanitized (raw HTML converted to code blocks,
/// attributes stripped) before extraction for defense-in-depth.
pub fn extract_excerpt(djot_content: &str) -> DjotExcerpt {
    ExcerptRenderer::default()
        .sanitize()
        .render_into_document(djot_content)
        .expect("infallible")
}

/// What kind of content we are currently capturing.
#[derive(Debug, Clone, Copy)]
enum Capturing {
    Heading,
    Paragraph,
}

/// A [`Render`] implementation that extracts the first heading and first
/// paragraph from a djot document as plain text.
///
/// Can be composed with async filters (e.g. profile link resolution) via its
/// [`AsyncRender`] impl, then called with
/// [`AsyncRenderOutputExt::render_into_document`].
#[derive(Debug, Default)]
pub struct ExcerptRenderer {
    excerpt: DjotExcerpt,
    /// What we are currently capturing (`None` = not capturing).
    capturing: Option<Capturing>,
    /// Nesting depth within the captured container (to handle inline elements
    /// like emphasis, links, etc.).
    depth: usize,
    /// Buffer for text being accumulated.
    buf: String,
}

impl<'s> Render<'s> for ExcerptRenderer {
    type Error = std::convert::Infallible;

    fn emit(&mut self, event: Event<'s>) -> Result<(), Self::Error> {
        // Early exit: if we already have both, nothing left to do.
        if self.excerpt.first_heading.is_some() && self.excerpt.first_paragraph.is_some() {
            return Ok(());
        }

        match event {
            // Start of a container we might want to capture.
            Event::Start(container, _attrs) => {
                if self.capturing.is_some() {
                    // Already inside a captured container — track nesting for
                    // inline elements (emphasis, links, spans, etc.).
                    self.depth += 1;
                } else {
                    match container {
                        Container::Heading { .. } if self.excerpt.first_heading.is_none() => {
                            self.capturing = Some(Capturing::Heading);
                            self.depth = 0;
                            self.buf.clear();
                        }
                        Container::Paragraph if self.excerpt.first_paragraph.is_none() => {
                            self.capturing = Some(Capturing::Paragraph);
                            self.depth = 0;
                            self.buf.clear();
                        }
                        _ => {}
                    }
                }
            }

            // End of a container.
            Event::End => {
                if let Some(capturing) = self.capturing {
                    if self.depth == 0 {
                        // Closing the container we started capturing.
                        let text = self.buf.trim().to_owned();
                        let text = if text.is_empty() { None } else { Some(text) };
                        match capturing {
                            Capturing::Heading => {
                                self.excerpt.first_heading = text;
                            }
                            Capturing::Paragraph => {
                                self.excerpt.first_paragraph = text;
                            }
                        }
                        self.capturing = None;
                        self.buf.clear();
                    } else {
                        self.depth -= 1;
                    }
                }
            }

            // Text content — only collect while capturing.
            Event::Str(s) if self.capturing.is_some() => {
                self.buf.push_str(&s);
            }

            // Whitespace-like events — append a space while capturing.
            Event::Softbreak | Event::Hardbreak | Event::NonBreakingSpace
                if self.capturing.is_some() =>
            {
                self.buf.push(' ');
            }

            // Smart punctuation.
            Event::LeftSingleQuote if self.capturing.is_some() => {
                self.buf.push('\u{2018}');
            }
            Event::RightSingleQuote if self.capturing.is_some() => {
                self.buf.push('\u{2019}');
            }
            Event::LeftDoubleQuote if self.capturing.is_some() => {
                self.buf.push('\u{201C}');
            }
            Event::RightDoubleQuote if self.capturing.is_some() => {
                self.buf.push('\u{201D}');
            }
            Event::Ellipsis if self.capturing.is_some() => {
                self.buf.push('\u{2026}');
            }
            Event::EnDash if self.capturing.is_some() => {
                self.buf.push('\u{2013}');
            }
            Event::EmDash if self.capturing.is_some() => {
                self.buf.push('\u{2014}');
            }

            // Symbols like `:name:`.
            Event::Symbol(s) if self.capturing.is_some() => {
                self.buf.push(':');
                self.buf.push_str(&s);
                self.buf.push(':');
            }

            _ => {}
        }

        Ok(())
    }
}

impl<'s> RenderOutput<'s> for ExcerptRenderer {
    type Output = DjotExcerpt;

    fn into_output(self) -> DjotExcerpt {
        self.excerpt
    }
}

#[async_trait::async_trait]
impl<'s> AsyncRender<'s> for ExcerptRenderer {
    type Error = std::convert::Infallible;

    async fn emit(&mut self, event: Event<'s>) -> Result<(), Self::Error> {
        Render::emit(self, event)
    }
}

#[async_trait::async_trait]
impl<'s> AsyncRenderOutput<'s> for ExcerptRenderer {
    type Output = DjotExcerpt;

    fn into_output(self) -> DjotExcerpt {
        self.excerpt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_heading_only() {
        let excerpt = extract_excerpt("# Hello World\n");
        assert_eq!(excerpt.first_heading.as_deref(), Some("Hello World"));
        assert_eq!(excerpt.first_paragraph, None);
    }

    #[test]
    fn test_paragraph_only() {
        let excerpt = extract_excerpt("Just a paragraph.\n");
        assert_eq!(excerpt.first_heading, None);
        assert_eq!(
            excerpt.first_paragraph.as_deref(),
            Some("Just a paragraph.")
        );
    }

    #[test]
    fn test_heading_and_paragraph() {
        let excerpt = extract_excerpt("# My Title\n\nSome description here.\n");
        assert_eq!(excerpt.first_heading.as_deref(), Some("My Title"));
        assert_eq!(
            excerpt.first_paragraph.as_deref(),
            Some("Some description here.")
        );
    }

    #[test]
    fn test_empty_content() {
        let excerpt = extract_excerpt("");
        assert_eq!(excerpt.first_heading, None);
        assert_eq!(excerpt.first_paragraph, None);
    }

    #[test]
    fn test_nested_formatting_in_heading() {
        let excerpt = extract_excerpt("# Hello *bold* and [a link](http://example.com)\n");
        assert_eq!(
            excerpt.first_heading.as_deref(),
            Some("Hello bold and a link")
        );
    }

    #[test]
    fn test_code_block_not_captured() {
        let excerpt = extract_excerpt("``` rust\nfn main() {}\n```\n\nActual paragraph.\n");
        assert_eq!(excerpt.first_heading, None);
        assert_eq!(
            excerpt.first_paragraph.as_deref(),
            Some("Actual paragraph.")
        );
    }

    #[test]
    fn test_captures_first_only() {
        let excerpt =
            extract_excerpt("# First Heading\n\n# Second Heading\n\nFirst para.\n\nSecond para.\n");
        assert_eq!(excerpt.first_heading.as_deref(), Some("First Heading"));
        assert_eq!(excerpt.first_paragraph.as_deref(), Some("First para."));
    }

    #[test]
    fn test_smart_punctuation() {
        let excerpt = extract_excerpt("\"Hello\" -- world...\n");
        assert_eq!(
            excerpt.first_paragraph.as_deref(),
            Some("\u{201C}Hello\u{201D} \u{2013} world\u{2026}")
        );
    }

    #[test]
    fn test_multiline_paragraph() {
        let excerpt = extract_excerpt("First line\nsecond line\nthird line.\n");
        assert_eq!(
            excerpt.first_paragraph.as_deref(),
            Some("First line second line third line.")
        );
    }

    #[test]
    fn test_raw_html_in_paragraph_sanitized() {
        // Raw HTML inline should be converted to code block by sanitize filter,
        // so the HTML tags should NOT appear in the extracted paragraph text.
        let excerpt = extract_excerpt("Normal text.\n\n```=html\n<script>alert(1)</script>\n```\n");
        assert_eq!(excerpt.first_paragraph.as_deref(), Some("Normal text."));
        // The raw HTML block is converted to a code block, not a paragraph
    }
}
