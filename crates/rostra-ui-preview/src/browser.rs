use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;
use std::{fs, thread};

use anyhow::{Context, Result, bail};
use data_encoding::BASE64;
use serde_json::{Value, json};
use tempfile::TempDir;

use crate::cdp::Cdp;
use crate::command::ScrollDirection;
use crate::endpoint::SiteOrigin;

/// Owned isolated Chromium process and its page-level DevTools client.
pub struct Browser {
    /// Chromium process and profile cleanup owner.
    _process: BrowserProcess,
    /// Page DevTools connection.
    cdp: Cdp,
    /// Only origin that navigation commands may target.
    origin: SiteOrigin,
}

impl Browser {
    /// Launch Chromium with a fresh profile and connect to its initial page.
    pub fn launch(
        executable: &Path,
        origin: &SiteOrigin,
        width: u32,
        height: u32,
        headed: bool,
    ) -> Result<Self> {
        let profile = tempfile::Builder::new()
            .prefix("rostra-ui-preview-")
            .tempdir()
            .context("creating isolated Chromium profile")?;
        let browser_profile = profile.path().join("profile");
        let mut command = Command::new(executable);
        command
            .arg("--remote-debugging-address=127.0.0.1")
            .arg("--remote-debugging-port=0")
            .arg(format!("--user-data-dir={}", browser_profile.display()))
            .arg(format!("--window-size={width},{height}"))
            .args([
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-background-networking",
                "--disable-component-update",
                "--disable-default-apps",
                "--disable-extensions",
                "--disable-sync",
                "--metrics-recording-only",
                "about:blank",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        if !headed {
            command.arg("--headless=new");
        }

        let child = command
            .spawn()
            .with_context(|| format!("launching Chromium at {}", executable.display()))?;
        let mut process = BrowserProcess {
            child,
            profile: Some(profile),
        };
        let stderr = process
            .child
            .stderr
            .take()
            .context("capturing Chromium stderr")?;
        let port = wait_for_debugging_port(stderr)?;
        let websocket_url =
            find_page_websocket(port).context("discovering Chromium's initial page")?;
        let mut cdp = Cdp::connect_to(&websocket_url)?;
        cdp.call(
            "Emulation.setDeviceMetricsOverride",
            json!({
                "width": width,
                "height": height,
                "deviceScaleFactor": 1,
                "mobile": false,
            }),
        )
        .context("setting the Chromium viewport")?;

        Ok(Self {
            _process: process,
            cdp,
            origin: origin.clone(),
        })
    }

    /// Navigate to a same-origin target and wait until it is visually ready.
    pub fn open(&mut self, target: &str) -> Result<()> {
        let url = self.origin.target(target)?;
        self.cdp
            .call("Page.enable", json!({}))
            .context("enabling page inspection")?;
        self.cdp
            .call("Page.setLifecycleEventsEnabled", json!({ "enabled": true }))?;
        self.cdp
            .call_and_wait(
                "Page.navigate",
                json!({ "url": url.as_str() }),
                "Page.lifecycleEvent",
            )
            .context("navigating Chromium")?;
        self.ready()?;
        self.ensure_same_origin()?;
        let title = self.evaluate("document.title")?;
        if !title
            .get("result")
            .and_then(|result| result.get("value"))
            .and_then(Value::as_str)
            .is_some_and(|title| title.to_ascii_lowercase().contains("rostra"))
        {
            bail!("loaded page is not recognizably Rostra");
        }
        Ok(())
    }

    /// Activate the unique interactive accessibility node with an exact label.
    pub fn click_label(&mut self, label: &str) -> Result<()> {
        self.ensure_same_origin()?;
        self.cdp.call("Accessibility.enable", json!({}))?;
        let tree = self.cdp.call("Accessibility.getFullAXTree", json!({}))?;
        let nodes = tree["nodes"]
            .as_array()
            .context("accessibility tree has no nodes")?;
        let matching = nodes
            .iter()
            .filter(|node| is_interactive(node) && ax_value(node, "name") == Some(label))
            .collect::<Vec<_>>();
        let [node] = matching.as_slice() else {
            bail!(
                "expected one interactive element labelled `{label}`, found {}",
                matching.len()
            );
        };
        let backend_node_id = node["backendDOMNodeId"]
            .as_u64()
            .context("labelled accessibility node has no DOM node")?;
        let resolved = self.cdp.call(
            "DOM.resolveNode",
            json!({ "backendNodeId": backend_node_id }),
        )?;
        let object_id = resolved["object"]["objectId"]
            .as_str()
            .context("could not resolve labelled element")?;
        self.cdp.call(
            "Runtime.callFunctionOn",
            json!({
                "functionDeclaration": "function () { this.click(); }",
                "objectId": object_id,
                "userGesture": true,
            }),
        )?;
        self.settle_after_interaction()
    }

    /// Activate an element by its exact HTML ID.
    pub fn click_id(&mut self, id: &str) -> Result<()> {
        self.ensure_same_origin()?;
        let id = serde_json::to_string(id)?;
        let result = self.evaluate(&format!(
            "(() => {{ const element = document.getElementById({id}); \
             if (!element) return false; element.click(); return true; }})()"
        ))?;
        if result["result"]["value"] != Value::Bool(true) {
            bail!(
                "no element has ID `{}`",
                serde_json::from_str::<String>(&id)?
            );
        }
        self.settle_after_interaction()
    }

    /// Scroll vertically by three quarters of the viewport height.
    pub fn scroll(&mut self, direction: ScrollDirection) -> Result<()> {
        self.ensure_same_origin()?;
        let sign = match direction {
            ScrollDirection::Up => -1,
            ScrollDirection::Down => 1,
        };
        self.evaluate(&format!(
            "window.scrollBy({{ top: {sign} * innerHeight * 0.75, behavior: 'instant' }})"
        ))?;
        self.animation_frames()?;
        self.ensure_same_origin()
    }

    /// Wait for document readiness, fonts, and two animation frames.
    pub fn ready(&mut self) -> Result<()> {
        self.cdp.call(
            "Runtime.evaluate",
            json!({
                "expression": "new Promise(async resolve => {\
                    if (document.readyState !== 'complete') {\
                      await new Promise(done => addEventListener('load', done, { once: true }));\
                    }\
                    if (document.fonts) await document.fonts.ready;\
                    requestAnimationFrame(() => requestAnimationFrame(resolve));\
                })",
                "awaitPromise": true,
                "returnByValue": true,
            }),
        )?;
        Ok(())
    }

    /// Capture the current viewport to a PNG file.
    pub fn screenshot(&mut self, path: &Path) -> Result<()> {
        self.ensure_same_origin()?;
        self.ready()?;
        let result = self.cdp.call(
            "Page.captureScreenshot",
            json!({ "format": "png", "fromSurface": true, "captureBeyondViewport": false }),
        )?;
        let encoded = result["data"].as_str().context("screenshot has no data")?;
        let png = BASE64
            .decode(encoded.as_bytes())
            .context("decoding screenshot")?;
        self.ensure_same_origin()?;
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, png).with_context(|| format!("writing {}", path.display()))
    }

    /// Verify origin confinement around an explicit readiness wait.
    pub fn verify_ready(&mut self) -> Result<()> {
        self.ensure_same_origin()?;
        self.ready()?;
        self.ensure_same_origin()
    }

    /// Evaluate JavaScript and return the DevTools result wrapper.
    fn evaluate(&mut self, expression: &str) -> Result<Value> {
        self.cdp.call(
            "Runtime.evaluate",
            json!({
                "expression": expression,
                "awaitPromise": true,
                "returnByValue": true,
                "userGesture": true,
            }),
        )
    }

    /// Wait for two animation frames without waiting on document state.
    fn animation_frames(&mut self) -> Result<()> {
        self.evaluate(
            "new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)))",
        )?;
        Ok(())
    }

    /// Allow a local async update to start, then wait for a quiet DOM interval.
    fn settle_after_interaction(&mut self) -> Result<()> {
        thread::sleep(Duration::from_millis(100));
        self.ready()?;
        self.evaluate(
            "new Promise(resolve => {\
                let timer;\
                const done = () => { observer.disconnect(); resolve(); };\
                const observer = new MutationObserver(() => {\
                    clearTimeout(timer); timer = setTimeout(done, 150);\
                });\
                observer.observe(document, { subtree: true, childList: true, attributes: true });\
                timer = setTimeout(done, 150);\
                setTimeout(done, 3000);\
            })",
        )?;
        self.ensure_same_origin()
    }

    /// Reject redirects or activated controls that leave the configured origin.
    fn ensure_same_origin(&mut self) -> Result<()> {
        let result = self.evaluate("location.href")?;
        let url = result["result"]["value"]
            .as_str()
            .context("browser did not report its current URL")?;
        if !self.origin.contains(url) {
            bail!("browser left the configured Rostra origin for `{url}`");
        }
        Ok(())
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = self.cdp.call("Browser.close", json!({}));
    }
}

/// Cleanup owner established immediately after Chromium is spawned.
struct BrowserProcess {
    /// Chromium child, killed and waited on during cleanup.
    child: Child,
    /// Temporary, private browser profile.
    profile: Option<TempDir>,
}

impl Drop for BrowserProcess {
    fn drop(&mut self) {
        for _ in 0..20 {
            if self.child.try_wait().is_ok_and(|status| status.is_some()) {
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
        if self.child.try_wait().is_ok_and(|status| status.is_none()) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        thread::sleep(Duration::from_millis(50));
        if let Some(profile) = self.profile.take()
            && let Err(error) = profile.close()
        {
            eprintln!("warning: could not remove Chromium profile: {error}");
        }
    }
}
/// Extract the DevTools port while continuously draining Chromium diagnostics.
fn wait_for_debugging_port(stderr: impl Read + Send + 'static) -> Result<u16> {
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if let Some(url) = line.strip_prefix("DevTools listening on ws://")
                && let Some(port) = url
                    .split('/')
                    .next()
                    .and_then(|authority| authority.rsplit(':').next())
                    .and_then(|port| port.parse().ok())
            {
                let _ = sender.send(port);
            }
        }
    });
    receiver
        .recv_timeout(Duration::from_secs(10))
        .context("Chromium did not expose DevTools within 10 seconds")
}

/// Query Chromium's loopback-only discovery endpoint for the initial page.
fn find_page_websocket(port: u16) -> Result<String> {
    let mut stream = TcpStream::connect_timeout(
        &format!("127.0.0.1:{port}").parse()?,
        Duration::from_secs(3),
    )?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    write!(
        stream,
        "GET /json/list HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )?;
    let mut response = Vec::new();
    loop {
        let mut chunk = [0; 8192];
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => response.extend_from_slice(&chunk[..read]),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                break;
            }
            Err(error) => return Err(error.into()),
        }
    }
    let body = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| &response[index + 4..])
        .context("invalid DevTools discovery response")?;
    let targets: Vec<Value> = serde_json::from_slice(body)?;
    targets
        .iter()
        .find(|target| target["type"] == "page")
        .and_then(|target| target["webSocketDebuggerUrl"].as_str())
        .map(ToOwned::to_owned)
        .context("Chromium has no inspectable page")
}

/// Read a string-valued accessibility property.
fn ax_value<'a>(node: &'a Value, property: &str) -> Option<&'a str> {
    node[property]["value"].as_str()
}

/// Return whether an accessibility node represents an activatable control.
fn is_interactive(node: &Value) -> bool {
    matches!(
        ax_value(node, "role"),
        Some(
            "button"
                | "checkbox"
                | "link"
                | "menuitem"
                | "option"
                | "radio"
                | "switch"
                | "tab"
                | "treeitem"
        )
    )
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::Path;
    use std::time::Duration;
    use std::{env, thread};

    use tempfile::tempdir;

    use super::Browser;
    use crate::command::ScrollDirection;
    use crate::endpoint::SiteOrigin;

    #[test]
    #[ignore = "requires Chromium from the Nix development shell"]
    fn chromium_smoke() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut request = [0; 4096];
                let _ = stream.read(&mut request);
                let body = r#"<!doctype html>
                    <title>Rostra smoke fixture</title>
                    <button aria-label="Change label" onclick="this.textContent='changed'">label</button>
                    <button id="change-id" onclick="this.textContent='changed'">id</button>
                    <button id="leave" onclick="setTimeout(() => location.href='http://127.0.0.1:9/', 300)">leave</button>
                    <div style="height: 2000px"></div>"#;
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });

        let origin = SiteOrigin::parse(&format!("http://{address}")).unwrap();
        let executable = env::var_os("ROSTRA_CHROMIUM").unwrap_or_else(|| "chromium".into());
        let mut browser =
            Browser::launch(Path::new(&executable), &origin, 640, 480, false).unwrap();
        browser.open("/").unwrap();
        browser.open("/#fragment").unwrap();
        let fragment = browser.evaluate("location.hash").unwrap();
        assert_eq!(fragment["result"]["value"], "#fragment");
        browser.click_label("Change label").unwrap();
        browser.click_id("change-id").unwrap();
        browser.scroll(ScrollDirection::Down).unwrap();
        let state = browser
            .evaluate(
                "document.querySelector('[aria-label=\"Change label\"]').textContent === 'changed'\
                 && document.getElementById('change-id').textContent === 'changed'\
                 && scrollY > 0",
            )
            .unwrap();
        assert_eq!(state["result"]["value"], true);

        let output_dir = tempdir().unwrap();
        let output = output_dir.path().join("smoke.png");
        browser.screenshot(&output).unwrap();
        assert!(
            std::fs::read(&output)
                .unwrap()
                .starts_with(b"\x89PNG\r\n\x1a\n")
        );

        browser.click_id("leave").unwrap();
        thread::sleep(Duration::from_millis(400));
        let error = browser.screenshot(&output).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("browser left the configured Rostra origin")
        );
    }
}
