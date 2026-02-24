use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use reqwest::Client;
use rostra_core::Timestamp;
use scraper::{Html, Selector};
use snafu::{ResultExt, Snafu};
use tracing::{debug, info, warn};

use crate::tables::{Article, HnArticle};

const HN_BASE_URL: &str = "https://news.ycombinator.com/";
const LOBSTERS_BASE_URL: &str = "https://lobste.rs/";

/// Extract text content from an HTML element, properly decoding HTML entities.
/// Uses the element's text iterator which handles entity decoding,
/// then normalizes non-breaking spaces to regular spaces.
fn extract_text(element: &scraper::ElementRef) -> String {
    element.text().collect::<String>().replace('\u{00a0}', " ")
}

#[derive(Debug, Snafu)]
pub enum ScraperError {
    #[snafu(display("HTTP request failed: {source}"))]
    Http { source: reqwest::Error },
    #[snafu(display("Failed to parse HTML"))]
    HtmlParse,
    #[snafu(display("Failed to parse URL: {source}"))]
    UrlParse { source: url::ParseError },
    #[snafu(display("Failed to parse HN ID from string: {id_str}"))]
    HnIdParse { id_str: String },
    #[snafu(display("Failed to parse ID from string: {id_str}"))]
    IdParse { id_str: String },
    #[snafu(display("Failed to parse score from string: {score_str}"))]
    ScoreParse { score_str: String },
    #[snafu(display("Failed to parse Atom feed: {details}"))]
    AtomParse { details: String },
}

pub type ScraperResult<T> = std::result::Result<T, ScraperError>;

#[async_trait::async_trait]
pub trait Scraper {
    async fn scrape_frontpage(&self) -> ScraperResult<Vec<Article>>;
}

pub fn create_scrapers(
    hn: bool,
    lobsters: bool,
    atom_feed_urls: &[String],
) -> Vec<Box<dyn Scraper + Send + Sync>> {
    let mut scrapers: Vec<Box<dyn Scraper + Send + Sync>> = Vec::new();

    if hn {
        scrapers.push(Box::new(HnScraper::new()));
    }
    if lobsters {
        scrapers.push(Box::new(LobstersScraper::new()));
    }
    for url in atom_feed_urls {
        scrapers.push(Box::new(AtomScraper::new(url.clone())));
    }

    scrapers
}

pub struct HnScraper {
    client: Client,
}

#[async_trait::async_trait]
impl Scraper for HnScraper {
    async fn scrape_frontpage(&self) -> ScraperResult<Vec<Article>> {
        let hn_articles = self.scrape_hn_frontpage().await?;
        Ok(hn_articles.into_iter().map(Article::from).collect())
    }
}

impl Default for HnScraper {
    fn default() -> Self {
        Self::new()
    }
}

impl HnScraper {
    pub fn new() -> Self {
        let client = Client::builder()
            .user_agent("rostra-bot/1.0")
            .build()
            .expect("Failed to create HTTP client");

        Self { client }
    }

    /// Scrape the main HackerNews page and extract articles
    pub async fn scrape_hn_frontpage(&self) -> ScraperResult<Vec<HnArticle>> {
        info!("Scraping HackerNews frontpage");

        let response = self
            .client
            .get(HN_BASE_URL)
            .send()
            .await
            .context(HttpSnafu)?;

        let html_content = response.text().await.context(HttpSnafu)?;
        let document = Html::parse_document(&html_content);

        let mut articles = Vec::new();
        let scraped_at = Timestamp::from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("Time went backwards")
                .as_secs(),
        );

        // HN uses a table structure with specific classes
        // Each story row has class "athing" with an ID that's the HN item ID
        let story_selector = Selector::parse("tr.athing").map_err(|_| ScraperError::HtmlParse)?;
        let title_selector =
            Selector::parse("span.titleline > a").map_err(|_| ScraperError::HtmlParse)?;
        let score_selector = Selector::parse("span.score").map_err(|_| ScraperError::HtmlParse)?;
        let author_selector = Selector::parse("a.hnuser").map_err(|_| ScraperError::HtmlParse)?;

        // Collect scores and authors separately
        let scores: Vec<u32> = document
            .select(&score_selector)
            .map(|score_elem| {
                let score_text = score_elem.inner_html();
                score_text
                    .split_whitespace()
                    .next()
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(0)
            })
            .collect();

        let authors: Vec<String> = document
            .select(&author_selector)
            .map(|author_elem| author_elem.inner_html())
            .collect();

        for (story_index, story_element) in document.select(&story_selector).enumerate() {
            // Get HN ID from the story row ID attribute
            let hn_id_str = story_element.value().id().unwrap_or("");
            let hn_id = hn_id_str
                .parse::<u32>()
                .map_err(|_| ScraperError::HnIdParse {
                    id_str: hn_id_str.to_string(),
                })?;

            // Get title and URL from the title link
            let title_link = match story_element.select(&title_selector).next() {
                Some(link) => link,
                None => {
                    warn!(hn_id = hn_id, "No title link found for story, skipping");
                    continue;
                }
            };

            let title = extract_text(&title_link);
            let url = title_link.value().attr("href").map(|s| {
                // Handle relative URLs
                if s.starts_with("item?id=") {
                    format!("{HN_BASE_URL}{s}")
                } else {
                    s.to_string()
                }
            });

            // Get score and author by index
            let score = scores.get(story_index).copied().unwrap_or(0);
            let author = authors
                .get(story_index)
                .cloned()
                .unwrap_or_else(|| "unknown".to_string());

            let hn_url = format!("{HN_BASE_URL}item?id={hn_id}");

            let article = HnArticle {
                hn_id,
                title,
                url,
                hn_url,
                score,
                author,
                scraped_at,
            };

            debug!(hn_id = hn_id, title = %article.title, score = score, "Scraped article");
            articles.push(article);
        }

        info!(
            count = articles.len(),
            "Scraped articles from HackerNews frontpage"
        );
        Ok(articles)
    }
}

pub struct LobstersScraper {
    client: Client,
}

impl Default for LobstersScraper {
    fn default() -> Self {
        Self::new()
    }
}

impl LobstersScraper {
    pub fn new() -> Self {
        let client = Client::builder()
            .user_agent("rostra-bot/1.0")
            .build()
            .expect("Failed to create HTTP client");

        Self { client }
    }

    /// Scrape the main Lobsters page and extract articles
    pub async fn scrape_lobsters_frontpage(&self) -> ScraperResult<Vec<Article>> {
        info!("Scraping Lobsters frontpage");

        let response = self
            .client
            .get(LOBSTERS_BASE_URL)
            .send()
            .await
            .context(HttpSnafu)?;

        let html_content = response.text().await.context(HttpSnafu)?;
        let document = Html::parse_document(&html_content);

        let mut articles = Vec::new();
        let scraped_at = Timestamp::from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("Time went backwards")
                .as_secs(),
        );

        // Lobsters uses a list structure with li elements
        let story_selector = Selector::parse("li.story").map_err(|_| ScraperError::HtmlParse)?;

        for story_element in document.select(&story_selector) {
            // Extract story ID from data-shortid attribute
            let story_id = story_element
                .value()
                .attr("data-shortid")
                .unwrap_or_default()
                .to_string();

            if story_id.is_empty() {
                warn!("No story ID found for story, skipping");
                continue;
            }

            // Extract title and URL
            let title_selector = Selector::parse("a.u-url").map_err(|_| ScraperError::HtmlParse)?;
            let title_link = match story_element.select(&title_selector).next() {
                Some(link) => link,
                None => {
                    warn!(story_id = %story_id, "No title link found for story, skipping");
                    continue;
                }
            };

            let title = extract_text(&title_link);
            let url = title_link.value().attr("href").map(|s| s.to_string());

            // Extract score from voting element - correct selector for Lobsters
            let vote_selector =
                Selector::parse("div.voters > a.upvoter").map_err(|_| ScraperError::HtmlParse)?;
            let score = story_element
                .select(&vote_selector)
                .next()
                .and_then(|vote_elem| {
                    let vote_text = vote_elem.inner_html();
                    vote_text.trim().parse::<u32>().ok()
                })
                .unwrap_or(0);

            // Extract author from byline
            let author_selector =
                Selector::parse("a.u-author").map_err(|_| ScraperError::HtmlParse)?;
            let author = story_element
                .select(&author_selector)
                .next()
                .map(|author_elem| author_elem.inner_html())
                .unwrap_or_else(|| "unknown".to_string());

            let source_url = format!("{LOBSTERS_BASE_URL}s/{story_id}");

            let article = Article {
                id: story_id.clone(),
                title,
                url,
                source_url,
                score,
                author,
                scraped_at,
                source: "lobsters".to_string(),
                feed_title: None,
                feed_link: None,
                feed_subtitle: None,
                published_at: None,
            };

            debug!(story_id = %story_id, title = %article.title, score = score, "Scraped article");
            articles.push(article);
        }

        info!(
            count = articles.len(),
            "Scraped articles from Lobsters frontpage"
        );
        Ok(articles)
    }
}

#[async_trait::async_trait]
impl Scraper for LobstersScraper {
    async fn scrape_frontpage(&self) -> ScraperResult<Vec<Article>> {
        self.scrape_lobsters_frontpage().await
    }
}

pub struct AtomScraper {
    client: Client,
    feed_url: String,
    /// Short hash of the feed URL for use in source identifier
    feed_hash: String,
}

fn is_leap_year(year: i32) -> bool {
    time::util::is_leap_year(year)
}

impl AtomScraper {
    pub fn new(feed_url: String) -> Self {
        let client = Client::builder()
            .user_agent("rostra-bot/1.0")
            .build()
            .expect("Failed to create HTTP client");

        // Create a short hash of the feed URL for the source identifier
        let feed_hash = {
            let hash = blake3::hash(feed_url.as_bytes());
            data_encoding::HEXLOWER.encode(&hash.as_bytes()[..8])
        };

        Self {
            client,
            feed_url,
            feed_hash,
        }
    }

    /// Create a unique article ID from feed URL and entry ID
    fn create_article_id(&self, entry_id: &str) -> String {
        let combined = format!("{}:{}", self.feed_url, entry_id);
        let hash = blake3::hash(combined.as_bytes());
        data_encoding::HEXLOWER.encode(&hash.as_bytes()[..16])
    }

    /// Parse an RFC 3339 / ISO 8601 date string to a Timestamp
    /// Example formats: "2024-01-15T10:30:00Z", "2024-01-15T10:30:00+00:00"
    fn parse_rfc3339_date(date_str: &str) -> Option<Timestamp> {
        // Extract date components from the beginning of the string
        // Format: YYYY-MM-DDTHH:MM:SS...
        if date_str.len() < 19 {
            return None;
        }

        let year: i32 = date_str.get(0..4)?.parse().ok()?;
        let month: u32 = date_str.get(5..7)?.parse().ok()?;
        let day: u32 = date_str.get(8..10)?.parse().ok()?;
        let hour: u32 = date_str.get(11..13)?.parse().ok()?;
        let minute: u32 = date_str.get(14..16)?.parse().ok()?;
        let second: u32 = date_str.get(17..19)?.parse().ok()?;

        // Calculate Unix timestamp (simplified, assumes UTC)
        // Days from year 1970 to year Y
        let days_since_epoch = {
            let mut days: i64 = 0;
            for y in 1970..year {
                days += if is_leap_year(y) { 366 } else { 365 };
            }
            // Days in current year up to this month
            let days_in_months = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
            for m in 1..month {
                days += days_in_months[(m - 1) as usize] as i64;
                if m == 2 && is_leap_year(year) {
                    days += 1;
                }
            }
            days += (day - 1) as i64;
            days
        };

        let timestamp =
            days_since_epoch * 86400 + (hour as i64) * 3600 + (minute as i64) * 60 + second as i64;

        Some(Timestamp::from(timestamp as u64))
    }

    /// Extract the best URL from an Atom entry's links
    fn extract_url(entry: &atom_syndication::Entry) -> Option<String> {
        let links = &entry.links;

        // First, try to find a link with rel="alternate" or no rel (default is
        // alternate)
        for link in links {
            match link.rel.as_deref() {
                Some("alternate") | None => return Some(link.href.clone()),
                _ => {}
            }
        }

        // Fall back to the first link
        links.first().map(|link| link.href.clone())
    }

    /// Scrape the Atom feed and extract articles
    pub async fn scrape_atom_feed(&self) -> ScraperResult<Vec<Article>> {
        info!(feed_url = %self.feed_url, "Scraping Atom feed");

        let response = self
            .client
            .get(&self.feed_url)
            .send()
            .await
            .context(HttpSnafu)?;

        let content = response.text().await.context(HttpSnafu)?;

        let feed =
            atom_syndication::Feed::from_str(&content).map_err(|e| ScraperError::AtomParse {
                details: e.to_string(),
            })?;

        let mut articles = Vec::new();
        let scraped_at = Timestamp::from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("Time went backwards")
                .as_secs(),
        );

        // Extract feed metadata
        let feed_title = Some(feed.title.to_string());
        let feed_link = feed
            .links
            .iter()
            .find(|l| l.rel.as_deref() == Some("alternate") || l.rel.is_none())
            .or_else(|| feed.links.first())
            .map(|l| l.href.clone());
        let feed_subtitle = feed.subtitle.as_ref().map(|s| s.to_string());

        for entry in &feed.entries {
            let entry_id = &entry.id;
            // Atom titles may contain non-breaking spaces
            let title = entry.title.replace('\u{00a0}', " ");

            let url = Self::extract_url(entry);

            // Use the URL as source_url since Atom feeds don't have separate comment pages
            let source_url = url.clone().unwrap_or_else(|| self.feed_url.clone());

            let author = entry
                .authors
                .first()
                .map(|p| p.name.clone())
                .unwrap_or_else(|| "unknown".to_string());

            // Extract publication date (prefer published, fall back to updated)
            // Atom dates are ISO 8601 / RFC 3339 format, e.g. "2024-01-15T10:30:00Z"
            let published_at = entry
                .published
                .as_ref()
                .or(Some(&entry.updated))
                .and_then(|date_str| Self::parse_rfc3339_date(date_str));

            let article = Article {
                id: self.create_article_id(entry_id),
                title,
                url,
                source_url,
                score: 0, // Atom feeds don't have scores
                author,
                scraped_at,
                source: format!("atom:{}", self.feed_hash),
                feed_title: feed_title.clone(),
                feed_link: feed_link.clone(),
                feed_subtitle: feed_subtitle.clone(),
                published_at,
            };

            debug!(
                entry_id = %entry_id,
                title = %article.title,
                "Scraped Atom entry"
            );
            articles.push(article);
        }

        info!(
            count = articles.len(),
            feed_url = %self.feed_url,
            "Scraped articles from Atom feed"
        );
        Ok(articles)
    }
}

#[async_trait::async_trait]
impl Scraper for AtomScraper {
    async fn scrape_frontpage(&self) -> ScraperResult<Vec<Article>> {
        self.scrape_atom_feed().await
    }
}

#[cfg(test)]
mod tests {
    use scraper::{Html, Selector};

    use super::*;

    /// Helper to extract text from an HTML fragment.
    /// Wraps content in a div to ensure we always have an element to select.
    fn text_from_html(html: &str) -> String {
        let wrapped = format!("<div>{html}</div>");
        let fragment = Html::parse_fragment(&wrapped);
        let selector = Selector::parse("div").unwrap();
        let el = fragment.select(&selector).next().expect("div must exist");
        extract_text(&el)
    }

    #[test]
    fn extract_text_plain() {
        assert_eq!(text_from_html("<span>Hello World</span>"), "Hello World");
    }

    #[test]
    fn extract_text_strips_bold() {
        assert_eq!(
            text_from_html("<span>Hello <b>World</b></span>"),
            "Hello World"
        );
    }

    #[test]
    fn extract_text_strips_nested_tags() {
        assert_eq!(
            text_from_html("<span>A <b>B <i>C</i> D</b> E</span>"),
            "A B C D E"
        );
    }

    #[test]
    fn extract_text_converts_nbsp_entity() {
        assert_eq!(
            text_from_html("<span>Hello&nbsp;World</span>"),
            "Hello World"
        );
    }

    #[test]
    fn extract_text_converts_nbsp_unicode() {
        // U+00A0 non-breaking space
        assert_eq!(
            text_from_html("<span>Hello\u{00a0}World</span>"),
            "Hello World"
        );
    }

    #[test]
    fn extract_text_preserves_em_dash() {
        // &mdash; is em-dash U+2014
        assert_eq!(
            text_from_html("<span>Hello&mdash;World</span>"),
            "Hello\u{2014}World"
        );
    }

    #[test]
    fn extract_text_preserves_en_dash() {
        // &ndash; is en-dash U+2013
        assert_eq!(
            text_from_html("<span>2020&ndash;2024</span>"),
            "2020\u{2013}2024"
        );
    }

    #[test]
    fn extract_text_decodes_common_entities() {
        assert_eq!(text_from_html("<span>&amp;</span>"), "&");
        assert_eq!(text_from_html("<span>&lt;</span>"), "<");
        assert_eq!(text_from_html("<span>&gt;</span>"), ">");
        assert_eq!(text_from_html("<span>&quot;</span>"), "\"");
        assert_eq!(text_from_html("<span>&apos;</span>"), "'");
    }

    #[test]
    fn extract_text_handles_mixed_content() {
        assert_eq!(
            text_from_html("<span>The&nbsp;<b>Rust</b>&mdash;Programming&nbsp;Language</span>"),
            "The Rust\u{2014}Programming Language"
        );
    }

    #[test]
    fn extract_text_strips_links() {
        assert_eq!(
            text_from_html("<span>Check out <a href=\"https://example.com\">this link</a>!</span>"),
            "Check out this link!"
        );
    }
}
