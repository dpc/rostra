use std::sync::Arc;

use async_trait::async_trait;
use rostra_bot::database::BotDatabase;
use rostra_bot::dedup::DEDUP_TIME_WINDOW_SECS;
use rostra_bot::publisher::Publisher;
use rostra_bot::scraper::{Scraper, ScraperResult};
use rostra_bot::tables::Article;
use rostra_client::Client;
use rostra_core::Timestamp;
use rostra_core::id::RostraIdSecretKey;
use tokio::sync::Mutex;

/// A fixed "now" timestamp for deterministic tests.
const NOW: u64 = 1_700_000_000;

/// A mock scraper that returns a configurable list of articles.
struct MockScraper {
    articles: Arc<Mutex<Vec<Article>>>,
}

impl MockScraper {
    fn new(articles: Vec<Article>) -> Self {
        Self {
            articles: Arc::new(Mutex::new(articles)),
        }
    }
}

#[async_trait]
impl Scraper for MockScraper {
    async fn scrape_frontpage(&self) -> ScraperResult<Vec<Article>> {
        Ok(self.articles.lock().await.clone())
    }
}

/// Helper: create an Article with sensible defaults.
fn make_article(id: &str, title: &str, url: Option<&str>, ts: u64) -> Article {
    Article {
        id: id.to_string(),
        title: title.to_string(),
        url: url.map(|u| u.to_string()),
        source_url: format!("https://example.com/{id}"),
        score: 10,
        author: "test-author".to_string(),
        scraped_at: Timestamp::from(ts),
        source: "test".to_string(),
        feed_title: None,
        feed_link: None,
        feed_subtitle: None,
        published_at: Some(Timestamp::from(ts)),
    }
}

struct TestHarness {
    db: BotDatabase,
    publisher: Publisher,
}

impl TestHarness {
    async fn new() -> Self {
        let secret = RostraIdSecretKey::generate();
        let id = secret.id();

        let client = Client::builder(id)
            .secret(secret)
            .start_background_tasks(false)
            .start_request_handler(false)
            .build()
            .await
            .expect("client build should succeed");

        let db = BotDatabase::new(client.db().clone());
        db.init_tables().await.expect("init_tables should succeed");

        let publisher = Publisher::new(client, secret);

        Self { db, publisher }
    }
}

// ── Test: exact ID dedup ─────────────────────────────────────────────

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_exact_id_dedup() {
    let h = TestHarness::new().await;

    let article = make_article("dup-1", "Some Title", Some("https://example.com/a"), NOW);

    // First add succeeds
    assert!(h.db.add_unpublished_article(&article).await.unwrap());

    // Second add with same ID is rejected
    assert!(!h.db.add_unpublished_article(&article).await.unwrap());
}

// ── Test: URL fuzzy dedup ────────────────────────────────────────────

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_url_fuzzy_dedup() {
    let h = TestHarness::new().await;

    // Publish an article with a URL containing tracking params
    let article_a = make_article(
        "url-a",
        "Article A",
        Some("https://www.example.com/article?utm_source=hn"),
        NOW,
    );
    assert!(h.db.add_unpublished_article(&article_a).await.unwrap());
    h.db.mark_article_published(&article_a, Timestamp::from(NOW))
        .await
        .unwrap();

    // Try to add a different ID but same normalized URL
    let article_b = make_article(
        "url-b",
        "Article B",
        Some("https://example.com/article?utm_source=lobsters"),
        NOW,
    );
    // Should be rejected by normalized URL dedup
    assert!(!h.db.add_unpublished_article(&article_b).await.unwrap());
}

// ── Test: URL fuzzy dedup in unpublished queue ───────────────────────

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_url_fuzzy_dedup_unpublished() {
    let h = TestHarness::new().await;

    // Add article to unpublished queue
    let article_a = make_article(
        "url-a",
        "Article A",
        Some("https://www.example.com/page?ref=sidebar"),
        NOW,
    );
    assert!(h.db.add_unpublished_article(&article_a).await.unwrap());

    // Try different ID but same normalized URL (without tracking params)
    let article_b = make_article("url-b", "Article B", Some("https://example.com/page"), NOW);
    assert!(!h.db.add_unpublished_article(&article_b).await.unwrap());
}

// ── Test: title fuzzy dedup ──────────────────────────────────────────

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_title_fuzzy_dedup() {
    let h = TestHarness::new().await;

    // Publish an article with a long title
    let article_a = make_article(
        "title-a",
        "The Rust Programming Language Book",
        Some("https://example.com/rust-book"),
        NOW,
    );
    assert!(h.db.add_unpublished_article(&article_a).await.unwrap());
    h.db.mark_article_published(&article_a, Timestamp::from(NOW))
        .await
        .unwrap();

    // Try a very similar title (differs by one word) with same timestamp window
    // Jaccard: {"the","rust","programming","language","guide","book"} vs
    //          {"the","rust","programming","language","book"}
    // intersection=5, union=6 → 5/6 ≈ 0.833 > 0.8 threshold
    let article_b = make_article(
        "title-b",
        "The Rust Programming Language Guide Book",
        Some("https://other.com/rust-guide"),
        NOW,
    );
    assert!(!h.db.add_unpublished_article(&article_b).await.unwrap());
}

// ── Test: title dedup outside time window ────────────────────────────

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_title_dedup_outside_time_window() {
    let h = TestHarness::new().await;

    // Publish an article with a specific timestamp
    let article_a = make_article(
        "tw-a",
        "The Rust Programming Language Book",
        Some("https://example.com/rust-book"),
        NOW,
    );
    assert!(h.db.add_unpublished_article(&article_a).await.unwrap());
    h.db.mark_article_published(&article_a, Timestamp::from(NOW))
        .await
        .unwrap();

    // Same similar title but timestamp is >30 days apart → NOT caught
    let far_future = NOW + DEDUP_TIME_WINDOW_SECS + 1;
    let article_b = make_article(
        "tw-b",
        "The Rust Programming Language Guide Book",
        Some("https://other.com/rust-guide"),
        far_future,
    );
    // Should be accepted because timestamps are too far apart
    assert!(h.db.add_unpublished_article(&article_b).await.unwrap());
}

// ── Test: short title skips Jaccard ──────────────────────────────────

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_short_title_skips_jaccard() {
    let h = TestHarness::new().await;

    // Publish a short 2-token title
    let article_a = make_article(
        "short-a",
        "Rust News",
        Some("https://example.com/rust-news-1"),
        NOW,
    );
    assert!(h.db.add_unpublished_article(&article_a).await.unwrap());
    h.db.mark_article_published(&article_a, Timestamp::from(NOW))
        .await
        .unwrap();

    // Identical 2-token title, different ID and URL → passes because <3 tokens
    let article_b = make_article(
        "short-b",
        "Rust News",
        Some("https://example.com/rust-news-2"),
        NOW,
    );
    assert!(h.db.add_unpublished_article(&article_b).await.unwrap());
}

// ── Test: full cycle scrape → publish ────────────────────────────────

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_full_cycle_scrape_to_publish() {
    let h = TestHarness::new().await;

    let articles = vec![
        make_article(
            "cycle-1",
            "First Article Title Here",
            Some("https://example.com/1"),
            NOW,
        ),
        make_article(
            "cycle-2",
            "Second Article Title Here",
            Some("https://example.com/2"),
            NOW,
        ),
        make_article(
            "cycle-3",
            "Third Article Title Here",
            Some("https://example.com/3"),
            NOW,
        ),
    ];

    let scrapers: Vec<Box<dyn Scraper + Send + Sync>> = vec![Box::new(MockScraper::new(articles))];

    // Run one cycle: should scrape 3 articles → add to queue → publish all 3
    rostra_bot::run_one_cycle(0, 0, 10, &h.db, &scrapers, &h.publisher)
        .await
        .unwrap();

    // Unpublished queue should be empty (all published)
    let unpublished = h.db.get_unpublished_articles().await.unwrap();
    assert!(
        unpublished.is_empty(),
        "all articles should have been published"
    );

    // Run again with same articles → 0 new additions (exact ID dedup)
    rostra_bot::run_one_cycle(0, 0, 10, &h.db, &scrapers, &h.publisher)
        .await
        .unwrap();

    let unpublished = h.db.get_unpublished_articles().await.unwrap();
    assert!(
        unpublished.is_empty(),
        "no new articles should have been added"
    );
}

// ── Test: full cycle with score filtering ────────────────────────────

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_full_cycle_score_filter() {
    let h = TestHarness::new().await;

    let mut low_score = make_article(
        "low-score",
        "Low Score Article Title",
        Some("https://example.com/low"),
        NOW,
    );
    low_score.source = "hn".to_string();
    low_score.score = 5;

    let mut high_score = make_article(
        "high-score",
        "High Score Article Title",
        Some("https://example.com/high"),
        NOW,
    );
    high_score.source = "hn".to_string();
    high_score.score = 50;

    let scrapers: Vec<Box<dyn Scraper + Send + Sync>> =
        vec![Box::new(MockScraper::new(vec![low_score, high_score]))];

    // hn_min_score = 10, so only the high-score article should pass
    rostra_bot::run_one_cycle(10, 0, 10, &h.db, &scrapers, &h.publisher)
        .await
        .unwrap();

    // Only the high-score article should have been published
    let unpublished = h.db.get_unpublished_articles().await.unwrap();
    assert!(unpublished.is_empty());
}

// ── Test: second cycle with new articles ─────────────────────────────

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_full_cycle_new_articles_second_run() {
    let h = TestHarness::new().await;

    // First cycle
    let scrapers_first: Vec<Box<dyn Scraper + Send + Sync>> =
        vec![Box::new(MockScraper::new(vec![
            make_article(
                "batch1-1",
                "First Batch Article One",
                Some("https://example.com/b1-1"),
                NOW,
            ),
            make_article(
                "batch1-2",
                "First Batch Article Two",
                Some("https://example.com/b1-2"),
                NOW,
            ),
        ]))];

    rostra_bot::run_one_cycle(0, 0, 10, &h.db, &scrapers_first, &h.publisher)
        .await
        .unwrap();

    let unpublished = h.db.get_unpublished_articles().await.unwrap();
    assert!(unpublished.is_empty(), "first batch should be published");

    // Second cycle with entirely new articles
    let scrapers_second: Vec<Box<dyn Scraper + Send + Sync>> =
        vec![Box::new(MockScraper::new(vec![make_article(
            "batch2-1",
            "Second Batch New Article",
            Some("https://example.com/b2-1"),
            NOW,
        )]))];

    rostra_bot::run_one_cycle(0, 0, 10, &h.db, &scrapers_second, &h.publisher)
        .await
        .unwrap();

    let unpublished = h.db.get_unpublished_articles().await.unwrap();
    assert!(unpublished.is_empty(), "second batch should be published");
}
