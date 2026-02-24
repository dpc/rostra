mod database;
mod dedup;
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
use tracing::{debug, error, info, warn};
use tracing_subscriber::EnvFilter;

use crate::database::BotDatabase;
use crate::publisher::{Publisher, PublisherError};
use crate::scraper::{Scraper, ScraperError, create_scrapers};

pub const PROJECT_NAME: &str = "rostra-bot";
pub const LOG_TARGET: &str = "rostra_bot::main";
const MAX_ARTICLE_AGE_SECS: u64 = 30 * 24 * 60 * 60; // ~1 month

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
    #[snafu(display("At least one source is required (--hn, --lobsters, or --atom-feed-url)"))]
    NoSourcesSpecified,
}

pub type BotResult<T> = std::result::Result<T, BotError>;

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

    /// Data dir to store the database in
    #[arg(long)]
    pub data_dir: Option<PathBuf>,

    /// Enable HackerNews scraping
    #[arg(long)]
    pub hn: bool,

    /// Minimum score for HackerNews articles
    #[arg(long, default_value = "0")]
    pub hn_min_score: u32,

    /// Enable Lobsters scraping
    #[arg(long)]
    pub lobsters: bool,

    /// Minimum score for Lobsters articles
    #[arg(long, default_value = "0")]
    pub lobsters_min_score: u32,

    /// Atom feed URLs to scrape (can specify multiple)
    #[arg(long, value_name = "URL", action = clap::ArgAction::Append)]
    pub atom_feed_url: Vec<String>,
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
        /// Enable HackerNews scraping
        #[arg(long)]
        hn: bool,

        /// Enable Lobsters scraping
        #[arg(long)]
        lobsters: bool,

        /// Atom feed URLs to scrape (can specify multiple)
        #[arg(long, value_name = "URL", action = clap::ArgAction::Append)]
        atom_feed_url: Vec<String>,
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
    let scrapers = create_scrapers(opts.hn, opts.lobsters, &opts.atom_feed_url);

    if scrapers.is_empty() {
        return Err(BotError::NoSourcesSpecified);
    }

    let source_desc = build_sources_description(&opts);

    info!(target: LOG_TARGET, sources = %source_desc, "Starting Rostra Bot");
    info!(
      target: LOG_TARGET,
      sources = %source_desc,
      scrape_interval = opts.scrape_interval_minutes,
      max_articles = opts.max_articles_per_run,
      hn_min_score = opts.hn_min_score,
      lobsters_min_score = opts.lobsters_min_score,
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

    // Initialize publisher
    let publisher = Publisher::new(client.clone(), secret);

    info!(target: LOG_TARGET, "Bot is running. Press Ctrl+C to stop.");

    // Main bot loop
    run_bot_loop(&opts, &db, &scrapers, &publisher).await
}

async fn handle_dev_command(dev_command: DevCommand) -> BotResult<()> {
    match dev_command {
        DevCommand::Test {
            hn,
            lobsters,
            atom_feed_url,
        } => {
            let scrapers = create_scrapers(hn, lobsters, &atom_feed_url);

            if scrapers.is_empty() {
                return Err(BotError::NoSourcesSpecified);
            }

            let source_desc = build_test_sources_description(hn, lobsters, &atom_feed_url);
            info!(target: LOG_TARGET, sources = %source_desc, "Testing scrapers");

            let mut total_articles = 0;

            for scraper in &scrapers {
                match scraper.scrape_frontpage().await {
                    Ok(articles) => {
                        println!("Scraped {} articles:", articles.len());
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

                        total_articles += articles.len();
                    }
                    Err(e) => {
                        eprintln!("Failed to scrape: {e}");
                        return Err(BotError::Scraper { source: e });
                    }
                }
            }

            println!(
                "Total: {} articles from {} source(s)",
                total_articles,
                scrapers.len()
            );
            Ok(())
        }
    }
}

async fn run_bot_loop(
    opts: &Opts,
    db: &BotDatabase,
    scrapers: &[Box<dyn Scraper + Send + Sync>],
    publisher: &Publisher,
) -> BotResult<()> {
    let mut interval = interval(Duration::from_secs(opts.scrape_interval_minutes * 60));

    loop {
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

                        let min_score = get_min_score_for_article(opts, &article);
                        if article.score >= min_score {
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

        info!(target: LOG_TARGET,
              next_run_in_minutes = opts.scrape_interval_minutes,
              "Cycle complete, waiting for next run"
        );

        interval.tick().await;
    }
}

fn get_min_score_for_article(opts: &Opts, article: &crate::tables::Article) -> u32 {
    if article.source == "hn" {
        opts.hn_min_score
    } else if article.source == "lobsters" {
        opts.lobsters_min_score
    } else {
        0
    }
}

fn build_sources_description(opts: &Opts) -> String {
    let mut sources = Vec::new();
    if opts.hn {
        sources.push("HackerNews".to_string());
    }
    if opts.lobsters {
        sources.push("Lobsters".to_string());
    }
    for url in &opts.atom_feed_url {
        sources.push(format!("Atom:{url}"));
    }
    sources.join(", ")
}

fn build_test_sources_description(hn: bool, lobsters: bool, atom_feed_urls: &[String]) -> String {
    let mut sources = Vec::new();
    if hn {
        sources.push("HackerNews".to_string());
    }
    if lobsters {
        sources.push("Lobsters".to_string());
    }
    for url in atom_feed_urls {
        sources.push(format!("Atom:{url}"));
    }
    sources.join(", ")
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
