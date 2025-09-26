use bincode::{Decode, Encode};
// Re-export the def_table macro from rostra-client-db
pub use rostra_client_db::def_table;
use rostra_core::Timestamp;
use serde::{Deserialize, Serialize};

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

// Table to store unpublished HN articles
def_table! {
    /// HackerNews articles waiting to be published
    hn_articles_unpublished: u32 => HnArticle
}

// Table to track published articles (to avoid duplicates)
def_table! {
    /// HackerNews articles that have been published
    hn_articles_published: u32 => Timestamp
}
