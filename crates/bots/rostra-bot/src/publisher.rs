use std::collections::BTreeSet;
use std::sync::Arc;

use rostra_client::Client;
use rostra_client::error::PostError;
use rostra_core::event::PersonaTag;
use rostra_core::id::RostraIdSecretKey;
use snafu::{ResultExt, Snafu};
use tracing::{info, warn};

use crate::tables::Article;

/// Escape parentheses in URLs for Djot/Markdown link syntax.
/// Parentheses in URLs conflict with the `[text](url)` syntax.
fn escape_url_for_djot(url: &str) -> String {
    url.replace('(', "%28").replace(')', "%29")
}

/// Escape text that will be used inside link brackets `[text]`.
/// Both `[` and `]` characters need escaping to prevent djot from
/// interpreting them as nested link syntax.
fn escape_link_text(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]")
}

#[derive(Debug, Snafu)]
pub enum PublisherError {
    #[snafu(display("Failed to post to Rostra: {source}"))]
    Post { source: PostError },
}

pub type PublisherResult<T> = std::result::Result<T, PublisherError>;

pub struct Publisher {
    client: Arc<Client>,
    secret: RostraIdSecretKey,
    persona_tags: BTreeSet<PersonaTag>,
}

impl Publisher {
    pub fn new(client: Arc<Client>, secret: RostraIdSecretKey) -> Self {
        Self {
            client,
            secret,
            persona_tags: BTreeSet::new(),
        }
    }

    /// Format an article into a Rostra post body
    fn format_article_post(&self, article: &Article) -> String {
        let mut post = String::new();

        // Handle different sources with appropriate formatting
        if article.source.starts_with("atom:") {
            // Atom feed format: title on top, then "by {author} from {feed_title}
            // ({subtitle})"
            if let Some(ref url) = article.url {
                let escaped_title = escape_link_text(&article.title);
                let escaped_url = escape_url_for_djot(url);
                post.push_str(&format!("##### [{escaped_title}]({escaped_url})\n\n"));
            } else {
                post.push_str(&format!("##### {}\n\n", article.title));
            }

            // Build the "Posted by ... on ... from ..." line
            post.push_str(&format!("Posted by {}", article.author));
            if let Some(published_at) = article.published_at {
                if let Some(dt) = published_at.to_offset_date_time() {
                    let (year, month, day) = (dt.year(), dt.month() as u8, dt.day());
                    post.push_str(&format!(" on {year}-{month:02}-{day:02}"));
                }
            }
            if let Some(ref feed_title) = article.feed_title {
                post.push_str(" from ");
                if let Some(ref feed_link) = article.feed_link {
                    let escaped_feed_title = escape_link_text(feed_title);
                    let escaped_link = escape_url_for_djot(feed_link);
                    post.push_str(&format!("[{escaped_feed_title}]({escaped_link})"));
                } else {
                    post.push_str(feed_title);
                }
                if let Some(ref subtitle) = article.feed_subtitle {
                    post.push_str(&format!(" ({subtitle})"));
                }
            }
            post.push('\n');
        } else {
            // HN/Lobsters format: title with comments link
            if let Some(ref url) = article.url {
                let escaped_title = escape_link_text(&article.title);
                let escaped_url = escape_url_for_djot(url);
                post.push_str(&format!("##### [{escaped_title}]({escaped_url})\n\n"));
            } else {
                post.push_str(&format!("##### {}\n\n", article.title));
            }

            let source_name = match article.source.as_str() {
                "hn" => "HN",
                "lobsters" => "Lobsters",
                _ => &article.source,
            };
            let escaped_source_url = escape_url_for_djot(&article.source_url);
            post.push_str(&format!(
                "* [💬 {source_name} Comments]({escaped_source_url})\n"
            ));
        }

        post.push('\n');
        post
    }

    /// Publish an article to Rostra
    pub async fn publish_article(&self, article: &Article) -> PublisherResult<()> {
        let body = self.format_article_post(article);

        info!(
            article_id = %article.id,
            title = %article.title,
            source = %article.source,
            "Publishing article to Rostra"
        );

        self.client
            .social_post(
                self.secret,
                body,
                None, // No reply to
                self.persona_tags.clone(),
            )
            .await
            .context(PostSnafu)?;

        info!(
            article_id = %article.id,
            "Successfully published article to Rostra"
        );
        Ok(())
    }

    /// Publish multiple articles with a delay between each
    pub async fn publish_articles(
        &self,
        articles: &[Article],
    ) -> Vec<(String, PublisherResult<()>)> {
        let mut results = Vec::new();

        for article in articles {
            let result = self.publish_article(article).await;

            if let Err(ref err) = result {
                warn!(
                    article_id = %article.id,
                    error = %err,
                    "Failed to publish article"
                );
            }

            results.push((article.id.clone(), result));

            // Add a small delay between posts to be respectful
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        }

        results
    }
}

#[cfg(test)]
mod tests {
    use jotup::r#async::AsyncRenderOutputExt;

    use super::*;

    /// Helper to render djot content to HTML
    async fn render_djot(content: &str) -> String {
        let out = jotup::html::tokio::Renderer::default()
            .render_into_document(content)
            .await
            .expect("Rendering failed");
        String::from_utf8(out.into_inner()).expect("valid utf8")
    }

    #[test]
    fn test_escape_url_for_djot() {
        assert_eq!(
            escape_url_for_djot("https://example.com/path"),
            "https://example.com/path"
        );
        assert_eq!(
            escape_url_for_djot("https://example.com/S1550-4131(26)00008-2"),
            "https://example.com/S1550-4131%2826%2900008-2"
        );
        assert_eq!(
            escape_url_for_djot("https://example.com/nested((parens))"),
            "https://example.com/nested%28%28parens%29%29"
        );
    }

    #[test]
    fn test_escape_link_text() {
        assert_eq!(escape_link_text("Normal title"), "Normal title");
        assert_eq!(
            escape_link_text("Title with [brackets]"),
            "Title with \\[brackets\\]"
        );
        assert_eq!(escape_link_text("Title with ] only"), "Title with \\] only");
        assert_eq!(escape_link_text("Title with [ only"), "Title with \\[ only");
        assert_eq!(
            escape_link_text("Title with \\ backslash"),
            "Title with \\\\ backslash"
        );
    }

    #[tokio::test]
    async fn test_url_with_parentheses_renders_correctly() {
        // Test the exact URL pattern that was causing issues
        let escaped_url = escape_url_for_djot(
            "https://www.cell.com/cell-metabolism/abstract/S1550-4131(26)00008-2",
        );
        let djot = format!(
            "##### [Semaglutide improves knee osteoarthritis]({})",
            escaped_url
        );

        let html = render_djot(&djot).await;

        // The link should be complete and correct
        assert!(
            html.contains(
                "href=\"https://www.cell.com/cell-metabolism/abstract/S1550-4131%2826%2900008-2\""
            ),
            "URL should have escaped parentheses. Got: {}",
            html
        );
        assert!(
            html.contains(">Semaglutide improves knee osteoarthritis</a>"),
            "Link text should be complete. Got: {}",
            html
        );
        // Should NOT have broken content like ")00008-2)" outside the link
        assert!(
            !html.contains("00008-2)"),
            "Should not have broken URL fragments in text. Got: {}",
            html
        );
    }

    #[tokio::test]
    async fn test_title_with_brackets_renders_correctly() {
        let escaped_title = escape_link_text("Article about [Rust] programming");
        let djot = format!("##### [{}](https://example.com/article)", escaped_title);

        let html = render_djot(&djot).await;

        // Should be a single link, not broken into multiple
        assert_eq!(
            html.matches("<a ").count(),
            1,
            "Should have exactly one link. Got: {}",
            html
        );
        // The link should contain the full text with brackets preserved
        assert!(
            html.contains("Article about [Rust] programming</a>"),
            "Link text should contain brackets. Got: {}",
            html
        );
    }

    #[tokio::test]
    async fn test_title_with_closing_bracket_renders_correctly() {
        // This is the edge case that would break without escaping
        let escaped_title = escape_link_text("Why ] matters in code");
        let djot = format!("##### [{}](https://example.com/article)", escaped_title);

        let html = render_djot(&djot).await;

        // Should have a proper link
        assert!(
            html.contains("<a ") && html.contains("</a>"),
            "Should have a complete link. Got: {}",
            html
        );
        assert!(
            html.contains("href=\"https://example.com/article\""),
            "Link should have correct href. Got: {}",
            html
        );
    }

    #[tokio::test]
    async fn test_feed_title_with_special_chars() {
        let escaped_feed_title = escape_link_text("Blog [Tech] News");
        let escaped_url = escape_url_for_djot("https://blog.example.com/(feed)");
        let djot = format!(
            "Posted by Author from [{}]({})",
            escaped_feed_title, escaped_url
        );

        let html = render_djot(&djot).await;

        // Feed title link should work
        assert!(
            html.contains("href=\"https://blog.example.com/%28feed%29\""),
            "Feed URL should have escaped parentheses. Got: {}",
            html
        );
    }

    #[tokio::test]
    async fn test_complex_url_with_multiple_special_chars() {
        // Real-world URLs can have multiple parentheses and other chars
        let url = "https://en.wikipedia.org/wiki/Rust_(programming_language)";
        let escaped = escape_url_for_djot(url);
        let djot = format!("[Rust]({})", escaped);

        let html = render_djot(&djot).await;

        assert!(
            html.contains("href=\"https://en.wikipedia.org/wiki/Rust_%28programming_language%29\""),
            "Wikipedia-style URL should be escaped. Got: {}",
            html
        );
        assert!(
            html.contains(">Rust</a>"),
            "Link text should be intact. Got: {}",
            html
        );
    }

    #[tokio::test]
    async fn test_hn_format_renders_correctly() {
        // Test the HN/Lobsters format with comments link
        let title = escape_link_text("Show HN: My [new] project");
        let article_url = escape_url_for_djot("https://github.com/user/project");
        let source_url = escape_url_for_djot("https://news.ycombinator.com/item?id=12345");

        let djot = format!("##### [{title}]({article_url})\n\n* [💬 HN Comments]({source_url})\n",);

        let html = render_djot(&djot).await;

        // Should have two links
        assert_eq!(
            html.matches("<a ").count(),
            2,
            "Should have two links. Got: {}",
            html
        );
        assert!(
            html.contains("💬 HN Comments</a>"),
            "Should have comments link text. Got: {}",
            html
        );
    }
}
