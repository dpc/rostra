use std::io;
use std::path::PathBuf;

use clap::Parser;
use rostra_bot::database::BotDatabase;
use rostra_bot::publisher::{Publisher, PublisherError};
use rostra_bot::scraper::{ScraperError, create_scrapers};
use rostra_bot::{LOG_TARGET, run_one_cycle};
use rostra_client::Client;
use rostra_client::error::{IdSecretReadError, InitError};
use rostra_client_db::Database;
use snafu::{ResultExt, Snafu};
use tokio::time::{Duration, interval};
use tracing::info;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::EnvFilter;

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
    #[snafu(display("Run cycle error: {source}"))]
    RunCycle {
        source: Box<dyn std::error::Error + Send + Sync>,
    },
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
    scrapers: &[Box<dyn rostra_bot::scraper::Scraper + Send + Sync>],
    publisher: &Publisher,
) -> BotResult<()> {
    let mut interval = interval(Duration::from_secs(opts.scrape_interval_minutes * 60));

    loop {
        run_one_cycle(
            opts.hn_min_score,
            opts.lobsters_min_score,
            opts.max_articles_per_run,
            db,
            scrapers,
            publisher,
        )
        .await
        .map_err(|source| BotError::RunCycle { source })?;

        info!(target: LOG_TARGET,
              next_run_in_minutes = opts.scrape_interval_minutes,
              "Cycle complete, waiting for next run"
        );

        interval.tick().await;
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

fn init_logging() -> BotResult<()> {
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
