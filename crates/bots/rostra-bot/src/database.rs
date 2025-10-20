use std::sync::Arc;

use rostra_client_db::{Database as ClientDatabase, DbResult};
use rostra_core::Timestamp;
use tracing::{debug, info};

use crate::tables::{
    Article, articles_published, articles_unpublished, hn_articles_published,
    hn_articles_unpublished,
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
                // Legacy HN tables for backward compatibility
                let _hn_unpublished_table = tx.open_table(&hn_articles_unpublished::TABLE)?;
                let _hn_published_table = tx.open_table(&hn_articles_published::TABLE)?;
                Ok(())
            })
            .await
    }

    /// Add an unpublished article to the database
    pub async fn add_unpublished_article(&self, article: &Article) -> DbResult<bool> {
        self.client_db
            .write_with(|tx| {
                let mut unpublished_table = tx.open_table(&articles_unpublished::TABLE)?;
                let published_table = tx.open_table(&articles_published::TABLE)?;
                let hn_unpublished_table = tx.open_table(&hn_articles_unpublished::TABLE)?;
                let hn_published_table = tx.open_table(&hn_articles_published::TABLE)?;

                // Check if already published in new tables
                if published_table.get(&article.id)?.is_some() {
                    debug!(article_id = %article.id, "Article already published, skipping");
                    return Ok(false);
                }

                // For HN articles, also check legacy tables
                if article.source == "hn" {
                    if let Ok(hn_id) = article.id.parse::<u32>() {
                        // Check legacy HN published table
                        if hn_published_table.get(&hn_id)?.is_some() {
                            debug!(article_id = %article.id, hn_id = hn_id, "HN article already published in legacy table, skipping");
                            return Ok(false);
                        }
                        // Check legacy HN unpublished table
                        if hn_unpublished_table.get(&hn_id)?.is_some() {
                            debug!(article_id = %article.id, hn_id = hn_id, "HN article already in legacy unpublished queue, skipping");
                            return Ok(false);
                        }
                    }
                }

                // Check if already in unpublished queue
                if unpublished_table.get(&article.id)?.is_some() {
                    debug!(article_id = %article.id, "Article already in unpublished queue, updating");
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

    /// Mark an article as published and remove from unpublished queue
    pub async fn mark_article_published(
        &self,
        article_id: &str,
        published_at: Timestamp,
    ) -> DbResult<()> {
        self.client_db
            .write_with(|tx| {
                let mut unpublished_table = tx.open_table(&articles_unpublished::TABLE)?;
                let mut published_table = tx.open_table(&articles_published::TABLE)?;
                let mut hn_unpublished_table = tx.open_table(&hn_articles_unpublished::TABLE)?;
                let mut hn_published_table = tx.open_table(&hn_articles_published::TABLE)?;

                // Remove from new unpublished table
                unpublished_table.remove(article_id)?;

                // If this is an HN article (numeric ID), also remove from legacy table
                if let Ok(hn_id) = article_id.parse::<u32>() {
                    hn_unpublished_table.remove(&hn_id)?;
                    hn_published_table.insert(&hn_id, &published_at)?;
                }

                // Add to new published table
                published_table.insert(article_id, &published_at)?;

                info!(article_id = %article_id, "Marked article as published");
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
