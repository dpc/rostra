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

            let title = title_link.inner_html();
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

            let title = title_link.inner_html();
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

        for entry in &feed.entries {
            let entry_id = &entry.id;
            let title = entry.title.clone();

            let url = Self::extract_url(entry);

            // Use the URL as source_url since Atom feeds don't have separate comment pages
            let source_url = url.clone().unwrap_or_else(|| self.feed_url.clone());

            let author = entry
                .authors
                .first()
                .map(|p| p.name.clone())
                .unwrap_or_else(|| "unknown".to_string());

            let article = Article {
                id: self.create_article_id(entry_id),
                title,
                url,
                source_url,
                score: 0, // Atom feeds don't have scores
                author,
                scraped_at,
                source: format!("atom:{}", self.feed_hash),
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
