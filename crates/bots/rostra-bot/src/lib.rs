pub mod database;
pub mod dedup;
pub mod publisher;
pub mod scraper;
pub mod tables;

use std::time::{SystemTime, UNIX_EPOCH};

use rostra_core::Timestamp;
use tracing::{debug, error, info, warn};

use crate::database::BotDatabase;
use crate::publisher::Publisher;
use crate::scraper::Scraper;

pub const PROJECT_NAME: &str = "rostra-bot";
pub const LOG_TARGET: &str = "rostra_bot::main";
pub const MAX_ARTICLE_AGE_SECS: u64 = 30 * 24 * 60 * 60; // ~1 month

pub fn get_min_score_for_article(
    hn_min_score: u32,
    lobsters_min_score: u32,
    article: &crate::tables::Article,
) -> u32 {
    if article.source == "hn" {
        hn_min_score
    } else if article.source == "lobsters" {
        lobsters_min_score
    } else {
        0
    }
}

pub async fn run_one_cycle(
    hn_min_score: u32,
    lobsters_min_score: u32,
    max_articles_per_run: usize,
    db: &BotDatabase,
    scrapers: &[Box<dyn Scraper + Send + Sync>],
    publisher: &Publisher,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    info!(target: LOG_TARGET, "Starting scraping and publishing cycle");

    // Scrape articles from all sources
    let mut total_added = 0;
    for scraper in scrapers {
        match scraper.scrape_frontpage().await {
            Ok(articles) => {
                info!(target: LOG_TARGET, count = articles.len(), "Scraped articles from source");

                // Filter articles by age and per-source score, then add to database
                let now = Timestamp::now();
                let mut added_count = 0;
                for article in articles {
                    // Skip articles older than ~1 month (when publication date is known)
                    if let Some(published_at) = article.published_at {
                        if MAX_ARTICLE_AGE_SECS < now.secs_since(published_at) {
                            debug!(
                                target: LOG_TARGET,
                                article_id = %article.id,
                                title = %article.title,
                                "Skipping old article"
                            );
                            continue;
                        }
                    }

                    let min_score =
                        get_min_score_for_article(hn_min_score, lobsters_min_score, &article);
                    if min_score <= article.score {
                        match db.add_unpublished_article(&article).await {
                            Ok(true) => added_count += 1,
                            Ok(false) => {}
                            Err(e) => {
                                warn!(target: LOG_TARGET, error = %e, article_id = %article.id, "Failed to add article to database")
                            }
                        }
                    }
                }
                total_added += added_count;
            }
            Err(e) => {
                error!(target: LOG_TARGET, error = %e, "Failed to scrape frontpage");
            }
        }
    }
    info!(target: LOG_TARGET, added = total_added, "Added new articles to unpublished queue");

    // Publish unpublished articles
    match db.get_unpublished_articles().await {
        Ok(articles) => {
            let articles_to_publish: Vec<_> =
                articles.into_iter().take(max_articles_per_run).collect();

            if !articles_to_publish.is_empty() {
                info!(target: LOG_TARGET, count = articles_to_publish.len(), "Publishing articles to Rostra");

                let results = publisher.publish_articles(&articles_to_publish).await;

                // Mark successful publications as published
                let published_at = Timestamp::from(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .expect("Time went backwards")
                        .as_secs(),
                );
                for (article_id, result) in results {
                    match result {
                        Ok(()) => {
                            if let Some(article) =
                                articles_to_publish.iter().find(|a| a.id == article_id)
                            {
                                if let Err(e) =
                                    db.mark_article_published(article, published_at).await
                                {
                                    error!(target: LOG_TARGET, error = %e, article_id = %article_id, "Failed to mark article as published in database");
                                }
                            }
                        }
                        Err(e) => {
                            error!(target: LOG_TARGET, error = %e, article_id = %article_id, "Failed to publish article");
                        }
                    }
                }
            } else {
                info!(target: LOG_TARGET, "No articles to publish");
            }
        }
        Err(e) => {
            error!(target: LOG_TARGET, error = %e, "Failed to get unpublished articles from database");
        }
    }

    // Show current queue status
    if let Ok(unpublished_count) = db.get_unpublished_count().await {
        info!(target: LOG_TARGET, queue_size = unpublished_count, "Articles in unpublished queue");
    }

    Ok(())
}
