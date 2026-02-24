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
    _temp_dir: TempDir,
    base_url: String,
}

impl TestServer {
    pub async fn start() -> Self {
        // Use dev mode so assets are served from the source tree
        // (avoids needing compiled/bundled assets).
        // SAFETY: Integration tests run as separate binaries, so no other
        // threads are reading env vars at this point during setup.
        unsafe {
            std::env::set_var("ROSTRA_DEV_MODE", "1");
        }

        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let data_dir = temp_dir.path().to_path_buf();

        let pkarr_client = Client::make_pkarr_client().expect("Failed to create pkarr client");
        let clients = MultiClient::new(data_dir.clone(), 10, false, pkarr_client);

        let opts = Opts::new(
            rostra_util_bind_addr::BindAddr::Tcp(SocketAddr::from(([127, 0, 0, 1], 0))),
            None,  // origin
            None,  // assets_dir (uses default)
            false, // reuseport
            data_dir,
            None, // default_profile
            10,   // max_clients
            None, // welcome_redirect
        );

        let server = rostra_web_ui::start_ui(opts, clients)
            .await
            .expect("Failed to start test server");

        let base_url = format!("http://{}", server.local_addr());

        Self {
            server,
            _temp_dir: temp_dir,
            base_url,
        }
    }

    /// Create a new `UiDriver` with its own cookie jar (independent session).
    pub fn driver(&self) -> UiDriver {
        UiDriver::new(self.base_url.clone())
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
}
