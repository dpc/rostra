mod database;
mod publisher;
mod scraper;
mod tables;

use std::io;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::Parser;
use rostra_client::Client;
use rostra_client::error::{IdSecretReadError, InitError};
use rostra_core::Timestamp;
use snafu::{ResultExt, Snafu};
use tokio::time::{Duration, interval};
use tracing::level_filters::LevelFilter;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use crate::database::HnBotDatabase;
use crate::publisher::{HnPublisher, PublisherError};
use crate::scraper::{HnScraper, ScraperError};

pub const PROJECT_NAME: &str = "rostra-bot-hn";
pub const LOG_TARGET: &str = "rostra_bot_hn::main";

#[derive(Debug, Snafu)]
pub enum BotError {
    #[snafu(display("Initialization error: {source}"))]
    Init {
        #[snafu(source(from(InitError, Box::new)))]
        source: Box<InitError>,
    },
    #[snafu(display("Secret read error: {source}"))]
    Secret { source: IdSecretReadError },
    #[snafu(display("Database error: {source}"))]
    Database { source: rostra_client_db::DbError },
    #[snafu(display("Scraper error: {source}"))]
    Scraper { source: ScraperError },
    #[snafu(display("Publisher error: {source}"))]
    Publisher { source: PublisherError },
    #[snafu(display("Logging initialization failed"))]
    Logging,
}

pub type BotResult<T> = std::result::Result<T, BotError>;

/// Rostra Hacker News Bot
#[derive(Debug, Parser)]
#[command(version, about, long_about = None)]
pub struct Opts {
    /// Path to the secret file for authentication
    #[arg(long, required = true)]
    pub secret_file: PathBuf,

    /// Interval between scraping runs in minutes
    #[arg(long, default_value = "30")]
    pub scrape_interval_minutes: u64,

    /// Maximum articles to publish per run
    #[arg(long, default_value = "5")]
    pub max_articles_per_run: usize,

    /// Minimum score threshold for articles
    #[arg(long, default_value = "50")]
    pub min_score: u32,
}

#[snafu::report]
#[tokio::main]
async fn main() -> BotResult<()> {
    init_logging()?;

    let opts = Opts::parse();

    info!(target: LOG_TARGET, "Starting Rostra HN Bot");
    info!(
        target: LOG_TARGET,
      scrape_interval = opts.scrape_interval_minutes,
      max_articles = opts.max_articles_per_run,
      min_score = opts.min_score,
      "Bot configuration"
    );

    let secret = Client::read_id_secret(&opts.secret_file)
        .await
        .context(SecretSnafu)?;

    info!(
        target: LOG_TARGET,
        id = %secret.id(),
        "Loaded secret"
    );

    let client = Client::builder(secret.id())
        .secret(secret)
        .build()
        .await
        .context(InitSnafu)?;

    info!(target: LOG_TARGET, "Client initialized successfully");

    // Initialize HN bot database using client's database
    let hn_db = HnBotDatabase::new(client.db().clone());
    hn_db.init_hn_tables().await.context(DatabaseSnafu)?;

    info!(target: LOG_TARGET, "HN bot database initialized");

    // Initialize scraper and publisher
    let scraper = HnScraper::new();
    let publisher = HnPublisher::new(client.clone(), secret);

    info!(target: LOG_TARGET, "Bot is running. Press Ctrl+C to stop.");

    // Main bot loop
    run_bot_loop(&opts, &hn_db, &scraper, &publisher).await
}

async fn run_bot_loop(
    opts: &Opts,
    hn_db: &HnBotDatabase,
    scraper: &HnScraper,
    publisher: &HnPublisher,
) -> BotResult<()> {
    let mut interval = interval(Duration::from_secs(opts.scrape_interval_minutes * 60));

    loop {
        info!(target: LOG_TARGET, "Starting scraping and publishing cycle");

        // Scrape HN articles
        match scraper.scrape_frontpage().await {
            Ok(articles) => {
                info!(target: LOG_TARGET, count = articles.len(), "Scraped articles from HN");

                // Filter articles by score and add to database
                let mut added_count = 0;
                for article in articles {
                    if article.score >= opts.min_score {
                        match hn_db.add_unpublished_article(&article).await {
                            Ok(true) => added_count += 1,
                            Ok(false) => {} // Already exists
                            Err(e) => {
                                warn!(target: LOG_TARGET, error = %e, hn_id = article.hn_id, "Failed to add article to database")
                            }
                        }
                    }
                }
                info!(target: LOG_TARGET, added = added_count, "Added new articles to unpublished queue");
            }
            Err(e) => {
                error!(target: LOG_TARGET, error = %e, "Failed to scrape HN frontpage");
            }
        }

        // Publish unpublished articles
        match hn_db.get_unpublished_articles().await {
            Ok(articles) => {
                let articles_to_publish: Vec<_> = articles
                    .into_iter()
                    .take(opts.max_articles_per_run)
                    .collect();

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
                    for (hn_id, result) in results {
                        match result {
                            Ok(()) => {
                                if let Err(e) =
                                    hn_db.mark_article_published(hn_id, published_at).await
                                {
                                    error!(target: LOG_TARGET, error = %e, hn_id = hn_id, "Failed to mark article as published in database");
                                }
                            }
                            Err(e) => {
                                error!(target: LOG_TARGET, error = %e, hn_id = hn_id, "Failed to publish article");
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
        if let Ok(unpublished_count) = hn_db.get_unpublished_count().await {
            info!(target: LOG_TARGET, queue_size = unpublished_count, "Articles in unpublished queue");
        }

        info!(target: LOG_TARGET,
              next_run_in_minutes = opts.scrape_interval_minutes,
              "Cycle complete, waiting for next run"
        );

        interval.tick().await;
    }
}

pub fn init_logging() -> BotResult<()> {
    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .with_env_filter(
            EnvFilter::builder()
                .with_default_directive(LevelFilter::INFO.into())
                .from_env_lossy(),
        )
        .try_init()
        .map_err(|_| BotError::Logging)?;

    Ok(())
}
