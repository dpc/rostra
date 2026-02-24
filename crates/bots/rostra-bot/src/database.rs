use std::sync::Arc;

use rostra_client_db::{Database as ClientDatabase, DbResult};
use rostra_core::Timestamp;
use tracing::{debug, info};

use crate::dedup::{self, MIN_TITLE_TOKENS, TITLE_SIMILARITY_THRESHOLD};
use crate::tables::{
    Article, TitleEntry, articles_published, articles_published_titles, articles_published_urls,
    articles_unpublished, hn_articles_published, hn_articles_unpublished,
};

pub struct BotDatabase {
    client_db: Arc<ClientDatabase>,
}

impl BotDatabase {
    pub fn new(client_db: Arc<ClientDatabase>) -> Self {
        Self { client_db }
    }

    /// Initialize bot specific tables
    pub async fn init_tables(&self) -> DbResult<()> {
        self.client_db
            .write_with(|tx| {
                let _unpublished_table = tx.open_table(&articles_unpublished::TABLE)?;
                let _published_table = tx.open_table(&articles_published::TABLE)?;
                let _published_urls_table = tx.open_table(&articles_published_urls::TABLE)?;
                let _published_titles_table = tx.open_table(&articles_published_titles::TABLE)?;
                // Legacy HN tables for backward compatibility
                let _hn_unpublished_table = tx.open_table(&hn_articles_unpublished::TABLE)?;
                let _hn_published_table = tx.open_table(&hn_articles_published::TABLE)?;
                Ok(())
            })
            .await
    }

    /// Add an unpublished article to the database.
    ///
    /// Three-layer dedup:
    /// 1. Exact ID match (existing published + unpublished)
    /// 2. Normalized URL match
    /// 3. Title Jaccard similarity + temporal proximity
    pub async fn add_unpublished_article(&self, article: &Article) -> DbResult<bool> {
        self.client_db
            .write_with(|tx| {
                let mut unpublished_table = tx.open_table(&articles_unpublished::TABLE)?;
                let published_table = tx.open_table(&articles_published::TABLE)?;
                let published_urls_table = tx.open_table(&articles_published_urls::TABLE)?;
                let published_titles_table = tx.open_table(&articles_published_titles::TABLE)?;
                let hn_unpublished_table = tx.open_table(&hn_articles_unpublished::TABLE)?;
                let hn_published_table = tx.open_table(&hn_articles_published::TABLE)?;

                // ── Layer 1: Exact ID ───────────────────────────────
                if published_table.get(&article.id)?.is_some() {
                    debug!(article_id = %article.id, "Article already published, skipping");
                    return Ok(false);
                }

                // For HN articles, also check legacy tables
                if article.source == "hn" {
                    if let Ok(hn_id) = article.id.parse::<u32>() {
                        if hn_published_table.get(&hn_id)?.is_some() {
                            debug!(article_id = %article.id, hn_id = hn_id, "HN article already published in legacy table, skipping");
                            return Ok(false);
                        }
                        if hn_unpublished_table.get(&hn_id)?.is_some() {
                            debug!(article_id = %article.id, hn_id = hn_id, "HN article already in legacy unpublished queue, skipping");
                            return Ok(false);
                        }
                    }
                }

                // Check if already in unpublished queue (exact ID)
                if unpublished_table.get(&article.id)?.is_some() {
                    debug!(article_id = %article.id, "Article already in unpublished queue, skipping");
                    return Ok(false);
                }

                // ── Layer 2: Normalized URL ─────────────────────────
                if let Some(ref url) = article.url {
                    if let Some(normalized) = dedup::normalize_url(url) {
                        // Check published URLs table
                        if let Some(existing) = published_urls_table.get(&normalized)? {
                            debug!(
                                article_id = %article.id,
                                existing_id = %existing.value(),
                                normalized_url = %normalized,
                                "Article URL already published (fuzzy), skipping"
                            );
                            return Ok(false);
                        }

                        // Check unpublished queue (in-memory scan)
                        for result in unpublished_table.range::<String>(..)? {
                            let (_key, value) = result?;
                            let queued = value.value();
                            if let Some(ref queued_url) = queued.url {
                                if dedup::normalize_url(queued_url).as_ref() == Some(&normalized) {
                                    debug!(
                                        article_id = %article.id,
                                        existing_id = %queued.id,
                                        normalized_url = %normalized,
                                        "Article URL already in unpublished queue (fuzzy), skipping"
                                    );
                                    return Ok(false);
                                }
                            }
                        }
                    }
                }

                // ── Layer 3: Title Jaccard + temporal proximity ─────
                let new_normalized_title = dedup::normalize_title(&article.title);
                let new_tokens = dedup::title_tokens(&new_normalized_title);

                if MIN_TITLE_TOKENS <= new_tokens.len() {
                    let new_ts = dedup::article_timestamp(article);

                    // Check published titles table
                    for result in published_titles_table.range::<String>(..)? {
                        let (_key, value) = result?;
                        let entry = value.value();

                        // Cheap temporal check first
                        if !dedup::articles_are_close_in_time(new_ts, entry.article_timestamp) {
                            continue;
                        }

                        let existing_tokens = dedup::title_tokens(&entry.normalized_title);
                        if MIN_TITLE_TOKENS <= existing_tokens.len() {
                            let sim = dedup::jaccard_similarity(&new_tokens, &existing_tokens);
                            if TITLE_SIMILARITY_THRESHOLD < sim || (sim - TITLE_SIMILARITY_THRESHOLD).abs() < f64::EPSILON {
                                debug!(
                                    article_id = %article.id,
                                    similarity = sim,
                                    "Article title matches published article (fuzzy), skipping"
                                );
                                return Ok(false);
                            }
                        }
                    }

                    // Check unpublished queue titles
                    for result in unpublished_table.range::<String>(..)? {
                        let (_key, value) = result?;
                        let queued = value.value();
                        let queued_ts = dedup::article_timestamp(&queued);

                        if !dedup::articles_are_close_in_time(new_ts, queued_ts) {
                            continue;
                        }

                        let queued_normalized = dedup::normalize_title(&queued.title);
                        let queued_tokens = dedup::title_tokens(&queued_normalized);
                        if MIN_TITLE_TOKENS <= queued_tokens.len() {
                            let sim = dedup::jaccard_similarity(&new_tokens, &queued_tokens);
                            if TITLE_SIMILARITY_THRESHOLD < sim || (sim - TITLE_SIMILARITY_THRESHOLD).abs() < f64::EPSILON {
                                debug!(
                                    article_id = %article.id,
                                    existing_id = %queued.id,
                                    similarity = sim,
                                    "Article title matches queued article (fuzzy), skipping"
                                );
                                return Ok(false);
                            }
                        }
                    }
                }

                unpublished_table.insert(&article.id, article)?;
                info!(article_id = %article.id, title = %article.title, "Added article to unpublished queue");
                Ok(true)
            })
            .await
    }

    /// Get all unpublished articles
    pub async fn get_unpublished_articles(&self) -> DbResult<Vec<Article>> {
        self.client_db
            .read_with(|tx| {
                let unpublished_table = tx.open_table(&articles_unpublished::TABLE)?;
                let hn_unpublished_table = tx.open_table(&hn_articles_unpublished::TABLE)?;
                let mut articles = Vec::new();

                // Get articles from new generic table
                for result in unpublished_table.range::<String>(..)? {
                    let (_key, value) = result?;
                    articles.push(value.value().clone());
                }

                // Get articles from legacy HN table and convert them
                for result in hn_unpublished_table.range::<u32>(..)? {
                    let (_key, value) = result?;
                    let hn_article = value.value();
                    articles.push(Article::from(hn_article.clone()));
                }

                Ok(articles)
            })
            .await
    }

    /// Mark an article as published and remove from unpublished queue.
    ///
    /// Also inserts normalized URL and title into fuzzy dedup tables.
    pub async fn mark_article_published(
        &self,
        article: &Article,
        published_at: Timestamp,
    ) -> DbResult<()> {
        let article = article.clone();
        self.client_db
            .write_with(move |tx| {
                let mut unpublished_table = tx.open_table(&articles_unpublished::TABLE)?;
                let mut published_table = tx.open_table(&articles_published::TABLE)?;
                let mut published_urls_table = tx.open_table(&articles_published_urls::TABLE)?;
                let mut published_titles_table =
                    tx.open_table(&articles_published_titles::TABLE)?;
                let mut hn_unpublished_table = tx.open_table(&hn_articles_unpublished::TABLE)?;
                let mut hn_published_table = tx.open_table(&hn_articles_published::TABLE)?;

                let article_id = &article.id;

                // Remove from new unpublished table
                unpublished_table.remove(article_id)?;

                // If this is an HN article (numeric ID), also remove from legacy table
                if let Ok(hn_id) = article_id.parse::<u32>() {
                    hn_unpublished_table.remove(&hn_id)?;
                    hn_published_table.insert(&hn_id, &published_at)?;
                }

                // Add to new published table
                published_table.insert(article_id, &published_at)?;

                // Insert normalized URL into fuzzy URL table
                if let Some(ref url) = article.url {
                    if let Some(normalized) = dedup::normalize_url(url) {
                        published_urls_table.insert(&normalized, article_id)?;
                    }
                }

                // Insert title entry into fuzzy title table
                let normalized_title = dedup::normalize_title(&article.title);
                let ts = article.published_at.unwrap_or(published_at);
                published_titles_table.insert(
                    article_id,
                    &TitleEntry {
                        normalized_title,
                        article_timestamp: ts,
                    },
                )?;

                info!(article_id = %article_id, "Marked article as published");
                Ok(())
            })
            .await
    }

    /// Remove an article from the unpublished queue without marking it as
    /// published.
    ///
    /// Workaround for draining stale entries that were enqueued before the
    /// scraping-time age filter existed. Can be removed once all deployed
    /// databases have been drained.
    pub async fn remove_unpublished_article(&self, article_id: &str) -> DbResult<()> {
        let article_id = article_id.to_string();
        self.client_db
            .write_with(move |tx| {
                let mut unpublished_table = tx.open_table(&articles_unpublished::TABLE)?;
                unpublished_table.remove(&article_id)?;
                Ok(())
            })
            .await
    }

    /// Get count of unpublished articles
    pub async fn get_unpublished_count(&self) -> DbResult<usize> {
        self.client_db
            .read_with(|tx| {
                let unpublished_table = tx.open_table(&articles_unpublished::TABLE)?;
                let hn_unpublished_table = tx.open_table(&hn_articles_unpublished::TABLE)?;
                let mut count = 0;

                // Count articles in new generic table
                for _ in unpublished_table.range::<String>(..)? {
                    count += 1;
                }

                // Count articles in legacy HN table
                for _ in hn_unpublished_table.range::<u32>(..)? {
                    count += 1;
                }

                Ok(count)
            })
            .await
    }
}
