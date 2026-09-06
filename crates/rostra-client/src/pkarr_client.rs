use std::num::NonZeroUsize;
use std::sync::Arc;

use pkarr::{Cache as _, CacheKey, InMemoryCache, ResolvePolicy, SignedPacket};

/// Build the relay HTTP client with the compiled Mozilla root set.
pub(crate) fn mozilla_root_http_client() -> Result<reqwest_13::Client, reqwest_13::Error> {
    reqwest_13::Client::builder()
        .tls_certs_only(
            webpki_root_certs::TLS_SERVER_ROOT_CERTS
                .iter()
                .map(|cert| reqwest_13::tls::Certificate::from_der(cert.as_ref()))
                .collect::<Result<Vec<_>, _>>()?,
        )
        .build()
}

/// Pkarr client with a locally inspectable cache for compatibility resolution.
#[derive(Debug)]
pub struct PkarrClient {
    /// Upstream network client using the same cache.
    client: pkarr::Client,
    /// Shared packet cache used for immediate local lookups.
    cache: Arc<InMemoryCache>,
    /// Minimum TTL applied by the upstream client.
    minimum_ttl: u32,
    /// Maximum TTL applied by the upstream client.
    maximum_ttl: u32,
}

impl PkarrClient {
    /// Build a Pkarr client with a shared local cache and explicit TTL bounds.
    pub fn build(
        mut builder: pkarr::ClientBuilder,
        cache_capacity: NonZeroUsize,
        minimum_ttl: u32,
        maximum_ttl: u32,
    ) -> Result<Self, pkarr::errors::BuildError> {
        let cache = Arc::new(InMemoryCache::new(cache_capacity));
        let minimum_ttl = minimum_ttl.min(maximum_ttl);
        let client = builder
            .cache(cache.clone())
            .minimum_ttl(minimum_ttl)
            .maximum_ttl(maximum_ttl)
            .build()?;

        Ok(Self {
            client,
            cache,
            minimum_ttl,
            maximum_ttl,
        })
    }

    /// Resolve a packet while preserving Pkarr 5's stale-cache fallback
    /// behavior.
    pub(crate) async fn resolve(
        &self,
        public_key: &pkarr::PublicKey,
    ) -> Result<SignedPacket, pkarr::errors::ResolveError> {
        if let Some(packet) = self.cache.get(&CacheKey::from(public_key)) {
            if packet.is_expired(self.minimum_ttl, self.maximum_ttl) {
                let client = self.client.clone();
                let public_key = public_key.clone();
                tokio::spawn(async move {
                    let _ = client.resolve(&public_key, ResolvePolicy::CacheFirst).await;
                });
            }
            return Ok(packet);
        }

        self.client
            .resolve(public_key, ResolvePolicy::CacheFirst)
            .await
    }

    /// Publish a signed packet through the upstream client.
    pub(crate) async fn publish(
        &self,
        packet: &SignedPacket,
    ) -> Result<pkarr::StoredNodeCount, pkarr::errors::PublishError> {
        self.client.publish(packet).await
    }
}

#[cfg(test)]
mod tests;
