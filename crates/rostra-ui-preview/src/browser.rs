use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use std::{fs, thread};

use anyhow::{Context, Result, bail};
use data_encoding::BASE64;
use serde_json::{Value, json};
use tempfile::TempDir;

use crate::cdp::Cdp;
use crate::command::ScrollDirection;
use crate::endpoint::SiteOrigin;

/// Exact element lookup failure eligible for deferred inspection reporting.
#[derive(Debug)]
struct ElementLookupError {
    /// Safe, bounded lookup diagnostic and suggestions.
    message: String,
}

impl std::fmt::Display for ElementLookupError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(formatter)
    }
}

impl std::error::Error for ElementLookupError {}

/// Semantic accessibility target selected by a label-based action.
#[derive(Clone, Copy)]
enum LabelTarget {
    /// An activatable control.
    Interactive,
    /// Any meaningful rendered element.
    Inspectable,
    /// An editable text element.
    Editable,
}

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
    /// Return whether an action error is only an exact element lookup miss.
    pub fn is_lookup_error(error: &anyhow::Error) -> bool {
        error.downcast_ref::<ElementLookupError>().is_some()
    }

    /// Construct a lookup miss for focused stream-policy tests.
    #[cfg(test)]
    pub(crate) fn lookup_error_for_test() -> anyhow::Error {
        ElementLookupError {
            message: "test lookup miss".into(),
        }
        .into()
    }

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
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            self.cdp
                .call_and_wait_with_timeout(
                    "Page.navigate",
                    json!({ "url": url.as_str() }),
                    "Page.lifecycleEvent",
                    remaining(deadline)?,
                )
                .context("navigating Chromium")?;
            self.ready_with_timeout(remaining(deadline)?)?;
            self.ensure_same_origin_with_timeout(remaining(deadline)?)?;
            if self.is_rostra_page_with_timeout(remaining(deadline)?)? {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(250).min(remaining(deadline)?));
        }
    }

    /// Activate the unique interactive accessibility node with an exact label.
    pub fn click_label(&mut self, label: &str) -> Result<()> {
        self.ensure_rostra_page()?;
        let object_id = self.label_object_id(label, LabelTarget::Interactive)?;
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

    /// Replace the text in an editable element found by its accessible label.
    pub fn fill_label(&mut self, label: &str, text: &str) -> Result<()> {
        self.ensure_rostra_page()?;
        let object_id = self.label_object_id(label, LabelTarget::Editable)?;
        self.fill_object(&object_id, text)
    }

    /// Replace the text in an editable element found by its exact HTML ID.
    pub fn fill_id(&mut self, id: &str, text: &str) -> Result<()> {
        self.ensure_rostra_page()?;
        let object_id = self.id_object_id(id)?;
        self.fill_object(&object_id, text)
    }

    /// Replace the text in a resolved editable element.
    fn fill_object(&mut self, object_id: &str, text: &str) -> Result<()> {
        self.cdp.call(
            "Runtime.callFunctionOn",
            json!({
                "functionDeclaration": "function () {\
                    const input = this instanceof HTMLInputElement;\
                    const textarea = this instanceof HTMLTextAreaElement;\
                    const textInputTypes = new Set(['text', 'search', 'email', 'url', 'tel']);\
                    if (input && this.type === 'password')\
                        throw new Error('generic fill refuses password controls');\
                    if (input && !textInputTypes.has(this.type))\
                        throw new Error('named input is not a textual input');\
                    if (!input && !textarea && !this.isContentEditable)\
                        throw new Error('named element is not editable');\
                    if ((input || textarea) && (this.disabled || this.readOnly))\
                        throw new Error('named control is disabled or read-only');\
                    if (this.isContentEditable && this.getAttribute('aria-disabled') === 'true')\
                        throw new Error('named editable element is disabled');\
                    this.focus();\
                    if (document.activeElement !== this && !this.contains(document.activeElement))\
                        throw new Error('named editable element did not receive focus');\
                    if (input || textarea) this.select();\
                    else {\
                        const selection = window.getSelection();\
                        const range = document.createRange();\
                        range.selectNodeContents(this);\
                        selection.removeAllRanges();\
                        selection.addRange(range);\
                        if (!selection.containsNode(this, true))\
                            throw new Error('named editable element could not be selected');\
                    }\
                }",
                "objectId": object_id,
                "userGesture": true,
            }),
        )?;
        if text.is_empty() {
            self.cdp.call(
                "Runtime.callFunctionOn",
                json!({
                    "functionDeclaration": "function () {\
                        if (this instanceof HTMLInputElement\
                            || this instanceof HTMLTextAreaElement) {\
                            const prototype = Object.getPrototypeOf(this);\
                            const setter = Object.getOwnPropertyDescriptor(prototype, 'value')?.set;\
                            if (!setter) throw new Error('editable control has no native value setter');\
                            setter.call(this, '');\
                        } else {\
                            this.textContent = '';\
                        }\
                        this.dispatchEvent(new InputEvent('input', {\
                            bubbles: true,\
                            inputType: 'deleteContentBackward',\
                            data: null,\
                        }));\
                    }",
                    "objectId": object_id,
                    "userGesture": true,
                }),
            )?;
        } else {
            self.cdp.call("Input.insertText", json!({ "text": text }))?;
        }
        self.settle_after_interaction()
    }

    /// Print-ready structured rendered evidence for an accessible label.
    pub fn inspect_label(&mut self, label: &str) -> Result<Value> {
        self.ensure_rostra_page()?;
        let object_id = self.label_object_id(label, LabelTarget::Inspectable)?;
        self.inspect_object(&object_id)
    }

    /// Print-ready structured rendered evidence for an exact element ID.
    pub fn inspect_id(&mut self, id: &str) -> Result<Value> {
        self.ensure_rostra_page()?;
        let object_id = self.id_object_id(id)?;
        self.inspect_object(&object_id)
    }

    /// Move Chromium's pointer onto an exact accessible-label target.
    pub fn hover_label(&mut self, label: &str) -> Result<()> {
        self.ensure_rostra_page()?;
        let object_id = self.label_object_id(label, LabelTarget::Inspectable)?;
        self.hover_object(&object_id)
    }

    /// Move Chromium's pointer onto an exact element-ID target.
    pub fn hover_id(&mut self, id: &str) -> Result<()> {
        self.ensure_rostra_page()?;
        let object_id = self.id_object_id(id)?;
        self.hover_object(&object_id)
    }

    /// Move Chromium's pointer outside the page viewport.
    pub fn unhover(&mut self) -> Result<()> {
        self.ensure_rostra_page()?;
        self.cdp.call(
            "Input.dispatchMouseEvent",
            json!({ "type": "mouseMoved", "x": -1, "y": -1, "buttons": 0 }),
        )?;
        self.animation_frames()?;
        self.ensure_rostra_page()
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

    /// Close open dialogs and delete the current Rostra server session.
    pub fn cleanup_authenticated_preview(&mut self) -> Result<()> {
        self.ensure_same_origin()?;
        let result = self.cdp.call_with_timeout(
            "Runtime.evaluate",
            json!({
                "expression": "(async () => {\
                    document.querySelectorAll('dialog[open]').forEach(dialog => dialog.close());\
                    const response = await fetch('/unlock/logout', {\
                        method: 'POST', credentials: 'same-origin', redirect: 'follow'\
                    });\
                    return response.ok;\
                })()",
                "awaitPromise": true,
                "returnByValue": true,
            }),
            Duration::from_secs(5),
        )?;
        if result["result"]["value"] != Value::Bool(true) {
            bail!("Rostra logout request did not succeed");
        }
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
        self.ready_with_timeout(Duration::from_secs(15))
    }

    /// Wait for visual readiness within a caller-provided budget.
    fn ready_with_timeout(&mut self, timeout: Duration) -> Result<()> {
        self.cdp.call_with_timeout(
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
            timeout,
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
        self.evaluate_with_timeout(expression, Duration::from_secs(15))
    }

    /// Evaluate JavaScript within a caller-provided budget.
    fn evaluate_with_timeout(&mut self, expression: &str, timeout: Duration) -> Result<Value> {
        self.cdp.call_with_timeout(
            "Runtime.evaluate",
            json!({
                "expression": expression,
                "awaitPromise": true,
                "returnByValue": true,
                "userGesture": true,
            }),
            timeout,
        )
    }

    /// Wait for two animation frames without waiting on document state.
    fn animation_frames(&mut self) -> Result<()> {
        self.evaluate(
            "new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)))",
        )?;
        Ok(())
    }

    /// Resolve a unique accessibility label of the requested semantic target.
    fn label_object_id(&mut self, label: &str, target: LabelTarget) -> Result<String> {
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
                let allowed = match target {
                    LabelTarget::Interactive => is_interactive(node),
                    LabelTarget::Inspectable => is_inspectable(node),
                    LabelTarget::Editable => is_editable(node),
                };
                allowed && ax_value(node, "name") == Some(label)
            })
            .collect::<Vec<_>>();
        let [node] = matching.as_slice() else {
            let kind = match target {
                LabelTarget::Interactive => "interactive element",
                LabelTarget::Inspectable => "inspectable element",
                LabelTarget::Editable => "editable element",
            };
            let suggestions = if matching.is_empty() {
                self.label_suggestions(label, target)?
            } else {
                String::new()
            };
            return Err(ElementLookupError {
                message: format!(
                    "expected one {kind} labelled `{label}`, found {}{suggestions}",
                    matching.len(),
                ),
            }
            .into());
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

    /// Build a bounded nearby-name hint after an exact AX lookup misses.
    fn label_suggestions(&mut self, label: &str, target: LabelTarget) -> Result<String> {
        let tree = self.cdp.call("Accessibility.getFullAXTree", json!({}))?;
        let mut candidates = tree["nodes"]
            .as_array()
            .context("accessibility tree has no nodes")?
            .iter()
            .filter(|node| match target {
                LabelTarget::Interactive => is_interactive(node),
                LabelTarget::Inspectable => is_inspectable(node),
                LabelTarget::Editable => is_editable(node),
            })
            .filter_map(|node| {
                let name = ax_value(node, "name")?;
                if name.is_empty() {
                    return None;
                }
                let name = name.chars().take(120).collect::<String>();
                let role = ax_value(node, "role")
                    .unwrap_or("unknown")
                    .chars()
                    .take(40)
                    .collect::<String>();
                Some((label_score(label, &name), name, role))
            })
            .collect::<Vec<_>>();
        candidates.sort();
        candidates.dedup_by(|left, right| left.1 == right.1 && left.2 == right.2);
        candidates.truncate(5);
        if candidates.is_empty() {
            return Ok(String::new());
        }
        let suggestions = candidates
            .into_iter()
            .map(|(_, name, role)| {
                format!(
                    "{} ({role})",
                    serde_json::to_string(&name).unwrap_or_else(|_| "\"?\"".into())
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        Ok(format!("; nearby labels: {suggestions}"))
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
            .ok_or_else(|| ElementLookupError {
                message: format!(
                    "no element has ID `{}`",
                    serde_json::from_str::<String>(&id).unwrap_or_default()
                ),
            })
            .map_err(Into::into)
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
        snapshot.insert(
            "renderedChildLayout".into(),
            Value::Array(self.inspect_rendered_child_layout(object_id)?),
        );
        Ok(Value::Object(snapshot))
    }

    /// Inspect rendered DOM children through depth two with browser AX
    /// semantics.
    fn inspect_rendered_child_layout(&mut self, object_id: &str) -> Result<Vec<Value>> {
        const OBJECT_GROUP: &str = "rostra-child-layout";
        let deadline = Instant::now() + Duration::from_secs(10);
        let result = (|| {
            let children = self.cdp.call_with_timeout(
                "Runtime.callFunctionOn",
                json!({
                    "functionDeclaration": CHILD_LAYOUT_ELEMENTS_SCRIPT,
                    "objectId": object_id,
                    "objectGroup": OBJECT_GROUP,
                    "returnByValue": false,
                }),
                remaining(deadline)?,
            )?;
            let children_object_id = children["result"]["objectId"]
                .as_str()
                .context("child layout traversal did not return an array")?;
            let properties = self.cdp.call_with_timeout(
                "Runtime.getProperties",
                json!({
                    "objectId": children_object_id,
                    "ownProperties": true,
                    "accessorPropertiesOnly": false,
                    "generatePreview": false,
                }),
                remaining(deadline)?,
            )?;
            let child_objects = properties["result"]
                .as_array()
                .context("child layout array has no properties")?
                .iter()
                .filter_map(|property| {
                    property["name"].as_str()?.parse::<usize>().ok()?;
                    let object_id = property["value"]["objectId"].as_str()?;
                    Some(object_id.to_owned())
                })
                .take(16)
                .collect::<Vec<_>>();

            let mut summaries = Vec::new();
            for wrapper_object_id in child_objects {
                let wrapper = self.cdp.call_with_timeout(
                    "Runtime.getProperties",
                    json!({
                        "objectId": wrapper_object_id,
                        "ownProperties": true,
                        "accessorPropertiesOnly": false,
                        "generatePreview": false,
                    }),
                    remaining(deadline)?,
                )?;
                let wrapper_properties = wrapper["result"]
                    .as_array()
                    .context("child layout wrapper has no properties")?;
                let depth = wrapper_properties
                    .iter()
                    .find(|property| property["name"] == "depth")
                    .and_then(|property| property["value"]["value"].as_u64())
                    .context("child layout wrapper has no depth")?;
                let child_object_id = wrapper_properties
                    .iter()
                    .find(|property| property["name"] == "element")
                    .and_then(|property| property["value"]["objectId"].as_str())
                    .context("child layout wrapper has no element")?
                    .to_owned();
                let rendered = self.cdp.call_with_timeout(
                    "Runtime.callFunctionOn",
                    json!({
                        "functionDeclaration": CHILD_LAYOUT_INSPECTION_SCRIPT,
                        "objectId": &child_object_id,
                        "objectGroup": OBJECT_GROUP,
                        "returnByValue": true,
                    }),
                    remaining(deadline)?,
                )?;
                let Some(mut summary) = rendered["result"]["value"].as_object().cloned() else {
                    continue;
                };
                let accessibility = self.cdp.call_with_timeout(
                    "Accessibility.getPartialAXTree",
                    json!({ "objectId": child_object_id, "fetchRelatives": false }),
                    remaining(deadline)?,
                )?;
                let ax_node = accessibility["nodes"]
                    .as_array()
                    .and_then(|nodes| nodes.first());
                summary.insert("depth".into(), depth.into());
                summary.insert(
                    "accessibility".into(),
                    json!({
                        "name": ax_node
                            .and_then(|node| ax_value(node, "name"))
                            .map(|name| name.chars().take(160).collect::<String>()),
                        "role": ax_node
                            .and_then(|node| ax_value(node, "role"))
                            .map(|role| role.chars().take(80).collect::<String>()),
                        "ignored": ax_node.and_then(|node| node["ignored"].as_bool()),
                    }),
                );
                summaries.push(Value::Object(summary));
            }
            Ok(summaries)
        })();
        let release_result = self.cdp.call(
            "Runtime.releaseObjectGroup",
            json!({ "objectGroup": OBJECT_GROUP }),
        );
        match (result, release_result) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error.context("releasing child layout objects")),
            (Ok(summaries), Ok(_)) => Ok(summaries),
        }
    }

    /// Scroll a resolved object into view and move Chromium's real pointer to
    /// it.
    fn hover_object(&mut self, object_id: &str) -> Result<()> {
        self.cdp.call(
            "Runtime.callFunctionOn",
            json!({
                "functionDeclaration": "function () {\
                    this.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' });\
                }",
                "objectId": object_id,
            }),
        )?;
        self.animation_frames()?;
        let result = self.cdp.call(
            "Runtime.callFunctionOn",
            json!({
                "functionDeclaration": "function () {\
                    const rect = this.getBoundingClientRect();\
                    return { x: rect.x + rect.width / 2, y: rect.y + rect.height / 2,\
                             width: rect.width, height: rect.height };\
                }",
                "objectId": object_id,
                "returnByValue": true,
            }),
        )?;
        let geometry = &result["result"]["value"];
        let width = geometry["width"]
            .as_f64()
            .context("hover target has no width")?;
        let height = geometry["height"]
            .as_f64()
            .context("hover target has no height")?;
        if width <= 0.0 || height <= 0.0 {
            bail!("hover target has no rendered area");
        }
        let x = geometry["x"].as_f64().context("hover target has no x")?;
        let y = geometry["y"].as_f64().context("hover target has no y")?;
        self.cdp.call(
            "Input.dispatchMouseEvent",
            json!({ "type": "mouseMoved", "x": x, "y": y, "buttons": 0 }),
        )?;
        self.animation_frames()?;
        let hovered = self.cdp.call(
            "Runtime.callFunctionOn",
            json!({
                "functionDeclaration": "function () { return this.matches(':hover'); }",
                "objectId": object_id,
                "returnByValue": true,
            }),
        )?;
        if hovered["result"]["value"] != Value::Bool(true) {
            bail!("hover target is occluded at its rendered center");
        }
        self.ensure_rostra_page()
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
        self.ensure_same_origin_with_timeout(Duration::from_secs(15))
    }

    /// Enforce the origin boundary within a caller-provided budget.
    fn ensure_same_origin_with_timeout(&mut self, timeout: Duration) -> Result<()> {
        let result = self.evaluate_with_timeout("location.href", timeout)?;
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
        if !self.is_rostra_page()? {
            bail!("current loopback page is not recognizably Rostra");
        }
        Ok(())
    }

    /// Return whether the page carries Rostra's stable application marker.
    fn is_rostra_page(&mut self) -> Result<bool> {
        self.is_rostra_page_with_timeout(Duration::from_secs(15))
    }

    /// Check Rostra's marker within a caller-provided budget.
    fn is_rostra_page_with_timeout(&mut self, timeout: Duration) -> Result<bool> {
        let result = self.evaluate_with_timeout(
            "document.querySelector('meta[name=\"description\"]')?.content\
                === 'Rostra — a peer-to-peer social network'\
             && Boolean(document.querySelector('.o-pageLayout'))",
            timeout,
        )?;
        Ok(result["result"]["value"] == Value::Bool(true))
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

/// JavaScript returning at most sixteen rendered child handles through depth
/// two.
const CHILD_LAYOUT_ELEMENTS_SCRIPT: &str = r#"function () {
    const output = [];
    let visited = 0;
    for (const child of this.children) {
        if (++visited > 64 || output.length >= 16) break;
        if (!child.checkVisibility({ checkOpacity: true, checkVisibilityCSS: true })) continue;
        output.push({ element: child, depth: 1 });
        for (const grandchild of child.children) {
            if (++visited > 64 || output.length >= 16) break;
            if (!grandchild.checkVisibility({ checkOpacity: true, checkVisibilityCSS: true })) continue;
            output.push({ element: grandchild, depth: 2 });
        }
    }
    return output;
}"#;

/// JavaScript returning a compact rendered-layout summary for one child.
const CHILD_LAYOUT_INSPECTION_SCRIPT: &str = r#"function () {
    if (!this.checkVisibility({ checkOpacity: true, checkVisibilityCSS: true })) return null;
    const short = (value, limit = 240) => String(value ?? '').slice(0, limit);
    const rect = this.getBoundingClientRect();
    const style = getComputedStyle(this);
    return {
        tag: short(this.tagName.toLowerCase(), 80),
        id: this.id ? short(this.id, 160) : null,
        classes: [...this.classList].slice(0, 12).map(name => short(name, 120)),
        text: short(this.innerText, 240).replace(/\s+/g, ' ').trim(),
        geometry: {
            x: rect.x, y: rect.y, width: rect.width, height: rect.height,
            top: rect.top, right: rect.right, bottom: rect.bottom, left: rect.left
        },
        styles: {
            display: short(style.display),
            position: short(style.position),
            padding: short(style.padding),
            margin: short(style.margin),
            gap: short(style.gap),
            alignItems: short(style.alignItems),
            justifyContent: short(style.justifyContent),
            fontFamily: short(style.fontFamily),
            fontSize: short(style.fontSize),
            fontWeight: short(style.fontWeight),
            lineHeight: short(style.lineHeight),
            textAlign: short(style.textAlign)
        }
    };
}"#;

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = self.cdp.call("Browser.close", json!({}));
    }
}

/// Return the remaining portion of one absolute retry budget.
fn remaining(deadline: Instant) -> Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .context("Rostra navigation retry budget expired")
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

/// Return whether an accessibility node represents an editable text control.
fn is_editable(node: &Value) -> bool {
    matches!(ax_value(node, "role"), Some("textbox" | "searchbox"))
        && node["ignored"] != Value::Bool(true)
        && node["backendDOMNodeId"].as_u64().is_some()
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

/// Rank a candidate name, strongly preferring case-insensitive containment.
fn label_score(expected: &str, candidate: &str) -> usize {
    let expected = expected
        .to_lowercase()
        .chars()
        .take(120)
        .collect::<String>();
    let candidate = candidate
        .to_lowercase()
        .chars()
        .take(120)
        .collect::<String>();
    if candidate.contains(&expected) || expected.contains(&candidate) {
        expected.len().abs_diff(candidate.len())
    } else {
        128 + levenshtein(&expected, &candidate)
    }
}

/// Compute bounded Unicode-scalar Levenshtein distance.
fn levenshtein(left: &str, right: &str) -> usize {
    let right = right.chars().collect::<Vec<_>>();
    let mut previous = (0..=right.len()).collect::<Vec<_>>();
    for (left_index, left_char) in left.chars().enumerate() {
        let mut current = vec![left_index + 1];
        for (right_index, right_char) in right.iter().enumerate() {
            current.push(
                (current[right_index] + 1)
                    .min(previous[right_index + 1] + 1)
                    .min(previous[right_index] + usize::from(left_char != *right_char)),
            );
        }
        previous = current;
    }
    previous[right.len()]
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};
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
        let transient_served = Arc::new(AtomicBool::new(false));
        let server_transient_served = transient_served.clone();
        let logout_seen = Arc::new(AtomicBool::new(false));
        let server_logout_seen = logout_seen.clone();
        let forbidden_submit_seen = Arc::new(AtomicBool::new(false));
        let server_forbidden_submit_seen = forbidden_submit_seen.clone();
        thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut request = [0; 4096];
                let read = stream.read(&mut request).unwrap_or(0);
                let request = String::from_utf8_lossy(&request[..read]);
                if request.starts_with("POST /unlock/logout ") {
                    server_logout_seen.store(true, Ordering::SeqCst);
                }
                if request.starts_with("POST /forbidden ") {
                    server_forbidden_submit_seen.store(true, Ordering::SeqCst);
                }
                let transient = request.starts_with("GET /transient ")
                    && !server_transient_served.swap(true, Ordering::SeqCst);
                let persistent = request.starts_with("GET /missing-marker ");
                let body = if transient || persistent {
                    "<!doctype html><title>Development server rebuilding</title>"
                } else {
                    r#"<!doctype html>
                    <title>Rostra smoke fixture</title>
                    <meta charset="utf-8">
                    <meta name="description" content="Rostra — a peer-to-peer social network">
                    <style>
                      #fixture-toggle { opacity: 0; width: 0; height: 0; }
                      .slider { display: inline-block; width: 40px; height: 20px; }
                      .slider::before { content: ''; display: block; width: 16px; height: 16px; }
                      .hover-target:hover { background-color: rgb(1, 2, 3); }
                      .hover-target .icon { background-image: url('/base.svg'); width: 16px; height: 16px; }
                      .hover-target:hover .icon { background-image: url('/hover.svg'); }
                    </style>
                    <div class="o-pageLayout">
                    <h2 aria-label="Fixture section">Visible heading</h2>
                    <button class="hover-target" aria-label="Change label" onclick="this.firstChild.textContent='changed'">label<span class="icon"></span><span style="display:none"><span>NESTED-HIDDEN-SENTINEL</span></span></button>
                    <button id="change-id" onclick="this.textContent='changed'">id</button>
                    <div style="position:relative;width:80px;height:30px">
                      <button id="occluded" style="width:80px;height:30px">occluded</button>
                      <span style="position:absolute;inset:0;z-index:2"></span>
                    </div>
                    <button id="leave" onclick="setTimeout(() => location.href='http://127.0.0.1:9/', 300)">leave</button>
                    <label><input id="fixture-toggle" type="checkbox"><span class="slider"></span></label>
                    <div class="o-unlockScreen"><form class="o-unlockScreen__form" action="/unlock">
                    <input id="secret-input" name="password" type="password"></form></div>
                    <label for="plain-text">Plain text</label>
                    <input id="plain-text" value="old" oninput="this.dataset.inputBubbled=event.bubbles">
                    <textarea id="plain-textarea">old area</textarea>
                    <div id="editable" role="textbox" aria-label="Editable text" contenteditable>old editable</div>
                    <input id="non-text-input" type="checkbox">
                    <input id="readonly-input" value="old" readonly>
                    <textarea id="disabled-textarea" disabled>old</textarea>
                    <div id="not-editable">old</div>
                    <div id="hidden-root" style="display:none"><span>HIDDEN-ROOT-SENTINEL</span></div>
                    <section id="layout-container">
                      <form class="dialog-content" style="padding:20px">
                        <input type="hidden" value="FORM-SECRET-SENTINEL">
                        <h4 class="dialog-title">Layout title</h4>
                        <div class="dialog-actions" style="display:flex;justify-content:flex-end;gap:8px">
                          <button>Cancel</button><button>Confirm</button>
                        </div>
                      </form>
                    </section>
                    <dialog id="cleanup-dialog" open>
                      <form action="/forbidden" method="post"><button>Reveal</button></form>
                    </dialog>
                    <div style="height: 2000px"></div></div>"#
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });

        let origin = SiteOrigin::parse(&format!("http://{address}")).unwrap();
        let executable = env::var_os("ROSTRA_CHROMIUM").unwrap_or_else(|| "chromium".into());
        let mut browser =
            Browser::launch(Path::new(&executable), &origin, 640, 480, false).unwrap();
        browser.open("/").unwrap();
        browser.open("/transient").unwrap();
        assert!(transient_served.load(Ordering::SeqCst));
        let retry_start = Instant::now();
        let retry_error = browser.open("/missing-marker").unwrap_err();
        assert!(retry_error.to_string().contains("retry budget"));
        assert!(retry_start.elapsed() < Duration::from_secs(4));
        browser.open("/").unwrap();
        browser.open("/#fragment").unwrap();
        let fragment = browser.evaluate("location.hash").unwrap();
        assert_eq!(fragment["result"]["value"], "#fragment");
        browser.fill_label("Plain text", "new value").unwrap();
        let filled = browser
            .evaluate(
                "({ value: document.getElementById('plain-text').value,\
                    bubbled: document.getElementById('plain-text').dataset.inputBubbled })",
            )
            .unwrap();
        assert_eq!(filled["result"]["value"]["value"], "new value");
        assert_eq!(filled["result"]["value"]["bubbled"], "true");
        browser.fill_id("plain-textarea", "new area").unwrap();
        assert_eq!(
            browser
                .evaluate("document.getElementById('plain-textarea').value")
                .unwrap()["result"]["value"],
            "new area"
        );
        browser.fill_id("plain-textarea", "").unwrap();
        assert_eq!(
            browser
                .evaluate("document.getElementById('plain-textarea').value")
                .unwrap()["result"]["value"],
            ""
        );
        browser.fill_label("Editable text", "new editable").unwrap();
        assert_eq!(
            browser
                .evaluate("document.getElementById('editable').textContent")
                .unwrap()["result"]["value"],
            "new editable"
        );
        assert!(
            browser
                .fill_id("secret-input", "refused")
                .unwrap_err()
                .to_string()
                .contains("password")
        );
        for id in [
            "non-text-input",
            "readonly-input",
            "disabled-textarea",
            "not-editable",
        ] {
            assert!(browser.fill_id(id, "refused").is_err(), "{id} was accepted");
        }
        browser.click_label("Change label").unwrap();
        browser.click_id("change-id").unwrap();
        let inspection = browser.inspect_label("Change label").unwrap();
        assert_eq!(inspection["accessibility"]["role"], "button");
        assert_eq!(inspection["text"], "changed");
        assert!(!inspection.to_string().contains("NESTED-HIDDEN-SENTINEL"));
        assert!(inspection.to_string().len() < 64 * 1024);
        let missing = browser.inspect_label("Change labl").unwrap_err();
        assert!(Browser::is_lookup_error(&missing));
        assert!(missing.to_string().contains("\"Change label\" (button)"));
        browser.hover_label("Change label").unwrap();
        let hovered = browser.inspect_label("Change label").unwrap();
        assert_eq!(hovered["state"]["hovered"], true);
        assert_eq!(hovered["styles"]["backgroundColor"], "rgb(1, 2, 3)");
        assert!(
            hovered["decoration"]["decorativeChildren"][0]["backgroundImage"]
                .as_str()
                .unwrap()
                .ends_with("/hover.svg\")")
        );
        browser.unhover().unwrap();
        let unhovered = browser.inspect_label("Change label").unwrap();
        assert_eq!(unhovered["state"]["hovered"], false);
        assert!(
            unhovered["decoration"]["decorativeChildren"][0]["backgroundImage"]
                .as_str()
                .unwrap()
                .ends_with("/base.svg\")")
        );
        browser.hover_id("change-id").unwrap();
        assert_eq!(
            browser.inspect_id("change-id").unwrap()["state"]["hovered"],
            true
        );
        browser.unhover().unwrap();
        assert!(
            browser
                .hover_id("occluded")
                .unwrap_err()
                .to_string()
                .contains("occluded")
        );
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
        browser
            .evaluate(
                "(() => {\
                    const container = document.getElementById('layout-container');\
                    for (let index = 0; index < 24; index++) {\
                        const child = document.createElement('span');\
                        child.className = `wide-child-${index}`;\
                        child.textContent = `wide ${index}`;\
                        container.append(child);\
                    }\
                    return true;\
                })()",
            )
            .unwrap();
        let container = browser.inspect_id("layout-container").unwrap();
        let child_layout = container["renderedChildLayout"].as_array().unwrap();
        assert_eq!(child_layout.len(), 16);
        assert!(
            child_layout
                .iter()
                .all(|child| child["depth"].as_u64().unwrap() <= 2)
        );
        assert!(!container.to_string().contains("FORM-SECRET-SENTINEL"));
        assert!(container.to_string().len() < 96 * 1024);
        let content = child_layout
            .iter()
            .find(|child| child["classes"][0] == "dialog-content")
            .unwrap();
        assert_eq!(content["depth"], 1);
        assert_eq!(content["styles"]["padding"], "20px");
        let title = child_layout
            .iter()
            .find(|child| child["classes"][0] == "dialog-title")
            .unwrap();
        assert_eq!(title["depth"], 2);
        assert_eq!(title["accessibility"]["role"], "heading");
        assert_eq!(title["accessibility"]["name"], "Layout title");
        let actions = child_layout
            .iter()
            .find(|child| child["classes"][0] == "dialog-actions")
            .unwrap();
        assert_eq!(actions["styles"]["justifyContent"], "flex-end");
        assert!(actions["geometry"]["width"].as_f64().unwrap() > 0.0);
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
        let failed_inspection = browser.inspect_id("missing-after-dialog").unwrap_err();
        assert!(Browser::is_lookup_error(&failed_inspection));
        assert!(
            crate::finalize_authenticated_preview::<()>(Err(failed_inspection), true, || browser
                .cleanup_authenticated_preview(),)
            .is_err()
        );
        let dialog_closed = browser
            .evaluate("document.getElementById('cleanup-dialog').open === false")
            .unwrap();
        assert_eq!(dialog_closed["result"]["value"], true);
        assert!(logout_seen.load(Ordering::SeqCst));
        assert!(!forbidden_submit_seen.load(Ordering::SeqCst));

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
