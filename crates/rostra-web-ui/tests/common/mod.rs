#![allow(dead_code)]

use std::net::SocketAddr;

use rostra_client::Client;
use rostra_client::multiclient::MultiClient;
use rostra_core::id::{RostraId, RostraIdSecretKey};
use rostra_web_ui::{Opts, UiServer};
use tempfile::TempDir;

/// A test web UI server running on a random port with ephemeral storage.
pub struct TestServer {
    server: UiServer,
    clients: MultiClient,
    _temp_dir: TempDir,
    base_url: String,
}

impl TestServer {
    /// Inspect loaded clients without creating/opening an identity database.
    pub async fn is_client_loaded(&self, id: RostraId) -> bool {
        self.clients.get(id).await.is_some()
    }

    pub async fn start() -> Self {
        Self::start_on(SocketAddr::from(([127, 0, 0, 1], 0)), None, None).await
    }

    /// Start with immutable pre-open ownership for explicitly listed accounts.
    pub async fn start_with_payload_accounts(
        accounts: Vec<rostra_client_db::PayloadAccount>,
    ) -> Self {
        Self::start_on_with_payload_accounts(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            None,
            None,
            accounts,
        )
        .await
    }

    /// Return the loopback listener for controlled streaming HTTP tests.
    pub fn address(&self) -> SocketAddr {
        self.server.local_addr()
    }

    /// Check the current-format account path without filesystem side effects.
    pub fn has_database_file(&self, id: RostraId) -> bool {
        self._temp_dir.path().join(format!("{id}.redb")).exists()
    }

    /// Start a test server that renders its default-profile landing actions.
    pub async fn start_with_default_profile(default_profile: RostraId) -> Self {
        Self::start_on(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            None,
            Some(default_profile),
        )
        .await
    }

    /// Start an HTTP server on a non-loopback bind address.
    pub async fn start_non_loopback_http() -> Self {
        Self::start_on(SocketAddr::from(([0, 0, 0, 0], 0)), None, None).await
    }

    /// Start a loopback server configured with a public plaintext origin.
    pub async fn start_public_http_origin() -> Self {
        Self::start_on(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            Some("http://public.example".to_string()),
            None,
        )
        .await
    }

    /// Start a loopback server configured with a loopback HTTPS origin.
    pub async fn start_loopback_https_origin() -> Self {
        Self::start_on(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            Some("https://localhost".to_string()),
            None,
        )
        .await
    }

    async fn start_on(
        listen: SocketAddr,
        origin: Option<String>,
        default_profile: Option<RostraId>,
    ) -> Self {
        Self::start_on_with_payload_accounts(listen, origin, default_profile, vec![]).await
    }

    async fn start_on_with_payload_accounts(
        listen: SocketAddr,
        origin: Option<String>,
        default_profile: Option<RostraId>,
        accounts: Vec<rostra_client_db::PayloadAccount>,
    ) -> Self {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let data_dir = temp_dir.path().to_path_buf();

        let pkarr_client = Client::make_pkarr_client().expect("Failed to create pkarr client");
        let clients = MultiClient::new_with_payload_accounts(
            data_dir.clone(),
            10,
            false,
            pkarr_client,
            accounts,
        )
        .expect("unique account configuration");

        let opts = Opts::new(
            rostra_util_bind_addr::BindAddr::Tcp(listen),
            origin,
            None,  // assets_dir (uses default)
            false, // reuseport
            data_dir,
            default_profile,
            10,   // max_clients
            None, // welcome_redirect
        )
        .with_source_assets();

        let server = rostra_web_ui::start_ui(opts, clients.clone())
            .await
            .expect("Failed to start test server");

        let server_addr = server.local_addr();
        let connect_ip = if server_addr.ip().is_unspecified() {
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
        } else {
            server_addr.ip()
        };
        let base_url = format!("http://{}:{}", connect_ip, server_addr.port());

        Self {
            server,
            clients,
            _temp_dir: temp_dir,
            base_url,
        }
    }

    /// Create a new `UiDriver` with its own cookie jar (independent session).
    pub fn driver(&self) -> UiDriver {
        UiDriver::new(self.base_url.clone())
    }

    /// Return an in-process handle to a test identity's client.
    pub async fn client(&self, id: RostraId) -> std::sync::Arc<Client> {
        self.clients.load(id).await.expect("load test client")
    }

    /// Shut down the server cleanly.
    pub async fn shutdown(self) {
        self.server
            .shutdown()
            .await
            .expect("Server shutdown failed");
    }
}

/// HTTP client driver for interacting with the web UI in tests.
///
/// Each `UiDriver` maintains its own cookie jar, so it represents
/// an independent browser session.
pub struct UiDriver {
    client: reqwest::Client,
    base_url: String,
}

impl UiDriver {
    /// Upgrade the current cookie-authenticated session to the UI update
    /// stream.
    pub async fn updates_websocket(&self) -> tokio_tungstenite::WebSocketStream<reqwest::Upgraded> {
        let response = self
            .client
            .get(self.url("/updates"))
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::SWITCHING_PROTOCOLS);
        tokio_tungstenite::WebSocketStream::from_raw_socket(
            response.upgrade().await.unwrap(),
            tokio_tungstenite::tungstenite::protocol::Role::Client,
            None,
        )
        .await
    }

    fn new(base_url: String) -> Self {
        let client = reqwest::Client::builder()
            .cookie_store(true)
            // Don't auto-follow redirects — let tests assert on redirect targets.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("Failed to build HTTP client");

        Self { client, base_url }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    /// Generate a new identity and log in with full read-write access.
    ///
    /// Returns the `(RostraId, RostraIdSecretKey)` for the new identity.
    pub async fn login_new_identity(&self) -> (RostraId, RostraIdSecretKey) {
        let secret = RostraIdSecretKey::generate();
        let id = secret.id();

        let resp = self
            .client
            .post(self.url("/unlock"))
            .form(&[
                ("username", id.to_string()),
                ("password", secret.to_string()),
            ])
            .send()
            .await
            .expect("Login request failed");

        assert_eq!(
            resp.status(),
            reqwest::StatusCode::SEE_OTHER,
            "Expected redirect after login, got {}",
            resp.status()
        );

        let location = resp
            .headers()
            .get("location")
            .expect("Missing Location header on login redirect")
            .to_str()
            .expect("Invalid Location header");
        assert_eq!(location, "/");

        (id, secret)
    }

    /// Log in with an existing identity using its secret key.
    pub async fn login_with_secret(&self, id: &str, secret: &str) {
        let resp = self
            .client
            .post(self.url("/unlock"))
            .form(&[("username", id), ("password", secret)])
            .send()
            .await
            .expect("Login request failed");

        assert_eq!(
            resp.status(),
            reqwest::StatusCode::SEE_OTHER,
            "Expected redirect after login, got {}",
            resp.status()
        );
    }

    /// Log in with an existing identity in read-only mode (no secret key).
    pub async fn login_readonly(&self, id: RostraId) {
        let resp = self
            .client
            .post(self.url("/unlock"))
            .form(&[("username", id.to_string()), ("password", String::new())])
            .send()
            .await
            .expect("Read-only login request failed");

        assert_eq!(
            resp.status(),
            reqwest::StatusCode::SEE_OTHER,
            "Expected redirect after read-only login"
        );
    }

    /// Send a GET request to the given path.
    pub async fn get(&self, path: &str) -> reqwest::Response {
        self.client
            .get(self.url(path))
            .send()
            .await
            .expect("GET request failed")
    }

    /// Send a HEAD request to the given path.
    pub async fn head(&self, path: &str) -> reqwest::Response {
        self.client
            .head(self.url(path))
            .send()
            .await
            .expect("HEAD request failed")
    }

    /// Send a GET with an explicitly supplied Cookie header.
    pub async fn get_with_cookie(&self, path: &str, cookie: &str) -> reqwest::Response {
        self.client
            .get(self.url(path))
            .header("Cookie", cookie)
            .send()
            .await
            .expect("GET request failed")
    }

    /// Send a GET request with an `If-None-Match` header (for ETag validation).
    pub async fn get_if_none_match(&self, path: &str, etag: &str) -> reqwest::Response {
        self.client
            .get(self.url(path))
            .header("If-None-Match", etag)
            .send()
            .await
            .expect("GET request failed")
    }

    /// Send a GET request with the `X-Alpine-Request` header (simulates AJAX).
    pub async fn ajax_get(&self, path: &str) -> reqwest::Response {
        self.client
            .get(self.url(path))
            .header("X-Alpine-Request", "true")
            .send()
            .await
            .expect("AJAX GET request failed")
    }

    /// Send a form POST with the Alpine request header.
    pub async fn ajax_post_form(&self, path: &str, form: &[(&str, &str)]) -> reqwest::Response {
        self.client
            .post(self.url(path))
            .header("X-Alpine-Request", "true")
            .form(form)
            .send()
            .await
            .expect("AJAX form POST request failed")
    }

    /// Send a form POST to the given path.
    pub async fn post_form(&self, path: &str, form: &[(&str, &str)]) -> reqwest::Response {
        self.client
            .post(self.url(path))
            .form(form)
            .send()
            .await
            .expect("POST request failed")
    }

    /// Create a new post.
    pub async fn post_new(&self, content: &str) -> reqwest::Response {
        self.post_form("/post", &[("content", content)]).await
    }

    /// Preview a post via the preview dialog endpoint.
    pub async fn preview_post(&self, content: &str) -> reqwest::Response {
        self.post_form("/post/preview_dialog", &[("content", content)])
            .await
    }

    /// Send a GET to an API endpoint with the version header.
    pub async fn api_get(&self, path: &str) -> reqwest::Response {
        self.client
            .get(self.url(path))
            .header("x-rostra-api-version", "0")
            .send()
            .await
            .expect("API GET request failed")
    }

    /// Send a GET without the version header (for negative tests).
    pub async fn api_get_no_version(&self, path: &str) -> reqwest::Response {
        self.client
            .get(self.url(path))
            .send()
            .await
            .expect("API GET request failed")
    }

    /// Send a POST to an API endpoint with JSON body, version header,
    /// and optional secret header.
    pub async fn api_post_json(
        &self,
        path: &str,
        secret: Option<&str>,
        body: &serde_json::Value,
    ) -> reqwest::Response {
        let mut req = self
            .client
            .post(self.url(path))
            .header("x-rostra-api-version", "0")
            .json(body);
        if let Some(s) = secret {
            req = req.header("x-rostra-id-secret", s);
        }
        req.send().await.expect("API POST request failed")
    }

    /// Send a GET to an API endpoint with a custom version header value.
    pub async fn api_get_with_version(&self, path: &str, version: &str) -> reqwest::Response {
        self.client
            .get(self.url(path))
            .header("x-rostra-api-version", version)
            .send()
            .await
            .expect("API GET request failed")
    }
}
