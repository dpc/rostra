//! Server-owned external-link composition; remote metadata is optional.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum_extra::extract::Form;
use maud::html;
use scraper::{ElementRef, Html, Node, Selector};
use serde::Deserialize;
use url::Url;

use super::unlock::session::UserSession;
use super::{Maud, fragment};
use crate::SharedState;
use crate::error::{BadRequestSnafu, ReadOnlyModeSnafu, RequestError, RequestResult};
use crate::util::extractors::ajax_request::AjaxRequest;

/// Form state carried through ordinary HTTP and enhanced dialog requests.
#[derive(Deserialize)]
pub(super) struct LinkInput {
    /// Existing composer draft.
    #[serde(default)]
    content: String,
    /// User-supplied destination.
    #[serde(default)]
    url: String,
    /// Editable display title.
    #[serde(default)]
    title: String,
    /// Requested form action.
    #[serde(default)]
    action: String,
    /// Browser-local request scope, not an authorization credential.
    #[serde(default)]
    link_request: String,
}

fn parse_url(value: &str) -> Option<Url> {
    let url = Url::parse(value.trim()).ok()?;
    (matches!(url.scheme(), "http" | "https")
        && url.host().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && value.len() <= 4096)
        .then_some(url)
}

fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !matches!(a, 0 | 10 | 127 | 224..=255)
                && !(a == 100 && (64..=127).contains(&b))
                && !(a == 169 && b == 254)
                && !(a == 172 && (16..=31).contains(&b))
                && !(a == 192 && (b == 0 || b == 168 || (b == 88 && c == 99) || (b == 2)))
                && !(a == 198 && (b == 18 || b == 19 || (b == 51 && c == 100)))
                && !(a == 203 && b == 0 && c == 113)
        }
        IpAddr::V6(ip) => {
            let segments = ip.segments();
            // Conservatively exclude special-use/tunnel ranges as well as all
            // addresses outside global unicast. Manual insertion still works.
            (segments[0] & 0xe000) == 0x2000
                && !matches!(segments[0], 0x2002 | 0x3fff)
                && !(segments[0] == 0x2001 && (segments[1] < 0x0200 || segments[1] == 0x0db8))
        }
    }
}

fn public_addrs(addrs: &[SocketAddr]) -> bool {
    !addrs.is_empty() && addrs.iter().all(|addr| public_ip(addr.ip()))
}

async fn fetch_title(url: &Url) -> Option<String> {
    static FETCH_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);
    let _permit = FETCH_SLOTS.try_acquire().ok()?;
    tokio::time::timeout(Duration::from_secs(8), fetch_title_inner(url))
        .await
        .ok()
        .flatten()
}

async fn fetch_title_inner(original: &Url) -> Option<String> {
    if let Some(url) = x_oembed_url(original) {
        return x_oembed_title(&fetch_metadata(&url, "application/json").await?);
    }
    if let Some(url) = youtube_oembed_url(original) {
        return oembed_title(&fetch_metadata(&url, "application/json").await?);
    }
    metadata_title(&String::from_utf8_lossy(
        &fetch_metadata(original, "text/html").await?,
    ))
}

fn x_oembed_url(original: &Url) -> Option<Url> {
    let id = super::content::x_post_id(original)?;
    let user = original.path_segments()?.next()?;
    let path = if original.path().starts_with("/i/web/status/") {
        format!("/i/web/status/{id}")
    } else {
        format!("/{user}/status/{id}")
    };
    let mut canonical = Url::parse("https://twitter.com").ok()?;
    canonical.set_path(&path);
    let mut endpoint = Url::parse("https://publish.x.com/oembed").ok()?;
    endpoint
        .query_pairs_mut()
        .append_pair("url", canonical.as_str())
        .append_pair("omit_script", "true")
        .append_pair("dnt", "true");
    Some(endpoint)
}

fn x_oembed_title(body: &[u8]) -> Option<String> {
    let response: serde_json::Value = serde_json::from_slice(body).ok()?;
    let document = Html::parse_fragment(response.get("html")?.as_str()?);
    let selector = Selector::parse("blockquote.twitter-tweet p").ok()?;
    let paragraph = document.select(&selector).next()?;
    if paragraph
        .ancestors()
        .filter_map(ElementRef::wrap)
        .any(|element| matches!(element.value().name(), "script" | "style" | "template"))
    {
        return None;
    }
    let mut text = String::new();
    let mut pending = paragraph.children().rev().collect::<Vec<_>>();
    while let Some(node) = pending.pop() {
        match node.value() {
            Node::Element(element) if matches!(element.name(), "script" | "style" | "template") => {
                continue;
            }
            Node::Text(value) => text.push_str(value),
            Node::Element(element) if element.name() == "br" => text.push(' '),
            _ => {}
        }
        pending.extend(node.children().rev());
    }
    normalized_title(without_trailing_x_shortlinks(&text))
}

/// Remove trailing X shortlink labels without changing links inside post text.
fn without_trailing_x_shortlinks(mut text: &str) -> &str {
    text = text.trim_end();
    while !text.is_empty() {
        let (prefix, token) = text.rsplit_once(char::is_whitespace).unwrap_or(("", text));
        let Some(code) = token
            .strip_prefix("https://t.co/")
            .or_else(|| token.strip_prefix("http://t.co/"))
            .or_else(|| token.strip_prefix("pic.twitter.com/"))
            .or_else(|| token.strip_prefix("https://pic.twitter.com/"))
            .or_else(|| token.strip_prefix("http://pic.twitter.com/"))
        else {
            break;
        };
        if code.is_empty() || !code.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
            break;
        }
        text = prefix.trim_end();
    }
    text
}

fn youtube_oembed_url(original: &Url) -> Option<Url> {
    let id = super::content::youtube_video_id(original)?;
    if id.len() != 11
        || !id
            .bytes()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b'-' | b'_'))
    {
        return None;
    }
    let mut canonical = Url::parse("https://www.youtube.com/watch").ok()?;
    canonical.query_pairs_mut().append_pair("v", &id);
    let mut endpoint = Url::parse("https://www.youtube.com/oembed").ok()?;
    endpoint
        .query_pairs_mut()
        .append_pair("url", canonical.as_str())
        .append_pair("format", "json");
    Some(endpoint)
}

fn oembed_title(body: &[u8]) -> Option<String> {
    let response: serde_json::Value = serde_json::from_slice(body).ok()?;
    normalized_title(response.get("title")?.as_str()?)
}

async fn fetch_metadata(original: &Url, content_type: &'static str) -> Option<Vec<u8>> {
    let mut url = original.clone();
    for _ in 0..=3 {
        let host = url.host_str()?;
        let port = url.port_or_known_default()?;
        if !matches!(port, 80 | 443) {
            return None;
        }
        let addrs: Vec<SocketAddr> = match url.host()? {
            url::Host::Ipv4(ip) => vec![SocketAddr::new(ip.into(), port)],
            url::Host::Ipv6(ip) => vec![SocketAddr::new(ip.into(), port)],
            url::Host::Domain(_) => tokio::net::lookup_host((host, port)).await.ok()?.collect(),
        };
        if !public_addrs(&addrs) {
            return None;
        }
        // Pin the validated DNS answers, disable ambient proxies and redirects,
        // and use a fresh cookie-free client without any incoming credentials.
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .referer(false)
            .timeout(Duration::from_secs(5))
            .resolve_to_addrs(host, &addrs)
            .build()
            .ok()?;
        let mut response = client
            .get(url.clone())
            .header(reqwest::header::ACCEPT, content_type)
            .send()
            .await
            .map_err(|error| {
                tracing::debug!(target: "rostra::link_metadata", error = %error.without_url(),
                    "Metadata request failed");
            })
            .ok()?;
        tracing::debug!(target: "rostra::link_metadata", status = %response.status(),
            content_type = ?response.headers().get(reqwest::header::CONTENT_TYPE),
            "Metadata response received");
        if response.status().is_redirection() {
            let target = response
                .headers()
                .get(reqwest::header::LOCATION)?
                .to_str()
                .ok()?;
            url = parse_url(url.join(target).ok()?.as_str())?;
            continue;
        }
        if !response.status().is_success()
            || !response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)?
                .to_str()
                .ok()?
                .split(';')
                .next()?
                .trim()
                .eq_ignore_ascii_case(content_type)
        {
            return None;
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.ok()? {
            if body.len() + chunk.len() > 256 * 1024 {
                return None;
            }
            body.extend_from_slice(&chunk);
        }
        return Some(body);
    }
    None
}

fn metadata_title(source: &str) -> Option<String> {
    let document = Html::parse_document(source);
    let og = Selector::parse("meta[property='og:title'], meta[name='twitter:title']").ok()?;
    let title = Selector::parse("title").ok()?;
    let value = document
        .select(&og)
        .find_map(|node| {
            node.value()
                .attr("content")
                .filter(|value| !value.trim().is_empty())
        })
        .map(str::to_owned)
        .or_else(|| {
            document
                .select(&title)
                .next()
                .map(|node| node.text().collect())
        })?;
    normalized_title(&value)
}

fn normalized_title(value: &str) -> Option<String> {
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    (!value.is_empty()).then(|| value.chars().take(300).collect())
}

fn link_separator(prefix: &str) -> &'static str {
    if prefix
        .chars()
        .next_back()
        .is_some_and(|ch| !ch.is_whitespace())
    {
        " "
    } else {
        ""
    }
}

fn link_syntax(url: &Url, title: &str) -> String {
    let title = if title.trim().is_empty() {
        url.as_str()
    } else {
        title.trim()
    };
    let title: String = title
        .chars()
        .take(300)
        .flat_map(|ch| {
            let ch = if ch.is_control() { ' ' } else { ch };
            if ch.is_ascii_punctuation() {
                vec!['\\', ch]
            } else {
                vec![ch]
            }
        })
        .collect();
    let target = url
        .as_str()
        .replace('(', "%28")
        .replace(')', "%29")
        .replace('\\', "%5C");
    let prefix = if super::content::is_external_embed(url) {
        "!"
    } else {
        ""
    };
    format!("{prefix}[{title}]({target})")
}

/// Suggest metadata or insert an external link without publishing a post.
pub(super) async fn post_link_dialog(
    State(state): State<SharedState>,
    session: UserSession,
    AjaxRequest(is_ajax): AjaxRequest,
    Form(mut form): Form<LinkInput>,
) -> RequestResult<impl IntoResponse> {
    state
        .id_secret(session.session_token())
        .ok_or_else(|| ReadOnlyModeSnafu.build())?;
    if form.content.len() > 1024 * 1024 || form.title.len() > 4096 || form.url.len() > 4096 {
        return Err(RequestError::User {
            source: BadRequestSnafu {
                message: "Link form is too large".to_owned(),
            }
            .build(),
        });
    }
    if !matches!(form.action.as_str(), "title" | "insert") || form.link_request.len() != 32 {
        form.link_request = format!("{:032x}", rand::random::<u128>());
    }
    let mut notice = None;
    let mut status = StatusCode::OK;
    let mut syntax = None;
    if matches!(form.action.as_str(), "title" | "insert") {
        if let Some(url) = parse_url(&form.url) {
            if form.action == "title" {
                if let Some(title) = fetch_title(&url).await {
                    form.title = title;
                } else {
                    status = StatusCode::UNPROCESSABLE_ENTITY;
                    notice = Some("Could not fetch a title. Enter one yourself, or use the URL.");
                }
            } else {
                syntax = Some(link_syntax(&url, &form.title));
            }
        } else {
            status = StatusCode::UNPROCESSABLE_ENTITY;
            notice = Some("Enter an HTTP or HTTPS URL without credentials.");
        }
    }
    let inserted = syntax.as_ref();
    let markup = html! {
        @if is_ajax {
            dialog id="post-preview-dialog" ."o-previewDialog -active"
                aria-labelledby="link-dialog-title"
                data-link-request=(&form.link_request)
                "@ajax:merge"="if (!$el.open && $event.detail.content.dataset.linkRequest === $el.dataset.linkRequest) $event.preventDefault()" {
                div ."o-previewDialog__content" {
                    (link_form(&form, notice, inserted, true))
                }
            }
        } @else {
            main { (link_form(&form, notice, inserted, false)) }
        }
    };
    Ok((
        status,
        Maud(if is_ajax {
            markup
        } else {
            state
                .render_nojs_full_page(&session, "Add link", markup)
                .await?
        }),
    ))
}

fn link_form(
    form: &LinkInput,
    notice: Option<&str>,
    syntax: Option<&String>,
    enhanced: bool,
) -> maud::Markup {
    html! {
        h4 id="link-dialog-title" ."o-mediaList__title" tabindex="-1" { "Add link" }
        @if syntax.is_some() || form.action == "cancel" {
            @if enhanced {
                @if let Some(syntax) = syntax {
                    div data-link-syntax=(syntax)
                        x-init="insertExternalLink($el.dataset.linkSyntax)" {}
                }
            }
            p { "Continue editing your draft below." }
            form ."m-linkForm" action="/post/preview_dialog" method="post" {
                label for="link-result" { "Post draft" }
                textarea id="link-result" name="content" {
                    (&form.content)
                    @if let Some(syntax) = syntax { (link_separator(&form.content)) (syntax) }
                }
                div ."o-mediaList__actionButtons" {
                    (fragment::button("o-previewDialog__submitButton", "Preview post").call())
                    (fragment::button("m-linkForm__linkButton", "Add another link")
                        .icon_class("m-newPostForm__linkButtonIcon")
                        .formaction("/post/link_dialog")
                        .call())
                }
            }
        } @else {
            form ."m-linkForm" action="/post/link_dialog" method="post" x-target=[enhanced.then_some("post-preview-dialog")]
                "x-target.error"=[enhanced.then_some("post-preview-dialog")]
                x-data="{ loading: false }"
                "@ajax:before"="loading = true"
                "@ajax:after"="loading = false" {
                input type="hidden" name="content" value=(&form.content) {}
                input type="hidden" name="link_request" value=(&form.link_request) {}
                div ."m-linkForm__urlRow" {
                    input id="link-url" name="url" type="url" required maxlength="4096"
                        aria-label="Link"
                        aria-describedby=[notice.map(|_| "link-form-error")]
                        aria-invalid=[(notice.is_some() && parse_url(&form.url).is_none()).then_some("true")]
                        value=(&form.url) placeholder="https://…" autofocus
                        ":readonly"="loading"
                        "@change"=[enhanced.then_some("if (!$el.form.elements.title.value.trim() && $el.validity.valid) $el.form.requestSubmit($el.form.querySelector('[value=title]'))")] {}
                    (fragment::button("m-postView__fetchButton", "Fetch")
                        .name("action")
                        .value("title")
                        .disabled_expression("loading")
                        .call())
                }
                @if enhanced {
                    p role="status" x-show="loading" x-cloak { "Working…" }
                }
                input id="link-title" name="title" value=(&form.title) maxlength="300"
                    aria-label="Title (optional)" placeholder="Title (optional)"
                    ":readonly"="loading" {}
                @if let Some(notice) = notice { p id="link-form-error" role="alert" { (notice) } }
                div ."o-mediaList__actionButtons" {
                    (fragment::button("m-linkForm__linkButton", "Insert")
                        .icon_class("m-newPostForm__linkButtonIcon")
                        .name("action")
                        .value("insert")
                        .disabled_expression("loading")
                        .call())
                    @if enhanced {
                        (fragment::button("o-mediaList__closeButton", "Cancel")
                            .button_type("button")
                            .onclick("closeRostraDialog(document.getElementById('post-preview-dialog'))")
                            .call())
                    } @else {
                        (fragment::button("o-mediaList__closeButton", "Cancel")
                            .name("action")
                            .value("cancel")
                            .formnovalidate(true)
                            .call())
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
