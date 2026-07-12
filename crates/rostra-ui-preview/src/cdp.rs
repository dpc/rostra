use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tungstenite::client::client;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};
use url::Url;

/// Synchronous connection to one Chromium page's DevTools endpoint.
pub struct Cdp {
    /// Page WebSocket.
    socket: WebSocket<MaybeTlsStream<TcpStream>>,
    /// Monotonically increasing request identifier.
    next_id: u64,
}

impl Cdp {
    /// Connect to a page WebSocket and apply bounded I/O timeouts.
    pub fn connect_to(websocket_url: &str) -> Result<Self> {
        let url = Url::parse(websocket_url).context("invalid Chromium DevTools URL")?;
        if url.scheme() != "ws" {
            bail!("Chromium DevTools must use an unencrypted loopback WebSocket");
        }
        let host = url
            .host_str()
            .context("Chromium DevTools URL has no host")?;
        let address: SocketAddr = format!("{host}:{}", url.port_or_known_default().unwrap_or(80))
            .parse()
            .context("Chromium DevTools URL is not a literal IP address")?;
        if !address.ip().is_loopback() {
            bail!("Chromium DevTools URL is not loopback");
        }
        let stream = TcpStream::connect_timeout(&address, Duration::from_secs(3))?;
        stream.set_read_timeout(Some(Duration::from_secs(3)))?;
        stream.set_write_timeout(Some(Duration::from_secs(3)))?;
        let (mut socket, _) = client(websocket_url, MaybeTlsStream::Plain(stream))
            .context("connecting to Chromium DevTools")?;
        if let MaybeTlsStream::Plain(stream) = socket.get_mut() {
            stream.set_read_timeout(Some(Duration::from_secs(15)))?;
            stream.set_write_timeout(Some(Duration::from_secs(15)))?;
        }
        Ok(Self { socket, next_id: 1 })
    }

    /// Invoke a CDP method and return its result object.
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.call_inner(method, params, None)
    }

    /// Invoke a CDP method and wait for a subsequent named lifecycle event.
    pub fn call_and_wait(&mut self, method: &str, params: Value, event: &str) -> Result<Value> {
        self.call_inner(method, params, Some(event))
    }

    /// Send one method and collect its response and optional event in either
    /// order.
    fn call_inner(&mut self, method: &str, params: Value, event: Option<&str>) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.socket.send(Message::Text(
            json!({ "id": id, "method": method, "params": params })
                .to_string()
                .into(),
        ))?;

        let mut result: Option<Value> = None;
        let mut event_seen = event.is_none();
        let mut lifecycle_loaders = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(15);
        while result.is_none() || !event_seen {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .context("DevTools call exceeded its 15-second deadline")?;
            if let MaybeTlsStream::Plain(stream) = self.socket.get_mut() {
                stream.set_read_timeout(Some(remaining))?;
            }
            let message = self.socket.read()?;
            match message {
                Message::Text(text) => {
                    let response: Value =
                        serde_json::from_str(&text).context("invalid DevTools response")?;
                    if response.get("method").and_then(Value::as_str) == event {
                        if event == Some("Page.lifecycleEvent") {
                            if response["params"]["name"] == "load"
                                && let Some(loader) = response["params"]["loaderId"].as_str()
                            {
                                lifecycle_loaders.push(loader.to_owned());
                                event_seen = result
                                    .as_ref()
                                    .and_then(|result| result["loaderId"].as_str())
                                    == Some(loader);
                            }
                        } else {
                            event_seen = true;
                        }
                    }
                    if response.get("id").and_then(Value::as_u64) != Some(id) {
                        continue;
                    }
                    if let Some(error) = response.get("error") {
                        bail!("DevTools method {method} failed: {error}");
                    }
                    let method_result = response
                        .get("result")
                        .cloned()
                        .context("DevTools response has no result")?;
                    if let Some(exception) = method_result.get("exceptionDetails") {
                        bail!("DevTools method {method} raised an exception: {exception}");
                    }
                    result = Some(method_result);
                    if event == Some("Page.lifecycleEvent") {
                        event_seen = result
                            .as_ref()
                            .and_then(|result| result["loaderId"].as_str())
                            .is_none_or(|loader| {
                                lifecycle_loaders.iter().any(|seen| seen == loader)
                            });
                    }
                }
                Message::Ping(payload) => self.socket.send(Message::Pong(payload))?,
                Message::Close(reason) => {
                    bail!("Chromium closed the DevTools connection: {reason:?}")
                }
                _ => {}
            }
            if event == Some("Page.lifecycleEvent")
                && let Some(loader) = result
                    .as_ref()
                    .and_then(|result| result["loaderId"].as_str())
            {
                event_seen = lifecycle_loaders.iter().any(|seen| seen == loader);
            }
        }
        Ok(result.expect("response loop requires a result"))
    }
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::thread;
    use std::time::{Duration, Instant};

    use super::Cdp;

    #[test]
    fn websocket_handshake_has_a_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        thread::spawn(move || {
            let _connection = listener.accept().unwrap();
            thread::sleep(Duration::from_secs(4));
        });

        let start = Instant::now();
        assert!(Cdp::connect_to(&format!("ws://{address}/devtools/page/test")).is_err());
        assert!(start.elapsed() < Duration::from_secs(4));
    }
}
