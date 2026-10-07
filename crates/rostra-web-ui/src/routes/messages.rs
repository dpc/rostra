//! HTML-first private-message workflows with session-only access.

mod session;
mod settings;
#[cfg(test)]
mod tests;

use axum::extract::{OriginalUri, Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Form, Json};
use maud::{DOCTYPE, Markup, html};
use rostra_core::ShortEventId;
use rostra_core::id::{RostraId, ToShort as _};
use serde::Deserialize;

use self::session::MessageSession;
pub(super) use self::settings::{get_retirement, get_settings, post_settings};
use super::url::{RostraPathId, profile_url, redirect_to_canonical};
use super::{Maud, fragment, recovery, search};
use crate::layout::{PageResources, render_app_navbar, render_html_body};
use crate::util::extractors::AjaxRequest;
use crate::{SharedState, UiState};

type MessageResult = Result<Response, Response>;

const THREAD_PAGE_SIZE: usize = 100;

/// Count exact-session unread messages only while that session retains DM
/// authority.
pub(super) async fn unread_count(
    state: &crate::UiState,
    user: &super::unlock::session::UserSession,
) -> usize {
    let Some(secret) = state.id_secret(user.session_token()) else {
        return 0;
    };
    let Ok(client) = state.client(user.id()).await else {
        return 0;
    };
    let Ok(client) = client.client_ref() else {
        return 0;
    };
    if client.require_dm_authority(secret).is_err() {
        return 0;
    }
    match client
        .db()
        .dm_count_unread(user.session_token().to_le_bytes(), None, 99)
        .await
    {
        Ok(count) => count,
        Err(error) => {
            tracing::warn!(
                target: "rostra::direct_messages::http",
                operation = "count-unread",
                error = %error,
                "Direct-message unread count is unavailable"
            );
            0
        }
    }
}

/// Apply private-response controls, including errors and redirects.
pub(super) fn sensitive_response(body: impl IntoResponse) -> Response {
    let mut response = body.into_response();
    if response.status().is_client_error() || response.status().is_server_error() {
        let is_html = response
            .headers()
            .get(header::CONTENT_TYPE)
            .is_some_and(|value| value.as_bytes().starts_with(b"text/html"));
        if !is_html {
            let page = error_page(
                response.status(),
                match response.status() {
                    StatusCode::PAYLOAD_TOO_LARGE => {
                        "The submitted form is too large. Messages must be at most 16 KiB of UTF-8."
                    }
                    StatusCode::NOT_FOUND => "This private-message page does not exist.",
                    _ => {
                        "The private-message request could not be processed. Return to conversations and try again."
                    }
                },
            );
            response.headers_mut().remove(header::CONTENT_LENGTH);
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            );
            *response.body_mut() = page.into_body();
        }
    }
    let mut response = recovery::sensitive_response(response);
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; script-src 'self' 'unsafe-eval'; connect-src 'self'; style-src 'self' 'unsafe-inline'; font-src 'self'; img-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'",
        ),
    );
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

/// Render the workspace through the shared application shell.
fn page(conversation_panel: Markup, content: Markup, thread_open: bool, unread: usize) -> Response {
    private_page(
        "m-directMessagesLayout",
        PageResources::PrivateRich,
        html! {
            div ."m-directMessages__sidebar" ."-threadOpen"[thread_open] {
                (render_app_navbar(Some("Primary"), html! {}))
                nav id="direct-message-conversations"
                    ."m-directMessages__conversationPanel"
                    aria-label="Conversations"
                {
                    (conversation_panel)
                }
            }
            main ."o-mainBar" {
              div ."o-mainBarTimeline m-directMessages"
                  x-data=[thread_open.then_some("websocket('/updates')")]
              {
                    (fragment::timeline_tab_bar(
                        Some("direct-message-tabs"),
                        None,
                        false,
                        fragment::timeline_tab_links(
                          "messages",
                          super::timeline::PendingCounts { messages: unread, ..Default::default() },
                          false,
                        ),
                    ))
                    div id="direct-message-thread"
                        ."m-directMessages__thread" ."-open"[thread_open] { (content) }
                }
            }
        },
    )
}

/// Share the document, asset policy, and notification runtime with normal
/// pages.
fn private_page(layout_class: &str, resources: PageResources, content: Markup) -> Response {
    Maud(html! {
            (DOCTYPE)
            html lang="en" {
                (UiState::render_html_head(
                    "Rostra", None, None, None, true, resources,
                ))
            (render_html_body(content, layout_class, resources))
        }
    })
    .into_response()
}

/// Render private message administration with the shared Settings navigation.
fn settings_page(title: &str, content: Markup) -> Response {
    private_page(
        "",
        PageResources::Private,
        html! {
            (super::settings::settings_navbar("messages"))
            main ."o-mainBar" {
                div ."o-mainBarTimeline m-directMessages" {
                    div ."o-mainBarTimeline__tabs" {
                        span ."o-mainBarTimeline__settingsTitle" { (title) }
                    }
                    div ."o-settingsContent" { (content) }
                }
            }
        },
    )
}

fn error_page(status: StatusCode, message: &str) -> Response {
    let mut response = page(
        html! {},
        html! {
            p role="alert" { (message) }
            p { a href="/messages" { "Return to conversations" } }
        },
        true,
        0,
    );
    *response.status_mut() = status;
    response
}

struct ConversationPanel {
    rows: Vec<(RostraId, String, usize)>,
    recipient_suggestions: Vec<RecipientSuggestion>,
    unread: usize,
}

struct RecipientSuggestion {
    id: RostraId,
    label: String,
}

const UNNAMED_PROFILE: &str = "Unnamed profile";

/// The default profile name is an identity string, not a chosen display name.
fn message_display_name(peer: RostraId, name: Option<&str>) -> &str {
    name.map(str::trim)
        .filter(|name| !name.is_empty() && *name != peer.to_short().to_string())
        .unwrap_or(UNNAMED_PROFILE)
}

async fn conversation_panel_data(
    db: &rostra_client_db::Database,
    session: &MessageSession,
    entries: &[rostra_client_db::dm::HistoryEntry],
) -> Result<ConversationPanel, Response> {
    let unread = db
        .dm_count_unread(session.read_key(), None, 99)
        .await
        .map_err(storage_error)?;
    let mut rows = Vec::with_capacity(entries.len());
    for entry in entries {
        let peer = peer_of(entry, session.user.id());
        let pending = db
            .dm_count_unread(session.read_key(), Some(peer), 99)
            .await
            .map_err(storage_error)?;
        let profile = db.get_social_profile(peer).await;
        let display_name = message_display_name(
            peer,
            profile
                .as_ref()
                .map(|profile| profile.display_name.as_str()),
        )
        .to_owned();
        rows.push((peer, display_name, pending));
    }
    let mut seen = std::collections::HashSet::new();
    let suggestion_ids = rows
        .iter()
        .map(|(peer, _, _)| *peer)
        .chain(db.get_known_identities_bounded(64).await)
        .filter(|id| *id != session.user.id() && seen.insert(*id))
        .take(64)
        .collect::<Vec<_>>();
    let mut recipient_suggestions = Vec::with_capacity(suggestion_ids.len());
    for id in suggestion_ids {
        let label = db
            .get_social_profile(id)
            .await
            .map(|profile| profile.display_name)
            .filter(|label| !label.trim().is_empty())
            .unwrap_or_else(|| id.to_short().to_string());
        recipient_suggestions.push(RecipientSuggestion { id, label });
    }
    recipient_suggestions.sort_by_cached_key(|suggestion| suggestion.label.to_lowercase());

    Ok(ConversationPanel {
        rows,
        recipient_suggestions,
        unread,
    })
}

fn render_conversation_panel(
    panel: &ConversationPanel,
    next: Option<&str>,
    selected: Option<RostraId>,
) -> Markup {
    html! {
        form ."m-directMessages__start" method="get" action="/messages/open" {
            label ."m-directMessages__startLabel" for="message-peer" { "Start a conversation" }
            div ."m-directMessages__startRow" {
                input id="message-peer" name="peer" type="text" required
                    list="message-peer-suggestions" autocomplete="off"
                    placeholder="Recipient's name or Rostra ID";
                (fragment::button("m-directMessages__openButton", "Open").call())
            }
            datalist id="message-peer-suggestions" {
                @for suggestion in &panel.recipient_suggestions {
                    option value=(suggestion.id.to_short()) label=(&suggestion.label) {}
                }
            }
        }
        @if panel.rows.is_empty() {
            p ."m-directMessages__empty" { "No conversations on this installation yet." }
        }
        ul ."o-settingsNav__group m-directMessages__conversations" {
            @for (peer, display_name, pending) in &panel.rows {
                li {
                    a ."o-settingsNav__item m-directMessages__conversationLink"
                        ."-active"[selected == Some(*peer)]
                        aria-current=[(selected == Some(*peer)).then_some("page")]
                        href=(thread_url(*peer))
                    {
                        span ."m-directMessages__conversationPeer" { (display_name) }
                        @if *pending > 0 {
                            span ."m-directMessages__unread" aria-label=(format!("{pending} unread messages")) {
                                ((*pending).min(99))
                                @if *pending >= 99 { "+" }
                            }
                        }
                    }
                }
            }
        }
        @if let Some(next) = next { a href=(next) { "More conversations" } }
    }
}
fn storage_error(error: rostra_client_db::DbError) -> Response {
    tracing::error!(target: "rostra::direct_messages::http", operation = "read", error = %error,
        "Private-message storage operation failed");
    error_page(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Private message storage is unavailable.",
    )
}

/// Preserve expected availability failures separately from internal failures.
fn publication_error(
    error: rostra_client::error::PostError,
    operation: &'static str,
) -> (StatusCode, &'static str) {
    use rostra_client::error::PostError;
    use rostra_client_db::DbError;
    match error {
        PostError::DirectMessageUnavailable
        | PostError::Storage {
            source: DbError::DmRecipientUnavailable,
        } => (
            StatusCode::BAD_REQUEST,
            "Sending is unavailable. Check this installation and the recipient's eligible devices. No message was queued.",
        ),
        PostError::Storage {
            source: DbError::PayloadAdmissionPaused { .. },
        } => (
            StatusCode::SERVICE_UNAVAILABLE,
            "Local storage admission is temporarily paused. Nothing was queued; retry after capacity is available.",
        ),
        error => {
            tracing::error!(target: "rostra::direct_messages::http", operation, error = %error,
                "Private-message publication failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "The operation could not finish because of an internal or storage failure. Reload before retrying.",
            )
        }
    }
}

fn access_error() -> Response {
    error_page(
        StatusCode::FORBIDDEN,
        "This authorized client is no longer available. Unlock this session again.",
    )
}

/// Keep a rendered page only after probing one additional bounded row.
fn take_page<T>(mut rows: Vec<T>) -> (Vec<T>, bool) {
    let has_more = rows.len() > 32;
    rows.truncate(32);
    (rows, has_more)
}

pub(super) fn thread_url(peer: RostraId) -> String {
    format!("/messages/{}", peer.to_short())
}

fn peer_of(entry: &rostra_client_db::dm::HistoryEntry, own: RostraId) -> RostraId {
    if entry.sender == own {
        entry.recipient
    } else {
        entry.sender
    }
}

/// Exclusive conversation-list cursor in sorted participant-pair order.
#[derive(Default, Deserialize)]
pub(super) struct ConversationsQuery {
    /// First member of the last displayed pair.
    first: Option<RostraId>,
    /// Second member of the last displayed pair.
    second: Option<RostraId>,
}

/// List a bounded page of conversations without reading unrelated history.
pub(super) async fn get_messages(
    session: MessageSession,
    Query(query): Query<ConversationsQuery>,
) -> MessageResult {
    let client = session.client().ok_or_else(access_error)?;
    let entries = client
        .db()
        .dm_conversations(query.first.zip(query.second), 33)
        .await
        .map_err(storage_error)?;
    let (entries, has_more) = take_page(entries);
    let next = has_more.then(|| {
        let last = entries.last().expect("nonempty page");
        format!(
            "/messages?first={}&second={}",
            last.sender.min(last.recipient),
            last.sender.max(last.recipient)
        )
    });
    let panel_data = conversation_panel_data(client.db(), &session, &entries).await?;
    let unread = panel_data.unread;
    let panel = render_conversation_panel(&panel_data, next.as_deref(), None);
    Ok(page(
        panel,
        html! {
            div ."m-directMessages__welcome" {
                h2 { "Your conversations" }
                p { "Choose a conversation on the left." }
                p { "History is local to this installation and does not automatically appear on another device." }
            }
        },
        false,
        unread,
    ))
}

/// Recipient submitted through an ordinary GET form.
#[derive(Deserialize)]
pub(super) struct OpenQuery {
    /// Full or locally resolvable short identity, never a secret.
    peer: RostraPathId,
}

/// Open only a resolvable identity so generated short URLs remain usable.
pub(super) async fn open_conversation(
    session: MessageSession,
    Query(query): Query<OpenQuery>,
) -> MessageResult {
    let client = session.client().ok_or_else(access_error)?;
    let peer = resolve_peer(query.peer, client.db()).await?;
    Ok(Redirect::to(&thread_url(peer)).into_response())
}

async fn resolve_peer(
    path: RostraPathId,
    db: &rostra_client_db::Database,
) -> Result<RostraId, Response> {
    let unavailable = || {
        error_page(
            StatusCode::NOT_FOUND,
            "This identity is not known on this installation. Obtain its events and an eligible message-device announcement before sending.",
        )
    };
    let peer = path.resolve(db).await.ok_or_else(unavailable)?;
    if db.get_known_identity(peer.to_short()).await != Some(peer) {
        return Err(unavailable());
    }
    Ok(peer)
}

/// Exclusive thread-history cursor.
#[derive(Default, Deserialize)]
pub(super) struct ThreadQuery {
    /// Signed timestamp of the oldest displayed message.
    before_time: Option<u64>,
    /// Event-ID tie breaker.
    before_event: Option<ShortEventId>,
    /// Exclusive local incoming-arrival cursor for progressively enhanced live
    /// updates; ordinary requests still return a complete latest-history page.
    after_sequence: Option<u64>,
    /// Browser convenience applied only when entering the latest conversation
    /// view.
    #[serde(skip)]
    focus_composer_on_open: bool,
}

struct ThreadRender<'a> {
    query: ThreadQuery,
    before: Option<(u64, ShortEventId)>,
    history_fragment: bool,
    draft: &'a str,
    error: Option<(StatusCode, &'a str)>,
    sent_draft_token: Option<&'a str>,
}

/// Render retained history and an ordinary HTTP composer with progressive
/// enhancement.
pub(super) async fn get_thread(
    State(state): State<SharedState>,
    session: MessageSession,
    Path(path): Path<RostraPathId>,
    OriginalUri(uri): OriginalUri,
    AjaxRequest(is_ajax): AjaxRequest,
    Query(mut query): Query<ThreadQuery>,
) -> MessageResult {
    let client = session.client().ok_or_else(access_error)?;
    let peer = resolve_peer(path, client.db()).await?;
    if let Some(response) = redirect_to_canonical(&uri, thread_url(peer)) {
        return Ok(response);
    }
    let before = match (query.before_time, query.before_event) {
        (None, None) => None,
        (Some(timestamp), Some(event_id)) => Some((timestamp, event_id)),
        _ => {
            return Err(error_page(
                StatusCode::BAD_REQUEST,
                "Conversation history links require both cursor fields.",
            ));
        }
    };
    if before.is_some() && query.after_sequence.is_some() {
        return Err(error_page(
            StatusCode::BAD_REQUEST,
            "Conversation history and live-update cursors cannot be combined.",
        ));
    }
    let history_fragment = is_ajax && (before.is_some() || query.after_sequence.is_some());
    if !is_ajax {
        query.after_sequence = None;
    }
    query.focus_composer_on_open = query.before_time.is_none() && query.before_event.is_none();
    render_thread(
        &state,
        &session,
        peer,
        ThreadRender {
            query,
            before,
            history_fragment,
            draft: "",
            error: None,
            sent_draft_token: None,
        },
    )
    .await
}

async fn render_thread(
    state: &UiState,
    session: &MessageSession,
    peer: RostraId,
    render: ThreadRender<'_>,
) -> MessageResult {
    let ThreadRender {
        query,
        before,
        history_fragment,
        draft,
        error,
        sent_draft_token,
    } = render;
    let client = session.client().ok_or_else(access_error)?;
    let db = client.db();
    // Snapshot before reading history so an arrival during rendering is also
    // returned by the next update, rather than skipped by its initial cursor.
    let mut after_sequence = match query.after_sequence {
        Some(sequence) => sequence,
        None => db
            .dm_latest_incoming_sequence(peer)
            .await
            .map_err(storage_error)?,
    };
    let entries = if query.after_sequence.is_some() {
        db.dm_incoming_after(peer, after_sequence, THREAD_PAGE_SIZE)
            .await
    } else {
        db.dm_history_with_sequences(peer, before, THREAD_PAGE_SIZE + 1)
            .await
    }
    .map_err(storage_error)?;
    if query.after_sequence.is_some() {
        after_sequence = entries
            .last()
            .and_then(|entry| entry.incoming_sequence)
            .unwrap_or(after_sequence);
    }
    let has_more = entries.len() > THREAD_PAGE_SIZE;
    let mut entries = entries;
    entries.truncate(THREAD_PAGE_SIZE);
    let next = has_more.then(|| {
        let oldest = entries.last().expect("nonempty page");
        format!(
            "{}?before_time={}&before_event={}",
            thread_url(peer),
            oldest.entry.timestamp,
            oldest.entry.event_id
        )
    });
    let unavailable = match db.dm_destinations_now(peer).await {
        Ok(_) => false,
        Err(rostra_client_db::DbError::DmRecipientUnavailable) => true,
        Err(error) => return Err(storage_error(error)),
    };
    let peer_profile = db.get_social_profile(peer).await;
    let peer_label = message_display_name(
        peer,
        peer_profile
            .as_ref()
            .map(|profile| profile.display_name.as_str()),
    );
    let self_profile = db.get_social_profile(session.user.id()).await;
    let self_label = message_display_name(
        session.user.id(),
        self_profile
            .as_ref()
            .map(|profile| profile.display_name.as_str()),
    );
    if error.is_none() {
        let sequences = entries
            .iter()
            .filter_map(|entry| entry.incoming_sequence)
            .collect::<Vec<_>>();
        db.dm_mark_read(session.read_key(), &sequences)
            .await
            .map_err(storage_error)?;
    }
    let mut rendered_entries = Vec::with_capacity(entries.len());
    let ordered_entries = if query.after_sequence.is_some() {
        entries.iter().collect::<Vec<_>>()
    } else {
        entries.iter().rev().collect::<Vec<_>>()
    };
    for entry in ordered_entries {
        rendered_entries.push((
            entry,
            state
                .render_private_content(&client, entry.entry.sender, &entry.entry.text)
                .await,
        ));
    }
    let history = html! {
        @if query.after_sequence.is_none() {
          div id="direct-message-history-pagination" x-merge="replace" {
            @if let Some(next) = next {
                a ."m-directMessages__older"
                    href=(next)
                    x-target="direct-message-history-pagination direct-message-history"
                    "@click.prevent"="loadOlder($el, $ajax)"
                    x-init="$nextTick(() => requestAnimationFrame(() => { const start = () => { if (loadingOlder) { requestAnimationFrame(start); return; } const root = document.body; const margin = Math.round(root.clientHeight * 2.5); new IntersectionObserver((entries, observer) => { if (entries[0].isIntersecting) { observer.disconnect(); loadOlder($el, $ajax); } }, { root, rootMargin: `${margin}px 0px 0px 0px` }).observe($el); }; start(); }))"
                { "Older messages" }
            }
          }
        }
        @if entries.is_empty() && query.after_sequence.is_none() {
            p id="direct-message-empty" { "No messages on this installation yet." }
        }
        ol id="direct-message-history" ."m-directMessages__history"
            x-merge:dynamic="loadingNew ? 'append' : 'prepend'"
        {
            @for (entry, rendered_text) in rendered_entries {
                li id=(format!("direct-message-{}", entry.entry.event_id))
                    ."m-directMessages__message"
                    ."-outgoing"[entry.entry.sender == session.user.id()]
                {
                    p {
                        strong {
                            @if entry.entry.sender == session.user.id() {
                                (self_label)
                            } @else {
                                (peer_label)
                            }
                        }
                        " · "
                        (crate::util::time::format_timestamp(rostra_core::Timestamp::from(entry.entry.timestamp)))
                    }
                    div ."m-directMessages__text m-postView__content" { (rendered_text) }
                    @if entry.entry.conflicted {
                        p role="status" { "A conflicting authenticated message reused this message ID. The first saved text is shown." }
                    }
                }
            }
        }
    };
    let live_cursor = html! {
        span id="direct-message-live-cursor" hidden data-after-sequence=(after_sequence)
            data-more=(query.after_sequence.is_some() && entries.len() == THREAD_PAGE_SIZE) {}
    };
    if history_fragment {
        return Ok(Maud(html! {
            (history)
            @if query.after_sequence.is_some() { (live_cursor) }
        })
        .into_response());
    }
    let csrf = session.csrf().await?;
    let draft_key = format!("direct-message-draft-{}-{peer}", session.user.id());
    let draft_token_key = format!("direct-message-draft-token-{}-{peer}", session.user.id());
    let draft_token = data_encoding::HEXLOWER.encode(&rand::random::<[u8; 32]>());
    let draft_state = format!(
        "{{ text: $persist({}).as({}), draftToken: $persist({}).as({}) }}",
        serde_json::to_string(draft).expect("message draft is JSON serializable"),
        serde_json::to_string(&draft_key).expect("draft key is JSON serializable"),
        serde_json::to_string(&draft_token).expect("draft token is JSON serializable"),
        serde_json::to_string(&draft_token_key).expect("draft token key is JSON serializable"),
    );
    let clear_draft = sent_draft_token.map(|sent_draft_token| {
        let next_draft_token = data_encoding::HEXLOWER.encode(&rand::random::<[u8; 32]>());
        format!(
            "if (draftToken === {sent} && (() => {{ try {{ return localStorage.getItem({key}) === JSON.stringify({sent}); }} catch {{ return true; }} }})()) {{ text = ''; draftToken = {next}; }}",
            sent = serde_json::to_string(sent_draft_token)
                .expect("sent draft token is JSON serializable"),
            key = serde_json::to_string(&draft_token_key)
                .expect("draft token key is JSON serializable"),
            next = serde_json::to_string(&next_draft_token)
                .expect("next draft token is JSON serializable"),
        )
    });
    let autocomplete_state = format!(
        "textAutocomplete({{ profileSearchUrl: '/messages/profile-search', profileSearchCsrf: {} }})",
        serde_json::to_string(&csrf).expect("CSRF token is JSON serializable"),
    );
    let conversations = db.dm_conversations(None, 32).await.map_err(storage_error)?;
    let panel_data = conversation_panel_data(db, session, &conversations).await?;
    let unread_after = panel_data.unread;
    let panel = render_conversation_panel(&panel_data, None, Some(peer));
    let loading = fragment::AjaxLoadingAttrs::for_class("m-directMessages__sendButton");
    let mut response = page(
        panel,
        html! {
            header ."m-directMessages__threadHeader" {
                a ."m-directMessages__mobileBack" href="/messages" { "← Conversations" }
                h1 {
                    a href=(profile_url(peer)) { (peer_label) }
                }
            }
            @if let Some((_, error)) = error { p role="alert" { (error) } }
            @if unavailable {
                p ."m-directMessages__availabilityWarning" role="status" {
                    "Sending is unavailable: this installation may be retired, or no eligible recipient device is known. No message will be queued."
                }
            }
            div
                x-data="directMessageHistory"
                data-refresh-url=(thread_url(peer))
                "@messages:changed.window"="queueRefresh()"
                "@visibilitychange.document"="queueRefresh()"
                "@ajax:merge"="beforeOlderMerge($event)"
                x-init=[before.is_some().then_some(
                    "$nextTick(() => Promise.resolve(typeof MathJax !== 'undefined' ? MathJax.typesetPromise() : undefined).then(() => { if (typeof Prism !== 'undefined') { Prism.highlightAll(); } requestAnimationFrame(() => document.body.scrollTo(0, document.body.scrollHeight)); }))"
                )]
            {
                (live_cursor)
                (history)
            }
            form method="post" action=(thread_url(peer))
                x-data=(draft_state)
                x-init=[clear_draft]
                x-target="direct-message-thread direct-message-conversations direct-message-tabs"
                "@ajax:before"=(loading.before)
                "@ajax:after"=(loading.after)
                "x-on:keyup.enter.ctrl"="if (!$event.repeat && !$event.isComposing && $event.keyCode !== 229) { $el.requestSubmit(); }"
            {
                input type="hidden" name="csrf" value=(csrf);
                input type="hidden" name="draft_token" x-model="draftToken";
                div ."m-directMessages__textareaWrapper"
                    x-data=(autocomplete_state)
                {
                    textarea id="message-text" name="text" rows="5" required aria-label="Message"
                        maxlength="16384" autocomplete="off" disabled[unavailable]
                        autofocus[query.focus_composer_on_open]
                        x-init=[query.focus_composer_on_open.then_some(
                            "$nextTick(() => { document.body.scrollTo(0, document.body.scrollHeight); $el.focus(); })"
                        )]
                        x-model="text"
                        "@input"="handleInput($event); draftToken = Array.from(crypto.getRandomValues(new Uint8Array(32)), byte => byte.toString(16).padStart(2, '0')).join('')"
                        "@keydown"="handleKeydown($event)"
                        role="combobox"
                        aria-autocomplete="list"
                        aria-controls="message-text-autocomplete"
                        ":aria-expanded"="showDropdown"
                        ":aria-activedescendant"="showDropdown && results.length > 0 ? 'message-text-autocomplete-option-' + selectedIndex : null"
                    { (draft) }
                    (fragment::text_autocomplete("message-text-autocomplete", true))
                }
                div ."m-directMessages__composerFooter" {
                    a ."m-directMessages__emojiButton u-requiresJs"
                        href="#"
                        title="Insert emoji"
                        aria-label="Insert emoji"
                        "@click"="toggleEmojiPicker('message-emoji-picker', $event)"
                    { "😀" }
                    (fragment::button("m-directMessages__sendButton", "Send")
                        .title("Send message (Ctrl+Enter)")
                        .disabled(unavailable)
                        .call())
                }
                div id="message-emoji-picker" ."m-directMessages__emojiBar -hidden"
                    data-textarea-selector="#message-text"
                {
                    emoji-picker
                        data-source="/assets/libs/emoji-picker-element/data.json"
                    {}
                }
            }
        },
        true,
        unread_after,
    );
    if let Some((status, _)) = error {
        *response.status_mut() = status;
    }
    Ok(response)
}

#[derive(Deserialize)]
pub(super) struct ProfileSearchForm {
    /// Independent session-bound CSRF token.
    csrf: String,
    /// Private composer text fragment to match against known profiles.
    q: String,
}

/// Search mention candidates without copying private composer text into a URL.
pub(super) async fn post_profile_search(
    session: MessageSession,
    Form(form): Form<ProfileSearchForm>,
) -> MessageResult {
    session.check_csrf(&form.csrf).await?;
    let client = session.client().ok_or_else(access_error)?;
    Ok(
        Json(search::profile_search_results(client.db(), session.user.id(), &form.q).await)
            .into_response(),
    )
}

/// Direct-message send form; never derive Debug for plaintext-bearing input.
#[derive(Deserialize)]
pub(super) struct SendForm {
    /// Independent session-bound CSRF token.
    csrf: String,
    /// User text, encoded only after all authorization checks.
    text: String,
    /// Browser-local draft instance submitted only to clear the sent draft.
    #[serde(default)]
    draft_token: String,
}

/// Send once through an ordinary POST followed by a 303 redirect.
pub(super) async fn post_message(
    State(state): State<SharedState>,
    session: MessageSession,
    Path(path): Path<RostraPathId>,
    AjaxRequest(is_ajax): AjaxRequest,
    Form(form): Form<SendForm>,
) -> MessageResult {
    session.check_csrf(&form.csrf).await?;
    let client = session.client().ok_or_else(access_error)?;
    let peer = resolve_peer(path, client.db()).await?;
    let error = if form.text.is_empty() || form.text.len() > 16 * 1024 {
        Some((
            StatusCode::BAD_REQUEST,
            "Enter a nonempty message of at most 16 KiB of UTF-8. Nothing was sent.",
        ))
    } else {
        client
            .send_direct_message(session.secret, peer, form.text.clone())
            .await
            .err()
            .map(|error| publication_error(error, "send"))
    };
    if let Some(error) = error {
        return render_thread(
            &state,
            &session,
            peer,
            ThreadRender {
                query: ThreadQuery::default(),
                before: None,
                history_fragment: false,
                draft: &form.text,
                error: Some(error),
                sent_draft_token: None,
            },
        )
        .await;
    }
    if is_ajax {
        let sent_draft_token = (form.draft_token.len() == 64
            && form
                .draft_token
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit()))
        .then_some(form.draft_token.as_str());
        return render_thread(
            &state,
            &session,
            peer,
            ThreadRender {
                query: ThreadQuery::default(),
                before: None,
                history_fragment: false,
                draft: "",
                error: None,
                sent_draft_token,
            },
        )
        .await;
    }
    Ok(Redirect::to(&thread_url(peer)).into_response())
}
