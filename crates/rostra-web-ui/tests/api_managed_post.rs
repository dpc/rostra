mod common;

use common::TestServer;
use rostra_core::event::{
    Event, EventContentKind as _, EventExt as _, EventKind, SocialPost, VerifiedEvent,
};
use rostra_core::id::{RostraIdSecretKey, ToShort as _};
use rostra_core::{ShortEventId, Timestamp};

fn root(secret: RostraIdSecretKey, time: u64) -> VerifiedEvent {
    let event = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::NULL)
        .timestamp(Timestamp::from(time).to_offset_date_time().unwrap())
        .build()
        .signed_by(secret);
    VerifiedEvent::verify_signed(secret.id(), event).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn managed_post_consumes_supplied_sibling_not_independent_minimum() {
    let secret = RostraIdSecretKey::from_bytes([31; 32]);
    let mut roots = (1..=64).map(|time| root(secret, time)).collect::<Vec<_>>();
    roots.sort_by_key(|event| event.event_id.to_short());
    let (lower, requested) = roots
        .iter()
        .enumerate()
        .find_map(|(index, lower)| {
            roots[index + 1..]
                .iter()
                .find(|higher| {
                    higher.event_id.to_short().to_string() < lower.event_id.to_short().to_string()
                })
                .map(|higher| (*lower, *higher))
        })
        .expect("deterministic fixture has opposite encoded-string and raw-key order");
    let server = TestServer::start().await;
    let driver = server.driver();
    let client = server.client(secret.id()).await;
    client.db().try_process_event(&lower).await.unwrap();
    client.db().try_process_event(&requested).await.unwrap();
    assert_eq!(
        client.db().get_self_current_head().await,
        Some(lower.event_id.to_short())
    );
    let heads: serde_json::Value = driver
        .api_get(&format!("/api/{}/heads", secret.id()))
        .await
        .json()
        .await
        .unwrap();
    let expected = requested.event_id.to_short().to_string();
    assert_eq!(heads["heads"][0].as_str(), Some(expected.as_str()));
    let body =
        serde_json::json!({ "parent_head_id": expected, "content": "consume selected sibling" });
    let response = driver
        .api_post_json(
            &format!("/api/{}/publish-social-post-managed", secret.id()),
            Some(&secret.to_string()),
            &body,
        )
        .await;
    assert_eq!(response.status(), 200);
    let result: serde_json::Value = response.json().await.unwrap();
    let id: ShortEventId = result["event_id"].as_str().unwrap().parse().unwrap();
    let event = client.db().get_event(id).await.unwrap();
    assert_eq!(event.parent_prev(), Some(requested.event_id.to_short()));
    let response = driver
        .api_post_json(
            &format!("/api/{}/publish-social-post-managed", secret.id()),
            Some(&secret.to_string()),
            &body,
        )
        .await;
    assert_eq!(
        response.status(),
        409,
        "the supplied sibling is consumed, not merely observed"
    );
    let posts: serde_json::Value = driver
        .api_get(&format!("/api/{}/posts", secret.id()))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(posts["posts"].as_array().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn managed_post_concurrent_empty_and_existing_parent_publish_once() {
    for existing in [false, true] {
        let secret = RostraIdSecretKey::generate();
        let server = TestServer::start().await;
        let driver = server.driver();
        let client = server.client(secret.id()).await;
        let parent = if existing {
            let parent = root(secret, 1);
            client.db().try_process_event(&parent).await.unwrap();
            Some(parent.event_id.to_short())
        } else {
            None
        };
        let path = format!("/api/{}/publish-social-post-managed", secret.id());
        let secret_text = secret.to_string();
        let one = serde_json::json!({ "parent_head_id": parent.map(|id| id.to_string()), "content": "one" });
        let two = serde_json::json!({ "parent_head_id": parent.map(|id| id.to_string()), "content": "two" });
        let (one, two) = tokio::join!(
            driver.api_post_json(&path, Some(&secret_text), &one),
            driver.api_post_json(&path, Some(&secret_text), &two),
        );
        let (winner, loser) = if one.status() == 200 {
            (one, two)
        } else {
            (two, one)
        };
        assert_eq!(winner.status(), 200);
        assert_eq!(loser.status(), 409);
        let result: serde_json::Value = winner.json().await.unwrap();
        let id: ShortEventId = result["event_id"].as_str().unwrap().parse().unwrap();
        assert_eq!(
            client.db().get_event(id).await.unwrap().parent_prev(),
            parent
        );
        let posts: serde_json::Value = driver
            .api_get(&format!("/api/{}/posts", secret.id()))
            .await
            .json()
            .await
            .unwrap();
        assert_eq!(posts["posts"].as_array().unwrap().len(), 1);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn managed_post_activation_capacity_failure_reports_committed_post() {
    use std::num::{NonZeroU64, NonZeroUsize};
    use std::time::Duration;

    use rostra_client_db::{
        PayloadAccount, PayloadAdmissionConfig, PayloadAdmissionLimits, PayloadRetentionConfig,
        PayloadRuntimeLimits,
    };

    let secret = RostraIdSecretKey::generate();
    let body = "fill the local publication budget";
    let bytes = SocialPost::new(body.to_owned(), None, Default::default())
        .serialize_cbor()
        .unwrap()
        .as_slice()
        .len() as u64;
    let account = PayloadAccount::configured(
        secret.id(),
        PayloadRetentionConfig::Enforce {
            policy: rostra_core::retention::RetentionPolicy::new(1, 1, 0, 0, 1, 0).unwrap(),
            admission: PayloadAdmissionConfig::new(PayloadAdmissionLimits {
                database_bytes: NonZeroU64::new(bytes).unwrap(),
                author_bytes: NonZeroU64::new(bytes).unwrap(),
                overrides: Default::default(),
                in_flight_count: NonZeroUsize::new(8).unwrap(),
                in_flight_bytes: NonZeroU64::new(100_000).unwrap(),
            })
            .unwrap(),
            limits: PayloadRuntimeLimits {
                operations: NonZeroUsize::new(32).unwrap(),
                bytes: 100_000,
                gc_bytes: 100_000,
                time: Duration::from_millis(20),
            },
        },
    )
    .unwrap();
    let server = TestServer::start_with_payload_accounts(vec![account]).await;
    let driver = server.driver();
    let client = server.client(secret.id()).await;
    while !client
        .db()
        .rebuild_payload_accounting(NonZeroUsize::new(100).unwrap())
        .await
        .unwrap()
        .ready
    {}
    let path = format!("/api/{}/publish-social-post-managed", secret.id());
    let input = serde_json::json!({ "parent_head_id": null, "content": body });
    let response = driver
        .api_post_json(&path, Some(&secret.to_string()), &input)
        .await;
    assert_eq!(response.status(), 500);
    let error: serde_json::Value = response.json().await.unwrap();
    assert!(
        error["error"]
            .as_str()
            .unwrap()
            .starts_with("Post committed, but activation failed:")
    );
    let retry = driver
        .api_post_json(&path, Some(&secret.to_string()), &input)
        .await;
    assert_eq!(
        retry.status(),
        409,
        "activation failure does not undo head consumption"
    );
    let posts: serde_json::Value = driver
        .api_get(&format!("/api/{}/posts", secret.id()))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(posts["posts"].as_array().unwrap().len(), 1);
}
