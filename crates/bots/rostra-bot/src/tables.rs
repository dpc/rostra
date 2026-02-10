use bincode::{Decode, Encode};
// Re-export the def_table macro from rostra-client-db
pub use rostra_client_db::def_table;
use rostra_core::Timestamp;
use serde::{Deserialize, Serialize};

#[derive(Debug, Encode, Decode, Clone, Serialize, Deserialize)]
pub struct Article {
    pub id: String,
    pub title: String,
    pub url: Option<String>,
    pub source_url: String,
    pub score: u32,
    pub author: String,
    pub scraped_at: Timestamp,
    pub source: String,
    /// Feed title (for Atom/RSS feeds)
    pub feed_title: Option<String>,
    /// Feed link/homepage URL (for Atom/RSS feeds)
    pub feed_link: Option<String>,
    /// Feed subtitle/description (for Atom/RSS feeds)
    pub feed_subtitle: Option<String>,
    /// Publication date of the article
    pub published_at: Option<Timestamp>,
}

// Legacy HnArticle for compatibility
#[derive(Debug, Encode, Decode, Clone, Serialize, Deserialize)]
pub struct HnArticle {
    pub hn_id: u32,
    pub title: String,
    pub url: Option<String>,
    pub hn_url: String,
    pub score: u32,
    pub author: String,
    pub scraped_at: Timestamp,
}

impl From<HnArticle> for Article {
    fn from(hn_article: HnArticle) -> Self {
        Article {
            id: hn_article.hn_id.to_string(),
            title: hn_article.title,
            url: hn_article.url,
            source_url: hn_article.hn_url,
            score: hn_article.score,
            author: hn_article.author,
            scraped_at: hn_article.scraped_at,
            source: "hn".to_string(),
            feed_title: None,
            feed_link: None,
            feed_subtitle: None,
            published_at: None,
        }
    }
}

// Tables for generic articles
def_table! {
    /// Articles waiting to be published
    articles_unpublished: String => Article
}

def_table! {
    /// Articles that have been published
    articles_published: String => Timestamp
}

// Legacy HN tables for backward compatibility
def_table! {
    /// HackerNews articles waiting to be published
    hn_articles_unpublished: u32 => HnArticle
}

def_table! {
    /// HackerNews articles that have been published
    hn_articles_published: u32 => Timestamp
}
