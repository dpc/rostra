use std::sync::Arc;

use rostra_client_db::{Database as ClientDatabase, DbResult};
use rostra_core::Timestamp;
use tracing::{debug, info};

use crate::tables::{HnArticle, hn_articles_published, hn_articles_unpublished};

pub struct HnBotDatabase {
    client_db: Arc<ClientDatabase>,
}

impl HnBotDatabase {
    pub fn new(client_db: Arc<ClientDatabase>) -> Self {
        Self { client_db }
    }

    /// Initialize HN bot specific tables
    pub async fn init_hn_tables(&self) -> DbResult<()> {
        self.client_db
            .write_with(|tx| {
                let _unpublished_table = tx.open_table(&hn_articles_unpublished::TABLE)?;
                let _published_table = tx.open_table(&hn_articles_published::TABLE)?;
                Ok(())
            })
            .await
    }

    /// Add an unpublished article to the database
    pub async fn add_unpublished_article(&self, article: &HnArticle) -> DbResult<bool> {
        self.client_db
            .write_with(|tx| {
                let mut unpublished_table = tx.open_table(&hn_articles_unpublished::TABLE)?;
                let published_table = tx.open_table(&hn_articles_published::TABLE)?;

                // Check if already published
                if published_table.get(&article.hn_id)?.is_some() {
                    debug!(hn_id = article.hn_id, "Article already published, skipping");
                    return Ok(false);
                }

                // Check if already in unpublished queue
                if unpublished_table.get(&article.hn_id)?.is_some() {
                    debug!(hn_id = article.hn_id, "Article already in unpublished queue, updating");
                }

                unpublished_table.insert(&article.hn_id, article)?;
                info!(hn_id = article.hn_id, title = %article.title, "Added article to unpublished queue");
                Ok(true)
            })
            .await
    }

    /// Get all unpublished articles
    pub async fn get_unpublished_articles(&self) -> DbResult<Vec<HnArticle>> {
        self.client_db
            .read_with(|tx| {
                let unpublished_table = tx.open_table(&hn_articles_unpublished::TABLE)?;
                let mut articles = Vec::new();

                for result in unpublished_table.range::<u32>(..)? {
                    let (_key, value) = result?;
                    articles.push(value.value().clone());
                }

                Ok(articles)
            })
            .await
    }

    /// Mark an article as published and remove from unpublished queue
    pub async fn mark_article_published(
        &self,
        hn_id: u32,
        published_at: Timestamp,
    ) -> DbResult<()> {
        self.client_db
            .write_with(|tx| {
                let mut unpublished_table = tx.open_table(&hn_articles_unpublished::TABLE)?;
                let mut published_table = tx.open_table(&hn_articles_published::TABLE)?;

                // Remove from unpublished
                unpublished_table.remove(&hn_id)?;

                // Add to published
                published_table.insert(&hn_id, &published_at)?;

                info!(hn_id = hn_id, "Marked article as published");
                Ok(())
            })
            .await
    }

    /// Get count of unpublished articles
    pub async fn get_unpublished_count(&self) -> DbResult<usize> {
        self.client_db
            .read_with(|tx| {
                let unpublished_table = tx.open_table(&hn_articles_unpublished::TABLE)?;
                let mut count = 0;
                for _ in unpublished_table.range::<u32>(..)? {
                    count += 1;
                }
                Ok(count)
            })
            .await
    }
}
