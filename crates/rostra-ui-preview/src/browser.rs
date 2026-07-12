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
        self.ensure_rostra_page()?;
        Ok(())
    }

    /// Activate the unique interactive accessibility node with an exact label.
    pub fn click_label(&mut self, label: &str) -> Result<()> {
        self.ensure_rostra_page()?;
        let object_id = self.label_object_id(label, true)?;
        self.cdp.call(
            "Runtime.callFunctionOn",
            json!({
                "functionDeclaration": "function () { this.click(); }",
                "objectId": &object_id,
                "userGesture": true,
            }),
        )?;
        self.settle_after_interaction()
    }

    /// Activate an element by its exact HTML ID.
    pub fn click_id(&mut self, id: &str) -> Result<()> {
        self.ensure_rostra_page()?;
        let object_id = self.id_object_id(id)?;
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

    /// Print-ready structured rendered evidence for an accessible label.
    pub fn inspect_label(&mut self, label: &str) -> Result<Value> {
        self.ensure_rostra_page()?;
        let object_id = self.label_object_id(label, false)?;
        self.inspect_object(&object_id)
    }

    /// Print-ready structured rendered evidence for an exact element ID.
    pub fn inspect_id(&mut self, id: &str) -> Result<Value> {
        self.ensure_rostra_page()?;
        let object_id = self.id_object_id(id)?;
        self.inspect_object(&object_id)
    }

    /// Return the development-server port used to bind its expected secret
    /// path.
    pub fn origin_port(&self) -> u16 {
        self.origin.port()
    }

    /// Fill Rostra's known unlock password control with an approved dev secret.
    pub fn fill_rostra_unlock_password(&mut self, secret: &str) -> Result<()> {
        self.ensure_rostra_page()?;
        let result = self.cdp.call(
            "Runtime.evaluate",
            json!({
                "expression": "location.pathname === '/unlock'\
                    ? document.querySelector(\
                        '.o-unlockScreen form.o-unlockScreen__form[action=\"/unlock\"] \
                         input[type=\"password\"][name=\"password\"]')\
                    : null",
                "returnByValue": false,
            }),
        )?;
        let object_id = result["result"]["objectId"]
            .as_str()
            .context("Rostra unlock password control was not found at /unlock")?;
        self.cdp
            .call(
                "Runtime.callFunctionOn",
                json!({
                    "functionDeclaration": "function (secret) {\
                        if (!(this instanceof HTMLInputElement) || this.type !== 'password')\
                            throw new Error('named element is not a password input');\
                        const prototype = Object.getPrototypeOf(this);\
                        const setter = Object.getOwnPropertyDescriptor(prototype, 'value')?.set;\
                        if (!setter) throw new Error('form control has no native value setter');\
                        setter.call(this, secret);\
                        this.dispatchEvent(new Event('input', { bubbles: true }));\
                        this.dispatchEvent(new Event('change', { bubbles: true }));\
                    }",
                    "objectId": object_id,
                    "arguments": [{ "value": secret }],
                    "userGesture": true,
                }),
            )
            .map_err(|_| {
                anyhow::anyhow!("Rostra unlock password input failed (details redacted)")
            })?;
        Ok(())
    }

    /// Scroll vertically by three quarters of the viewport height.
    pub fn scroll(&mut self, direction: ScrollDirection) -> Result<()> {
        self.ensure_rostra_page()?;
        let sign = match direction {
            ScrollDirection::Up => -1,
            ScrollDirection::Down => 1,
        };
        self.evaluate(&format!(
            "window.scrollBy({{ top: {sign} * innerHeight * 0.75, behavior: 'instant' }})"
        ))?;
        self.animation_frames()?;
        self.ensure_rostra_page()
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
        self.ensure_rostra_page()?;
        self.ready()?;
        let result = self.cdp.call(
            "Page.captureScreenshot",
            json!({ "format": "png", "fromSurface": true, "captureBeyondViewport": false }),
        )?;
        let encoded = result["data"].as_str().context("screenshot has no data")?;
        let png = BASE64
            .decode(encoded.as_bytes())
            .context("decoding screenshot")?;
        self.ensure_rostra_page()?;
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
        self.ensure_rostra_page()?;
        self.ready()?;
        self.ensure_rostra_page()
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

    /// Resolve a unique interactive accessibility label to a remote object.
    fn label_object_id(&mut self, label: &str, interactive_only: bool) -> Result<String> {
        self.cdp.call("Accessibility.enable", json!({}))?;
        let document = self.cdp.call("DOM.getDocument", json!({ "depth": 0 }))?;
        let node_id = document["root"]["nodeId"]
            .as_u64()
            .context("document has no DOM node ID")?;
        let tree = self.cdp.call(
            "Accessibility.queryAXTree",
            json!({ "nodeId": node_id, "accessibleName": label }),
        )?;
        let nodes = tree["nodes"]
            .as_array()
            .context("accessibility tree has no nodes")?;
        let matching = nodes
            .iter()
            .filter(|node| {
                let allowed = if interactive_only {
                    is_interactive(node)
                } else {
                    is_inspectable(node)
                };
                allowed && ax_value(node, "name") == Some(label)
            })
            .collect::<Vec<_>>();
        let [node] = matching.as_slice() else {
            let kind = if interactive_only {
                "interactive element"
            } else {
                "inspectable element"
            };
            bail!(
                "expected one {kind} labelled `{label}`, found {}",
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
        resolved["object"]["objectId"]
            .as_str()
            .map(ToOwned::to_owned)
            .context("could not resolve labelled element")
    }

    /// Resolve an exact HTML element ID to a remote object.
    fn id_object_id(&mut self, id: &str) -> Result<String> {
        let id = serde_json::to_string(id)?;
        let result = self.cdp.call(
            "Runtime.evaluate",
            json!({
                "expression": format!("document.getElementById({id})"),
                "returnByValue": false,
            }),
        )?;
        result["result"]["objectId"]
            .as_str()
            .map(ToOwned::to_owned)
            .with_context(|| {
                format!(
                    "no element has ID `{}`",
                    serde_json::from_str::<String>(&id).unwrap_or_default()
                )
            })
    }

    /// Build a bounded JSON snapshot of an element's rendered and accessible
    /// state.
    fn inspect_object(&mut self, object_id: &str) -> Result<Value> {
        let accessibility = self.cdp.call(
            "Accessibility.getPartialAXTree",
            json!({ "objectId": object_id, "fetchRelatives": false }),
        )?;
        let ax_node = accessibility["nodes"]
            .as_array()
            .and_then(|nodes| nodes.first());
        let ax_name = ax_node
            .and_then(|node| ax_value(node, "name"))
            .map(|name| name.chars().take(500).collect::<String>());
        let ax_role = ax_node
            .and_then(|node| ax_value(node, "role"))
            .map(|role| role.chars().take(80).collect::<String>());

        let result = self.cdp.call(
            "Runtime.callFunctionOn",
            json!({
                "functionDeclaration": ELEMENT_INSPECTION_SCRIPT,
                "objectId": object_id,
                "returnByValue": true,
            }),
        )?;
        let mut snapshot = result["result"]["value"]
            .as_object()
            .cloned()
            .context("element inspection did not return an object")?;
        snapshot.insert(
            "accessibility".into(),
            json!({
                "name": ax_name,
                "role": ax_role,
                "ignored": ax_node.and_then(|node| node["ignored"].as_bool()),
            }),
        );
        Ok(Value::Object(snapshot))
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
        self.ensure_rostra_page()
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

    /// Verify the current page carries Rostra's stable application marker.
    fn ensure_rostra_page(&mut self) -> Result<()> {
        self.ensure_same_origin()?;
        let result = self.evaluate(
            "document.querySelector('meta[name=\"description\"]')?.content\
                === 'Rostra — a peer-to-peer social network'\
             && Boolean(document.querySelector('.o-pageLayout'))",
        )?;
        if result["result"]["value"] != Value::Bool(true) {
            bail!("current loopback page is not recognizably Rostra");
        }
        Ok(())
    }
}

/// JavaScript returning bounded, form-value-omitting evidence about an element.
const ELEMENT_INSPECTION_SCRIPT: &str = r#"function () {
    const short = (value, limit = 500) => String(value ?? '').slice(0, limit);
    const clean = (value, limit = 500) =>
        String(value ?? '').slice(0, limit).replace(/\s+/g, ' ').trim();
    const isRendered = element => Boolean(element?.checkVisibility({
        checkOpacity: true,
        checkVisibilityCSS: true
    }));
    const renderedText = (element, limit = 500) => {
        if (!isRendered(element)) return '';
        const walker = document.createTreeWalker(element, NodeFilter.SHOW_TEXT);
        const parts = [];
        let length = 0;
        for (let count = 0; count < 64 && length < limit;) {
            const node = walker.nextNode();
            if (!node) break;
            count += 1;
            if (!isRendered(node.parentElement)) continue;
            const part = clean(node.textContent, limit - length);
            if (part) {
                parts.push(part);
                length += part.length + 1;
            }
        }
        return clean(parts.join(' '), limit);
    };
    const classes = element => [...element.classList].slice(0, 24)
        .map(name => short(name, 120));
    const summarize = element => element ? {
        tag: short(element.tagName.toLowerCase(), 80),
        id: element.id ? short(element.id, 160) : null,
        classes: classes(element).slice(0, 16),
        text: renderedText(element, 160)
    } : null;
    const selectedStyles = style => Object.fromEntries(Object.entries({
        display: style.display,
        position: style.position,
        visibility: style.visibility,
        opacity: style.opacity,
        color: style.color,
        backgroundColor: style.backgroundColor,
        backgroundImage: style.backgroundImage,
        border: style.border,
        borderRadius: style.borderRadius,
        boxShadow: style.boxShadow,
        fontFamily: style.fontFamily,
        fontSize: style.fontSize,
        fontWeight: style.fontWeight,
        lineHeight: style.lineHeight,
        textDecoration: style.textDecoration,
        textAlign: style.textAlign,
        padding: style.padding,
        margin: style.margin,
        gap: style.gap,
        minWidth: style.minWidth,
        minHeight: style.minHeight,
        alignItems: style.alignItems,
        justifyContent: style.justifyContent,
        cursor: style.cursor,
        outline: style.outline
    }).map(([name, value]) => [name, short(value)]));
    const pseudo = function (element, name) {
        const style = getComputedStyle(element, name);
        return Object.fromEntries(Object.entries({
            content: style.content,
            width: style.width,
            height: style.height,
            color: style.color,
            backgroundColor: style.backgroundColor,
            backgroundImage: style.backgroundImage,
            maskImage: style.maskImage
        }).map(([property, value]) => [property, short(value)]));
    };
    const rendered = element => {
        if (!element) return null;
        const box = element.getBoundingClientRect();
        return {
            ...summarize(element),
            geometry: {
                x: box.x, y: box.y, width: box.width, height: box.height,
                top: box.top, right: box.right, bottom: box.bottom, left: box.left
            },
            styles: selectedStyles(getComputedStyle(element)),
            decoration: {
                before: pseudo(element, '::before'),
                after: pseudo(element, '::after')
            }
        };
    };
    const rect = this.getBoundingClientRect();
    const decorativeChildren = [];
    const childWalker = document.createTreeWalker(this, NodeFilter.SHOW_ELEMENT);
    for (let count = 0; count < 12;) {
        const element = childWalker.nextNode();
        if (!element) break;
        count += 1;
        const style = getComputedStyle(element);
        const evidence = {
            ...summarize(element),
            backgroundImage: short(style.backgroundImage),
            maskImage: short(style.maskImage),
            width: short(style.width),
            height: short(style.height)
        };
        if (evidence.backgroundImage !== 'none'
            || evidence.maskImage !== 'none'
            || evidence.classes.some(name => /icon/i.test(name))) {
            decorativeChildren.push(evidence);
        }
        if (decorativeChildren.length === 6) break;
    }
    const svgs = [];
    const svgWalker = document.createTreeWalker(this, NodeFilter.SHOW_ELEMENT);
    for (let count = 0; count < 128 && svgs.length < 4;) {
        const svg = svgWalker.nextNode();
        if (!svg) break;
        count += 1;
        if (svg.tagName.toLowerCase() !== 'svg') continue;
        svgs.push({
            classes: classes(svg).slice(0, 8),
            viewBox: short(svg.getAttribute('viewBox')),
            width: short(svg.getAttribute('width') || getComputedStyle(svg).width),
            height: short(svg.getAttribute('height') || getComputedStyle(svg).height),
            fill: short(svg.getAttribute('fill') || getComputedStyle(svg).fill),
            stroke: short(svg.getAttribute('stroke') || getComputedStyle(svg).stroke),
            title: renderedText(svg, 120),
            ariaHidden: short(svg.getAttribute('aria-hidden'))
        });
    }
    return {
        tag: short(this.tagName.toLowerCase(), 80),
        id: this.id ? short(this.id, 160) : null,
        classes: classes(this),
        text: renderedText(this),
        geometry: {
            x: rect.x, y: rect.y, width: rect.width, height: rect.height,
            top: rect.top, right: rect.right, bottom: rect.bottom, left: rect.left
        },
        styles: selectedStyles(getComputedStyle(this)),
        state: {
            disabled: Boolean(this.disabled),
            ariaDisabled: short(this.getAttribute('aria-disabled'), 80) || null,
            pressed: short(this.getAttribute('aria-pressed'), 80) || null,
            expanded: short(this.getAttribute('aria-expanded'), 80) || null,
            checked: 'checked' in this ? Boolean(this.checked) : null,
            selected: 'selected' in this ? Boolean(this.selected) : null,
            focused: document.activeElement === this,
            focusVisible: this.matches(':focus-visible'),
            hovered: this.matches(':hover'),
            hidden: Boolean(this.hidden),
            type: short(this.getAttribute('type'), 80) || null
        },
        decoration: {
            before: pseudo(this, '::before'),
            after: pseudo(this, '::after'),
            decorativeChildren,
            svgs
        },
        context: {
            parent: rendered(this.parentElement),
            previousSibling: rendered(this.previousElementSibling),
            nextSibling: rendered(this.nextElementSibling)
        }
    };
}"#;

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

/// Return whether an AX node is a meaningful labelled inspection target.
fn is_inspectable(node: &Value) -> bool {
    node["ignored"] != Value::Bool(true)
        && node["backendDOMNodeId"].as_u64().is_some()
        && !matches!(
            ax_value(node, "role"),
            Some("InlineTextBox" | "StaticText" | "none" | "presentation")
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
                    <meta charset="utf-8">
                    <meta name="description" content="Rostra — a peer-to-peer social network">
                    <style>
                      #fixture-toggle { opacity: 0; width: 0; height: 0; }
                      .slider { display: inline-block; width: 40px; height: 20px; }
                      .slider::before { content: ''; display: block; width: 16px; height: 16px; }
                    </style>
                    <div class="o-pageLayout">
                    <h2 aria-label="Fixture section">Visible heading</h2>
                    <button aria-label="Change label" onclick="this.firstChild.textContent='changed'">label<span style="display:none"><span>NESTED-HIDDEN-SENTINEL</span></span></button>
                    <button id="change-id" onclick="this.textContent='changed'">id</button>
                    <button id="leave" onclick="setTimeout(() => location.href='http://127.0.0.1:9/', 300)">leave</button>
                    <label><input id="fixture-toggle" type="checkbox"><span class="slider"></span></label>
                    <div class="o-unlockScreen"><form class="o-unlockScreen__form" action="/unlock">
                    <input id="secret-input" name="password" type="password"></form></div>
                    <div id="hidden-root" style="display:none"><span>HIDDEN-ROOT-SENTINEL</span></div>
                    <div style="height: 2000px"></div></div>"#;
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
        let inspection = browser.inspect_label("Change label").unwrap();
        assert_eq!(inspection["accessibility"]["role"], "button");
        assert_eq!(inspection["text"], "changed");
        assert!(!inspection.to_string().contains("NESTED-HIDDEN-SENTINEL"));
        assert!(inspection.to_string().len() < 64 * 1024);
        let hidden_root = browser.inspect_id("hidden-root").unwrap();
        assert!(!hidden_root.to_string().contains("HIDDEN-ROOT-SENTINEL"));
        let heading = browser.inspect_label("Fixture section").unwrap();
        assert_eq!(heading["accessibility"]["role"], "heading");
        browser
            .evaluate(
                "(() => {\
                    const element = document.createElement('x-' + 't'.repeat(2000));\
                    element.setAttribute('aria-label', 'Bound target');\
                    element.className = 'c'.repeat(2000);\
                    element.textContent = 'x'.repeat(2000);\
                    element.style.backgroundImage = `url(\"data:image/svg+xml,${'y'.repeat(2000)}\")`;\
                    document.querySelector('.o-pageLayout').append(element);\
                    return true;\
                })()",
            )
            .unwrap();
        let bounded = browser.inspect_label("Bound target").unwrap();
        assert!(bounded["tag"].as_str().unwrap().len() <= 80);
        assert!(bounded["classes"][0].as_str().unwrap().len() <= 120);
        assert!(bounded["text"].as_str().unwrap().len() <= 500);
        assert!(bounded["styles"]["backgroundImage"].as_str().unwrap().len() <= 500);
        assert!(bounded.to_string().len() < 64 * 1024);
        let toggle = browser.inspect_id("fixture-toggle").unwrap();
        assert_eq!(toggle["context"]["nextSibling"]["geometry"]["width"], 40);
        assert_eq!(
            toggle["context"]["nextSibling"]["decoration"]["before"]["width"],
            "16px"
        );
        let changed = browser
            .evaluate(
                "document.querySelector('[aria-label=\"Change label\"]').firstChild.textContent === 'changed'\
                 && document.getElementById('change-id').textContent === 'changed'",
            )
            .unwrap();
        assert_eq!(changed["result"]["value"], true);
        browser.open("/unlock").unwrap();
        browser
            .fill_rostra_unlock_password("not-a-real-secret")
            .unwrap();
        let secret_inspection = browser.inspect_id("secret-input").unwrap();
        assert!(!secret_inspection.to_string().contains("not-a-real-secret"));
        browser.scroll(ScrollDirection::Down).unwrap();
        let state = browser
            .evaluate(
                "document.getElementById('secret-input').value === 'not-a-real-secret'\
                 && scrollY > 0",
            )
            .unwrap();
        assert_eq!(state["result"]["value"], true);
        browser
            .evaluate(
                "Object.defineProperty(HTMLInputElement.prototype, 'value', {\
                    configurable: true,\
                    set(value) { throw new Error(value); }\
                }); true",
            )
            .unwrap();
        let redacted = browser
            .fill_rostra_unlock_password("SECRET-ERROR-SENTINEL")
            .unwrap_err();
        assert!(!format!("{redacted:#}").contains("SECRET-ERROR-SENTINEL"));

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
