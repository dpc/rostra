use std::time::Duration;

use rostra_client_db::Database;
use rostra_core::ShortEventId;
use rostra_core::id::RostraIdSecretKey;
use tracing::Instrument as _;
use tracing::instrument::WithSubscriber as _;

use super::{PkarrIdPublisher, log_publisher_transition, publish_attempt};
use crate::Client;
use crate::task::diagnostic_test::{EventCapture, assert_no_forbidden_fields};

#[tokio::test]
async fn controlled_publish_attempts_record_success_and_failure_without_sensitive_fields() {
    let capture = EventCapture::default();
    let success: Result<(), &'static str> = async {
        let identity_span = tracing::info_span!("test-identity", self_id = "forbidden");
        publish_attempt(7, Some(ShortEventId::ZERO), async { Ok(()) })
            .instrument(identity_span)
            .await
    }
    .with_subscriber(capture.subscriber())
    .await;
    assert_eq!(success, Ok(()));

    let failure: Result<(), &'static str> = async {
        let identity_span = tracing::info_span!("test-identity", self_id = "forbidden");
        publish_attempt(8, None, async { Err("raw-sensitive-error") })
            .instrument(identity_span)
            .await
    }
    .with_subscriber(capture.subscriber())
    .await;
    assert_eq!(failure, Err("raw-sensitive-error"));

    let attempts = capture.events("Pkarr publish attempt completed");
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0].target, "rostra::id-publish");
    assert_eq!(
        attempts[0].fields.get("attempt").map(String::as_str),
        Some("7")
    );
    assert_eq!(
        attempts[0].fields.get("outcome").map(String::as_str),
        Some("success")
    );
    assert_eq!(
        attempts[1].fields.get("attempt").map(String::as_str),
        Some("8")
    );
    assert_eq!(
        attempts[1].fields.get("outcome").map(String::as_str),
        Some("failure")
    );
    assert!(
        attempts
            .iter()
            .all(|event| event.fields.contains_key("head"))
    );
    assert!(
        attempts
            .iter()
            .all(|event| event.fields.contains_key("elapsed_ms"))
    );
    assert!(attempts.iter().all(|event| {
        !event.message.contains("raw-sensitive-error")
            && event
                .fields
                .values()
                .all(|value| !value.contains("raw-sensitive-error"))
    }));
    for attempt in &attempts {
        assert_no_forbidden_fields(attempt);
    }
    let identity_spans = capture.spans("test-identity");
    assert_eq!(identity_spans.len(), 2);
    assert!(
        identity_spans
            .iter()
            .all(|span| span.fields.contains_key("self_id"))
    );
}

#[test]
fn leader_wait_transition_uses_diagnostic_target_and_elapsed_time() {
    let capture = EventCapture::default();
    tracing::subscriber::with_default(capture.subscriber(), || {
        let span = tracing::info_span!("test-identity", self_id = "forbidden");
        span.in_scope(|| log_publisher_transition(Duration::from_millis(321)));
    });

    let transitions = capture.events("Leader wait completed; assuming Pkarr publisher role");
    assert_eq!(transitions.len(), 1);
    assert_eq!(transitions[0].target, "rostra::id-publish");
    assert_eq!(
        transitions[0].fields.get("elapsed_ms").map(String::as_str),
        Some("321")
    );
    assert_no_forbidden_fields(&transitions[0]);
}

#[tokio::test(flavor = "multi_thread")]
async fn publisher_run_span_uses_id_publish_target() {
    let id_secret = RostraIdSecretKey::generate();
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .bind()
        .await
        .expect("publisher endpoint");
    let client = Client::builder(id_secret.id())
        .db(Database::new_in_memory(id_secret.id())
            .await
            .expect("publisher database"))
        .iroh_endpoint(endpoint)
        .start_request_handler(false)
        .start_background_tasks(false)
        .build()
        .await
        .expect("publisher client");
    let capture = EventCapture::default();
    let worker = tokio::spawn(
        PkarrIdPublisher::new(&client, id_secret)
            .run()
            .with_subscriber(capture.subscriber()),
    );
    tokio::time::timeout(Duration::from_secs(1), async {
        while capture.spans("pkarr-id-publisher").is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("publisher run span");

    let spans = capture.spans("pkarr-id-publisher");
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].target, "rostra::id-publish");
    assert!(spans[0].fields.contains_key("self_id"));
    worker.abort();
    assert!(
        worker
            .await
            .expect_err("worker is cancelled")
            .is_cancelled()
    );
}
