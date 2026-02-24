use std::collections::BTreeSet;

use rostra_core::Timestamp;

use crate::tables::Article;

/// Minimum Jaccard similarity for two titles to be considered duplicates.
pub const TITLE_SIMILARITY_THRESHOLD: f64 = 0.8;

/// Skip Jaccard comparison for titles with fewer tokens than this.
pub const MIN_TITLE_TOKENS: usize = 3;

/// Two articles are only considered title-duplicates if their timestamps
/// are within this window (~1 month).
pub const DEDUP_TIME_WINDOW_SECS: u64 = 30 * 24 * 60 * 60;

/// Tracking query parameters to strip during URL normalization.
const TRACKING_PARAMS: &[&str] = &[
    "utm_source",
    "utm_medium",
    "utm_campaign",
    "utm_term",
    "utm_content",
    "ref",
    "fbclid",
    "gclid",
];

/// Normalize a URL for dedup comparison.
///
/// Strips scheme, `www.` prefix, trailing `/`, tracking query params,
/// sorts remaining params, and strips fragment.
/// Returns `host/path[?sorted_params]`.
pub fn normalize_url(raw: &str) -> Option<String> {
    let parsed = url::Url::parse(raw).ok()?;

    let host = parsed.host_str()?;
    let host = host.strip_prefix("www.").unwrap_or(host);

    let path = parsed.path().trim_end_matches('/');

    // Filter out tracking params and sort the rest
    let mut params: Vec<(String, String)> = parsed
        .query_pairs()
        .filter(|(key, _)| {
            let k = key.as_ref();
            !TRACKING_PARAMS.contains(&k) && !k.starts_with("utm_")
        })
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    params.sort();

    let mut result = format!("{host}{path}");
    if !params.is_empty() {
        let qs: Vec<String> = params
            .iter()
            .map(|(k, v)| {
                if v.is_empty() {
                    k.clone()
                } else {
                    format!("{k}={v}")
                }
            })
            .collect();
        result.push('?');
        result.push_str(&qs.join("&"));
    }

    Some(result)
}

/// Normalize a title for fuzzy comparison.
///
/// Lowercases, replaces non-alphanumeric/non-whitespace with space,
/// and collapses whitespace.
pub fn normalize_title(title: &str) -> String {
    let lowered = title.to_lowercase();
    let replaced: String = lowered
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c.is_whitespace() {
                c
            } else {
                ' '
            }
        })
        .collect();
    replaced.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Split a normalized title into a set of tokens.
pub fn title_tokens(normalized: &str) -> BTreeSet<&str> {
    normalized.split_whitespace().collect()
}

/// Compute Jaccard similarity between two token sets.
pub fn jaccard_similarity(a: &BTreeSet<&str>, b: &BTreeSet<&str>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let intersection = a.intersection(b).count();
    let union = a.union(b).count();
    if union == 0 {
        return 0.0;
    }
    intersection as f64 / union as f64
}

/// Check whether two timestamps are within `DEDUP_TIME_WINDOW_SECS` of each
/// other.
pub fn articles_are_close_in_time(a: Timestamp, b: Timestamp) -> bool {
    // secs_since is saturating: returns 0 when b > a
    let diff = a.secs_since(b).max(b.secs_since(a));
    diff < DEDUP_TIME_WINDOW_SECS
}

/// Return the best timestamp for an article: `published_at` if available,
/// otherwise `scraped_at`.
pub fn article_timestamp(article: &Article) -> Timestamp {
    article.published_at.unwrap_or(article.scraped_at)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── normalize_url ───────────────────────────────────────────────

    #[test]
    fn normalize_url_strips_scheme_and_www() {
        assert_eq!(
            normalize_url("https://www.example.com/page"),
            Some("example.com/page".into())
        );
        assert_eq!(
            normalize_url("http://example.com/page"),
            Some("example.com/page".into())
        );
    }

    #[test]
    fn normalize_url_strips_trailing_slash() {
        assert_eq!(
            normalize_url("https://example.com/page/"),
            Some("example.com/page".into())
        );
        assert_eq!(
            normalize_url("https://example.com/"),
            Some("example.com".into())
        );
    }

    #[test]
    fn normalize_url_strips_fragment() {
        assert_eq!(
            normalize_url("https://example.com/page#section"),
            Some("example.com/page".into())
        );
    }

    #[test]
    fn normalize_url_strips_tracking_params() {
        assert_eq!(
            normalize_url("https://example.com/page?utm_source=twitter&id=42"),
            Some("example.com/page?id=42".into())
        );
        assert_eq!(
            normalize_url("https://example.com/page?fbclid=abc&gclid=xyz"),
            Some("example.com/page".into())
        );
        assert_eq!(
            normalize_url("https://example.com/page?ref=homepage&utm_medium=email"),
            Some("example.com/page".into())
        );
    }

    #[test]
    fn normalize_url_sorts_params() {
        assert_eq!(
            normalize_url("https://example.com/page?z=1&a=2"),
            Some("example.com/page?a=2&z=1".into())
        );
    }

    #[test]
    fn normalize_url_invalid_returns_none() {
        assert_eq!(normalize_url("not a url"), None);
        assert_eq!(normalize_url(""), None);
    }

    #[test]
    fn normalize_url_preserves_path_case() {
        // URL paths are case-sensitive per spec
        assert_eq!(
            normalize_url("https://example.com/CaseSensitive"),
            Some("example.com/CaseSensitive".into())
        );
    }

    #[test]
    fn normalize_url_same_article_different_tracking() {
        let a = normalize_url("https://example.com/article?utm_source=hn");
        let b = normalize_url("https://www.example.com/article?utm_source=lobsters&ref=sidebar");
        assert_eq!(a, b);
    }

    // ── normalize_title ─────────────────────────────────────────────

    #[test]
    fn normalize_title_lowercases_and_strips_punctuation() {
        assert_eq!(
            normalize_title("Hello, World! — It's a Test."),
            "hello world it s a test"
        );
    }

    #[test]
    fn normalize_title_collapses_whitespace() {
        assert_eq!(normalize_title("  foo   bar  baz  "), "foo bar baz");
    }

    #[test]
    fn normalize_title_empty() {
        assert_eq!(normalize_title(""), "");
    }

    // ── title_tokens ────────────────────────────────────────────────

    #[test]
    fn title_tokens_basic() {
        let tokens = title_tokens("foo bar baz");
        assert_eq!(tokens.len(), 3);
        assert!(tokens.contains("foo"));
        assert!(tokens.contains("bar"));
        assert!(tokens.contains("baz"));
    }

    #[test]
    fn title_tokens_deduplicates() {
        let tokens = title_tokens("foo foo bar");
        assert_eq!(tokens.len(), 2);
    }

    // ── jaccard_similarity ──────────────────────────────────────────

    #[test]
    fn jaccard_identical() {
        let a: BTreeSet<&str> = ["foo", "bar", "baz"].into();
        assert!((jaccard_similarity(&a, &a) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn jaccard_disjoint() {
        let a: BTreeSet<&str> = ["foo", "bar"].into();
        let b: BTreeSet<&str> = ["baz", "qux"].into();
        assert!(jaccard_similarity(&a, &b).abs() < f64::EPSILON);
    }

    #[test]
    fn jaccard_partial_overlap() {
        // {a, b, c} vs {b, c, d} → intersection=2, union=4 → 0.5
        let a: BTreeSet<&str> = ["a", "b", "c"].into();
        let b: BTreeSet<&str> = ["b", "c", "d"].into();
        assert!((jaccard_similarity(&a, &b) - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn jaccard_both_empty() {
        let a: BTreeSet<&str> = BTreeSet::new();
        let b: BTreeSet<&str> = BTreeSet::new();
        assert!((jaccard_similarity(&a, &b) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn jaccard_above_threshold() {
        // 5 out of 6 tokens match → 5/6 ≈ 0.833 > 0.8
        let a: BTreeSet<&str> = ["the", "rust", "programming", "language", "book"].into();
        let b: BTreeSet<&str> = ["the", "rust", "programming", "language", "guide", "book"].into();
        let sim = jaccard_similarity(&a, &b);
        assert!(
            TITLE_SIMILARITY_THRESHOLD < sim,
            "expected above threshold, got {sim}"
        );
    }

    #[test]
    fn jaccard_below_threshold() {
        // 2 out of 6 tokens match → 2/6 ≈ 0.333 < 0.8
        let a: BTreeSet<&str> = ["rust", "is", "great", "for"].into();
        let b: BTreeSet<&str> = ["python", "is", "great", "too"].into();
        let sim = jaccard_similarity(&a, &b);
        assert!(
            sim < TITLE_SIMILARITY_THRESHOLD,
            "expected below threshold, got {sim}"
        );
    }

    // ── articles_are_close_in_time ──────────────────────────────────

    #[test]
    fn close_in_time_same_timestamp() {
        let t = Timestamp::from(1_000_000u64);
        assert!(articles_are_close_in_time(t, t));
    }

    #[test]
    fn close_in_time_within_window() {
        let a = Timestamp::from(1_000_000u64);
        let b = Timestamp::from(1_000_000u64 + DEDUP_TIME_WINDOW_SECS - 1);
        assert!(articles_are_close_in_time(a, b));
        assert!(articles_are_close_in_time(b, a));
    }

    #[test]
    fn close_in_time_exactly_at_boundary() {
        let a = Timestamp::from(1_000_000u64);
        let b = Timestamp::from(1_000_000u64 + DEDUP_TIME_WINDOW_SECS);
        // Equal to window ⇒ NOT close (strict <)
        assert!(!articles_are_close_in_time(a, b));
    }

    #[test]
    fn close_in_time_outside_window() {
        let a = Timestamp::from(1_000_000u64);
        let b = Timestamp::from(1_000_000u64 + DEDUP_TIME_WINDOW_SECS + 1);
        assert!(!articles_are_close_in_time(a, b));
        assert!(!articles_are_close_in_time(b, a));
    }

    // ── article_timestamp ───────────────────────────────────────────

    #[test]
    fn article_timestamp_prefers_published_at() {
        let article = Article {
            id: "test".into(),
            title: "Test".into(),
            url: None,
            source_url: "https://example.com".into(),
            score: 0,
            author: "author".into(),
            scraped_at: Timestamp::from(100u64),
            source: "test".into(),
            feed_title: None,
            feed_link: None,
            feed_subtitle: None,
            published_at: Some(Timestamp::from(200u64)),
        };
        assert_eq!(article_timestamp(&article), Timestamp::from(200u64));
    }

    #[test]
    fn article_timestamp_falls_back_to_scraped_at() {
        let article = Article {
            id: "test".into(),
            title: "Test".into(),
            url: None,
            source_url: "https://example.com".into(),
            score: 0,
            author: "author".into(),
            scraped_at: Timestamp::from(100u64),
            source: "test".into(),
            feed_title: None,
            feed_link: None,
            feed_subtitle: None,
            published_at: None,
        };
        assert_eq!(article_timestamp(&article), Timestamp::from(100u64));
    }
}
