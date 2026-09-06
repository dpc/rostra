use std::num::NonZeroUsize;
use std::time::Duration;

use pkarr::{Cache as _, CacheKey, Keypair, SignedPacket, Timestamp};

use super::{PkarrClient, mozilla_root_http_client};

fn unavailable_relay_builder(url: &str) -> pkarr::ClientBuilder {
    let mut builder = pkarr::Client::builder();
    builder
        .no_default_network()
        .request_timeout(Duration::from_secs(1))
        .relays(&[url])
        .unwrap()
        .reqwest_client(mozilla_root_http_client().unwrap());
    builder
}

#[tokio::test]
async fn reversed_ttl_bounds_use_upstream_normalization() {
    let unavailable_backend = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend_url = format!("http://{}", unavailable_backend.local_addr().unwrap());
    let client = PkarrClient::build(
        unavailable_relay_builder(&backend_url),
        NonZeroUsize::new(1).unwrap(),
        60,
        30,
    )
    .unwrap();
    let keypair = Keypair::random();
    let mut packet = SignedPacket::builder().sign(&keypair).unwrap();
    packet.set_last_seen(&(Timestamp::now() - 45 * 1_000_000_u64));
    client
        .cache
        .put(&CacheKey::from(&keypair.public_key()), &packet);

    let resolved = client.resolve(&keypair.public_key()).await.unwrap();

    assert_eq!(resolved, packet);
    assert_eq!(client.minimum_ttl, 30);
    assert_eq!(client.maximum_ttl, 30);
}

#[tokio::test]
async fn expired_cached_packet_remains_available_during_backend_outage() {
    let unavailable_backend = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend_url = format!("http://{}", unavailable_backend.local_addr().unwrap());
    let client = PkarrClient::build(
        unavailable_relay_builder(&backend_url),
        NonZeroUsize::new(1).unwrap(),
        0,
        30,
    )
    .unwrap();
    let keypair = Keypair::random();
    let mut packet = SignedPacket::builder().sign(&keypair).unwrap();
    packet.set_last_seen(&(Timestamp::now() - 60 * 1_000_000_u64));
    client
        .cache
        .put(&CacheKey::from(&keypair.public_key()), &packet);

    let resolved = tokio::time::timeout(
        Duration::from_millis(100),
        client.resolve(&keypair.public_key()),
    )
    .await
    .expect("cached resolution should not wait for an unavailable backend")
    .unwrap();

    assert_eq!(resolved, packet);
}

#[tokio::test]
async fn cache_miss_starts_normal_resolution_without_cache_only_delay() {
    let unavailable_backend = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend_url = format!("http://{}", unavailable_backend.local_addr().unwrap());
    let client = PkarrClient::build(
        unavailable_relay_builder(&backend_url),
        NonZeroUsize::new(1).unwrap(),
        0,
        30,
    )
    .unwrap();
    let keypair = Keypair::random();

    let result = tokio::time::timeout(
        Duration::from_millis(1500),
        client.resolve(&keypair.public_key()),
    )
    .await
    .expect("a cache miss should perform one relay-timeout period");

    assert!(
        result.is_err(),
        "an unavailable relay should fail normal resolution"
    );
}
