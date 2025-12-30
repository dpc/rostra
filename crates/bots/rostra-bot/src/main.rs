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
use rostra_client_db::Database;
use rostra_core::Timestamp;
use snafu::{ResultExt, Snafu};
use tokio::time::{Duration, interval};
use tracing::level_filters::LevelFilter;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use crate::database::BotDatabase;
use crate::publisher::{Publisher, PublisherError};
use crate::scraper::{Scraper, ScraperError, create_scraper};

pub const PROJECT_NAME: &str = "rostra-bot";
pub const LOG_TARGET: &str = "rostra_bot::main";

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
    #[snafu(display("Secret file is required for bot operation"))]
    MissingSecretFile,
}

pub type BotResult<T> = std::result::Result<T, BotError>;

#[derive(Debug, Clone, clap::ValueEnum)]
pub enum Source {
    #[value(name = "hn")]
    HackerNews,
    #[value(name = "lobsters")]
    Lobsters,
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Source::HackerNews => write!(f, "HackerNews"),
            Source::Lobsters => write!(f, "Lobsters"),
        }
    }
}

/// Rostra Bot - scrapes news sites and publishes to Rostra
#[derive(Debug, Parser)]
#[command(version, about, long_about = None)]
pub struct Opts {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Path to the secret file for authentication
    #[arg(long)]
    pub secret_file: Option<PathBuf>,

    /// Interval between scraping runs in minutes
    #[arg(long, default_value = "30")]
    pub scrape_interval_minutes: u64,

    /// Maximum articles to publish per run
    #[arg(long, default_value = "5")]
    pub max_articles_per_run: usize,

    /// Minimum score threshold for articles
    #[arg(long, default_value = "0")]
    pub min_score: u32,

    /// Data dir to store the database in
    #[arg(long)]
    pub data_dir: Option<PathBuf>,

    /// Source to scrape from
    #[arg(long, value_enum, default_value = "hn")]
    pub source: Source,
}

#[derive(Debug, Parser)]
pub enum Command {
    /// Development commands
    Dev {
        #[command(subcommand)]
        dev_command: DevCommand,
    },
}

#[derive(Debug, Parser)]
pub enum DevCommand {
    /// Test scraping functionality
    Test {
        /// Source to scrape from
        #[arg(long, value_enum, default_value = "hn")]
        source: Source,
    },
}

#[snafu::report]
#[tokio::main]
async fn main() -> BotResult<()> {
    init_logging()?;

    let opts = Opts::parse();

    match opts.command {
        Some(Command::Dev { dev_command }) => handle_dev_command(dev_command).await,
        None => {
            // Default behavior - run the bot
            let secret_file = opts
                .secret_file
                .clone()
                .ok_or_else(|| BotError::MissingSecretFile)?;
            run_bot(opts, secret_file).await
        }
    }
}

async fn run_bot(opts: Opts, secret_file: PathBuf) -> BotResult<()> {
    info!(target: LOG_TARGET, "Starting Rostra Bot for {}", opts.source);
    info!(
      target: LOG_TARGET,
      source = %opts.source,
      scrape_interval = opts.scrape_interval_minutes,
      max_articles = opts.max_articles_per_run,
      min_score = opts.min_score,
      "Bot configuration"
    );

    let secret = Client::read_id_secret(&secret_file)
        .await
        .context(SecretSnafu)?;

    info!(
        target: LOG_TARGET,
        id = %secret.id(),
        "Loaded secret"
    );

    let db = if let Some(data_dir) = opts.data_dir.clone() {
        Some(
            Database::open(&data_dir.join("rostra.redb"), secret.id())
                .await
                .context(DatabaseSnafu)?,
        )
    } else {
        None
    };

    let client = Client::builder(secret.id())
        .secret(secret)
        .maybe_db(db)
        .build()
        .await
        .context(InitSnafu)?;

    info!(target: LOG_TARGET, "Client initialized successfully");

    // Initialize bot database using client's database
    let db = BotDatabase::new(client.db().clone());
    db.init_tables().await.context(DatabaseSnafu)?;

    info!(target: LOG_TARGET, "Bot database initialized");

    // Initialize scraper and publisher
    let scraper = create_scraper(&opts.source);
    let publisher = Publisher::new(client.clone(), secret);

    info!(target: LOG_TARGET, "Bot is running. Press Ctrl+C to stop.");

    // Main bot loop
    run_bot_loop(&opts, &db, scraper.as_ref(), &publisher).await
}

async fn handle_dev_command(dev_command: DevCommand) -> BotResult<()> {
    match dev_command {
        DevCommand::Test { source } => {
            info!(target: LOG_TARGET, "Testing scraper for {}", source);

            let scraper = create_scraper(&source);

            match scraper.scrape_frontpage().await {
                Ok(articles) => {
                    println!(
                        "Successfully scraped {} articles from {}:",
                        articles.len(),
                        source
                    );
                    println!();

                    for (i, article) in articles.iter().enumerate() {
                        println!("Article {}: ", i + 1);
                        println!("  ID: {}", article.id);
                        println!("  Title: {}", article.title);
                        println!("  Score: {}", article.score);
                        println!("  Author: {}", article.author);
                        println!("  Source: {}", article.source);
                        println!("  URL: {}", article.url.as_deref().unwrap_or("None"));
                        println!("  Source URL: {}", article.source_url);
                        println!("  Scraped at: {:?}", article.scraped_at);
                        println!();
                    }

                    Ok(())
                }
                Err(e) => {
                    eprintln!("Failed to scrape {source}: {e}");
                    Err(BotError::Scraper { source: e })
                }
            }
        }
    }
}

async fn run_bot_loop(
    opts: &Opts,
    db: &BotDatabase,
    scraper: &dyn Scraper,
    publisher: &Publisher,
) -> BotResult<()> {
    let mut interval = interval(Duration::from_secs(opts.scrape_interval_minutes * 60));

    loop {
        info!(target: LOG_TARGET, "Starting scraping and publishing cycle");

        // Scrape articles
        match scraper.scrape_frontpage().await {
            Ok(articles) => {
                info!(target: LOG_TARGET, count = articles.len(), source = %opts.source, "Scraped articles");

                // Filter articles by score and add to database
                let mut added_count = 0;
                for article in articles {
                    if article.score >= opts.min_score {
                        match db.add_unpublished_article(&article).await {
                            Ok(true) => added_count += 1,
                            Ok(false) => {} // Already exists
                            Err(e) => {
                                warn!(target: LOG_TARGET, error = %e, article_id = %article.id, "Failed to add article to database")
                            }
                        }
                    }
                }
                info!(target: LOG_TARGET, added = added_count, "Added new articles to unpublished queue");
            }
            Err(e) => {
                error!(target: LOG_TARGET, error = %e, source = %opts.source, "Failed to scrape frontpage");
            }
        }

        // Publish unpublished articles
        match db.get_unpublished_articles().await {
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
                    for (article_id, result) in results {
                        match result {
                            Ok(()) => {
                                if let Err(e) =
                                    db.mark_article_published(&article_id, published_at).await
                                {
                                    error!(target: LOG_TARGET, error = %e, article_id = %article_id, "Failed to mark article as published in database");
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
