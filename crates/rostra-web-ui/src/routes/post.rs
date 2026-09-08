use std::collections::{BTreeSet, HashMap, HashSet};

use axum::extract::{OriginalUri, Path, Query, State};
use axum::http::Method;
use axum::response::IntoResponse;
use axum_extra::extract::Form;
use maud::{Markup, PreEscaped, html};
use rostra_client::ClientRef;
use rostra_client_db::IdSocialProfileRecord;
use rostra_client_db::social::SocialPostRecord;
use rostra_core::event::{EventExt as _, PersonaTag, SocialPost};
use rostra_core::id::{RostraId, ToShort as _};
use rostra_core::{ExternalEventId, ShortEventId, Timestamp};
use serde::Deserialize;
use snafu::ResultExt as _;
use tower_cookies::Cookies;
use tracing::debug;
use url::Url;

use super::unlock::session::{RoMode, UserSession};
use super::{Maud, fragment};
use crate::error::{
    EventContentStorageSnafu, ReadOnlyModeSnafu, RequestError, RequestResult, UserRequestError,
};
use crate::html_utils::re_typeset;
use crate::layout::OpenGraphMeta;
use crate::routes::url::{
    EventPathId, RostraPathId, post_delete_url, post_edit_cancel_url, post_edit_url,
    post_fetch_url, post_heart_reaction_url, post_reaction_delete_url, post_url, profile_url,
    redirect_to_canonical,
};
use crate::util::extractors::AjaxRequest;
use crate::util::time::{format_timestamp, format_timestamp_iso};
use crate::{SharedState, UiState};

mod metadata;
#[cfg(test)]
mod tests;

/// Generate HTML ID for post content element.
///
/// The `post_thread_id` identifies the timeline item/thread context (to
/// disambiguate the same post appearing in multiple places), and `event_id` is
/// the post's ID.
pub fn post_content_html_id(post_thread_id: ShortEventId, event_id: ShortEventId) -> String {
    format!("post-content-{post_thread_id}-{event_id}")
}

/// Generate HTML ID for post replies container.
pub fn post_replies_html_id(post_thread_id: ShortEventId, event_id: ShortEventId) -> String {
    format!("post-replies-{post_thread_id}-{event_id}")
}

/// Generate the HTML ID for a post's reaction bar.
pub fn post_reactions_html_id(post_thread_id: ShortEventId, event_id: ShortEventId) -> String {
    format!("post-reactions-{post_thread_id}-{event_id}")
}

/// Generate HTML ID for the whole post element (used for delete target).
pub fn post_html_id(post_thread_id: ShortEventId, event_id: ShortEventId) -> String {
    format!("post-{post_thread_id}-{event_id}")
}

/// Generate HTML ID for inline reply form container.
pub fn post_inline_reply_form_html_id(
    post_thread_id: ShortEventId,
    event_id: ShortEventId,
) -> String {
    format!("post-inline-reply-form-{post_thread_id}-{event_id}")
}

/// Generate HTML ID for inline reply preview container.
pub fn post_inline_reply_preview_html_id(
    post_thread_id: ShortEventId,
    event_id: ShortEventId,
) -> String {
    format!("post-inline-reply-preview-{post_thread_id}-{event_id}")
}

/// Generate HTML ID for inline reply added placeholder (for x-merge="after").
pub fn post_inline_reply_added_html_id(
    post_thread_id: ShortEventId,
    event_id: ShortEventId,
) -> String {
    format!("post-inline-reply-added-{post_thread_id}-{event_id}")
}

#[derive(Deserialize)]
pub struct SinglePostQuery {
    #[serde(default)]
    raw: bool,
}

#[derive(Deserialize)]
pub struct EditPostQuery {
    post_thread_id: ShortEventId,
    post_target_id: Option<String>,
}

#[derive(Deserialize)]
pub struct EditPostInput {
    content: String,
    post_thread_id: ShortEventId,
    post_target_id: String,
}

#[derive(Deserialize)]
pub struct EditPostPreviewInput {
    content: String,
    post_thread_id: ShortEventId,
    event_id: ShortEventId,
}

/// Rendering context retained while a reaction dialog updates its source post.
#[derive(Deserialize)]
pub struct ReactionContextInput {
    /// Thread instance containing the displayed post and reaction bar.
    post_thread_id: Option<ShortEventId>,
}

fn post_not_found() -> RequestError {
    RequestError::User {
        source: UserRequestError::SomethingNotFound,
    }
}

fn requested_author_matches_event(author: RostraId, event_author: Option<RostraId>) -> bool {
    event_author.is_none_or(|event_author| event_author == author)
}

/// A post-route author identifier in either canonical or legacy form.
pub(super) type PostAuthorId = RostraPathId;

pub async fn get_single_post(
    state: State<SharedState>,
    session: UserSession,
    _cookies: Cookies,
    AjaxRequest(is_ajax): AjaxRequest,
    Query(query): Query<SinglePostQuery>,
    OriginalUri(original_uri): OriginalUri,
    Path((author, event_id)): Path<(PostAuthorId, EventPathId)>,
) -> RequestResult<impl IntoResponse> {
    let client_handle = state.client(session.id()).await?;
    let client_ref = client_handle.client_ref()?;

    let author_is_full = matches!(author, PostAuthorId::Full(_));
    let event_is_full = matches!(event_id, EventPathId::Full(_));
    let author = author
        .resolve(client_ref.db())
        .await
        .ok_or_else(post_not_found)?;
    let event_id = event_id
        .resolve(client_ref.db())
        .await
        .ok_or_else(post_not_found)?;

    let post_record = client_ref.db().get_social_post(event_id).await;
    let event = client_ref.db().get_event(event_id).await;
    if !requested_author_matches_event(author, event.as_ref().map(|event| event.author())) {
        return Err(post_not_found());
    }
    if post_record
        .as_ref()
        .is_some_and(|post| post.author != author)
    {
        return Err(post_not_found());
    }
    if author_is_full || event_is_full {
        if event.is_none() {
            return Err(post_not_found());
        }
        if let Some(response) = redirect_to_canonical(&original_uri, post_url(author, event_id)) {
            return Ok(response);
        }
    }

    // Render raw post if it's an AJAX request or raw=true query parameter
    if is_ajax || query.raw {
        return Ok(Maud(
            state
                .render_post_context(&client_ref, author)
                .event_id(event_id)
                .post_thread_id(event_id)
                .maybe_content(
                    post_record
                        .as_ref()
                        .and_then(|r| r.content.djot_content.as_deref()),
                )
                .maybe_timestamp(post_record.as_ref().map(|r| r.ts))
                .ro(state.ro_mode(session.session_token()))
                .call()
                .await?,
        )
        .into_response());
    }

    // Full page: if we have the post record with content, render post + replies
    if let Some(post_record) = post_record
        .as_ref()
        .filter(|r| r.content.djot_content.is_some())
    {
        // Build Open Graph meta tags and JSON-LD for rich link previews
        let (og, json_ld) = if let Some(djot_content) = post_record.content.djot_content.as_deref()
        {
            use jotup::r#async::AsyncRenderOutputExt as _;
            use jotup::html::filters::AsyncSanitizeExt as _;

            use super::content::RostraRenderExt as _;

            let excerpt = rostra_djot::extract::SocialExcerptRenderer::default()
                .rostra_profile_links(client_ref.clone())
                .sanitize()
                .render_into_document(djot_content)
                .await
                .expect("infallible");

            let metadata_author = post_record.author;
            let og_profile = state
                .get_social_profile_opt(metadata_author, &client_ref)
                .await;
            let display_name = metadata::display_name_or_short_id(
                og_profile
                    .as_ref()
                    .map(|profile| profile.display_name.as_str()),
                &metadata_author.to_short().to_string(),
            );
            let og_event_id = og_profile
                .as_ref()
                .map(|p| p.event_id)
                .unwrap_or(ShortEventId::ZERO);

            let reply_target_name = match post_record.reply_to {
                Some(reply_to) => {
                    let target_id = reply_to.rostra_id();
                    state
                        .get_social_profile_opt(target_id, &client_ref)
                        .await
                        .map(|profile| {
                            metadata::display_name_or_short_id(
                                Some(&profile.display_name),
                                &target_id.to_short().to_string(),
                            )
                        })
                }
                None => None,
            };
            let metadata = metadata::social_metadata(
                &excerpt,
                &display_name,
                post_record.reply_to.is_some(),
                reply_target_name.as_deref(),
            );

            let post_url = state.absolute_url(&post_url(metadata_author, event_id));
            let avatar_url = state.absolute_url(&state.avatar_url(metadata_author, og_event_id));
            let profile_url = state.absolute_url(&profile_url(metadata_author));

            let ld = serde_json::json!({
                "@context": "https://schema.org",
                "@type": "SocialMediaPosting",
                "headline": metadata.title.clone(),
                "articleBody": metadata.description.clone(),
                "url": post_url,
                "datePublished": format_timestamp_iso(post_record.ts),
                "author": {
                    "@type": "Person",
                    "name": display_name,
                    "url": profile_url,
                    "image": avatar_url,
                }
            });

            (
                Some(OpenGraphMeta {
                    title: metadata.title,
                    description: metadata.description,
                    url: post_url,
                    image: Some(avatar_url),
                }),
                Some(ld.to_string()),
            )
        } else {
            (None, None)
        };

        // Load parent post if this is a reply
        let parent_post = if let Some(reply_to) = post_record.reply_to {
            client_ref
                .db()
                .get_social_post(reply_to.event_id().to_short())
                .await
        } else {
            None
        };

        let current_event_id = post_record.event_id;

        // Load replies
        let (comments, _) = client_ref
            .db()
            .paginate_social_post_comments_rev(current_event_id, None, 100)
            .await;

        let ro = state.ro_mode(session.session_token());

        let body = html! {
            // This post (with parent context if it's a reply)
            div ."o-mainBarTimeline__item" {
                (state.render_post_context(
                    &client_ref,
                    post_record.author
                    ).event_id(post_record.event_id)
                    .post_thread_id(current_event_id)
                    .maybe_content(post_record.content.djot_content.as_deref())
                    .maybe_reply_to(
                        post_record.reply_to
                            .map(|reply_to| (
                                reply_to.rostra_id(),
                                reply_to.event_id(),
                                parent_post.as_ref(),
                            ))
                    )
                    .link_to_post(false)
                    .timestamp(post_record.ts)
                    .ro(ro)
                    .call().await?)
            }

            // Replies
            @for comment in &comments {
                @if comment.content.djot_content.is_some() {
                    div ."o-mainBarTimeline__item -reply" style="margin-left: 1rem;" {
                        (state.render_post_context(
                            &client_ref,
                            comment.author
                            ).event_id(comment.event_id)
                            .post_thread_id(current_event_id)
                            .maybe_content(comment.content.djot_content.as_deref())
                            .reply_count(comment.reply_count)
                            .timestamp(comment.ts)
                            .ro(ro)
                            .call().await?)
                    }
                }
            }

            (re_typeset())
        };

        let navbar = state.render_navbar(author, &session).await?;
        let main_content = html! {
            div ."o-mainBarTimeline" {
                (crate::UiState::render_page_tab_bar("Post"))
                (body)
            }
        };
        let page_layout = state.render_page_layout(navbar, main_content);
        let content = html! {
            (page_layout)

            // Dialog containers for post interactions (preview, media, etc.)
            div id="post-preview-dialog" ."o-previewDialog" x-sync {}
            div id="media-list" ."o-mediaList" x-sync {}
            div id="ajax-scripts" style="display: none;" {}

            script type="module" src="/assets/emoji-init.js" {}
        };
        let page_title = og.as_ref().map(|og| og.title.as_str()).unwrap_or("Post");
        return Ok(Maud(
            state
                .render_html_page(
                    page_title,
                    content,
                    None,
                    og.as_ref(),
                    json_ld.as_deref(),
                    false,
                )
                .await?,
        )
        .into_response());
    }

    // Full page: event or content missing — render with Fetch button
    let body = html! {
        div ."o-mainBarTimeline__item" {
            (state
                .render_post_context(&client_ref, author)
                .event_id(event_id)
                .post_thread_id(event_id)
                .maybe_timestamp(post_record.as_ref().map(|r| r.ts))
                .ro(state.ro_mode(session.session_token()))
                .call()
                .await?)
        }
    };

    Ok(Maud(state.render_nojs_full_page(&session, "Post", body).await?).into_response())
}

pub async fn delete_post(
    state: State<SharedState>,
    session: UserSession,
    Path((author_id, event_id)): Path<(PostAuthorId, EventPathId)>,
) -> RequestResult<impl IntoResponse> {
    let client_handle = state.client(session.id()).await?;
    let client = client_handle.client_ref()?;
    let author_id = author_id
        .resolve(client.db())
        .await
        .ok_or_else(post_not_found)?;
    let event_id = event_id
        .resolve(client.db())
        .await
        .ok_or_else(post_not_found)?;

    let Some(post_record) = client.db().get_social_post(event_id).await else {
        return Ok(Maud(html! {
            div ."error" { "Post not found" }
        }));
    };

    if author_id != client.rostra_id() || post_record.author != client.rostra_id() {
        return Ok(Maud(html! {
            div ."error" {
                "You can only delete your own posts"
            }
        }));
    }

    let id_secret = state
        .id_secret(session.session_token())
        .ok_or_else(|| ReadOnlyModeSnafu.build())?;

    // Create and publish a delete event with DELETE_PARENT_AUX_CONTENT_FLAG set
    // and parent_aux pointing to the post we want to delete
    client
        .publish_event(
            id_secret,
            rostra_core::event::SocialPost::new(String::new(), None, Default::default()),
        )
        .replace(post_record.event_id)
        .call()
        .await?;

    // Return empty content to replace the post (x-target handles targeting)
    Ok(Maud(html! {
        div ."m-postView -deleted" {
            div ."m-postView__deletedMessage" {
                "This post has been deleted"
            }
        }
    }))
}

async fn resolve_reaction_target(
    client: &ClientRef<'_>,
    author_id: PostAuthorId,
    event_id: EventPathId,
) -> RequestResult<ExternalEventId> {
    let author_id = author_id
        .resolve(client.db())
        .await
        .ok_or_else(post_not_found)?;
    let event_id = event_id
        .resolve(client.db())
        .await
        .ok_or_else(post_not_found)?;
    let event = client
        .db()
        .get_event(event_id)
        .await
        .ok_or_else(post_not_found)?;
    if event.author() != author_id {
        return Err(post_not_found());
    }

    Ok(ExternalEventId::new(author_id, event_id))
}

fn is_own_reaction(
    reaction: &SocialPostRecord<SocialPost>,
    current_user: RostraId,
    reaction_event_id: ShortEventId,
) -> bool {
    reaction.author == current_user
        && reaction.event_id == reaction_event_id
        && reaction.content.get_reaction().is_some()
}

fn find_own_reaction(
    reactions: &[SocialPostRecord<SocialPost>],
    current_user: RostraId,
    reaction_event_id: ShortEventId,
) -> Option<&SocialPostRecord<SocialPost>> {
    reactions
        .iter()
        .find(|reaction| is_own_reaction(reaction, current_user, reaction_event_id))
}

enum ReactionConfirmationAction {
    Publish,
    Remove,
}

impl ReactionConfirmationAction {
    fn button_class(&self) -> &'static str {
        match self {
            Self::Publish => "o-reactionConfirmation__publishButton",
            Self::Remove => "o-reactionConfirmation__removeButton",
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Self::Publish => "Publish",
            Self::Remove => "Remove",
        }
    }
}

fn reaction_confirmation_submit_form(
    confirmation_action: ReactionConfirmationAction,
    action: &str,
    post_thread_id: ShortEventId,
    x_target: Option<&str>,
    disabled: bool,
) -> Markup {
    let ajax_attrs = fragment::AjaxLoadingAttrs::for_button();
    html! {
        form ."o-reactionConfirmation__form"
            action=(action)
            method="post"
            x-target=[x_target]
            "@ajax:before"=[x_target.map(|_| ajax_attrs.before.as_str())]
            "@ajax:after"=[x_target.map(|_| ajax_attrs.after.as_str())]
        {
            input type="hidden" name="post_thread_id" value=(post_thread_id) {}
            (fragment::button(
                confirmation_action.button_class(),
                confirmation_action.label(),
            )
                .disabled(disabled)
                .call())
        }
    }
}

fn ajax_reaction_confirmation(
    confirmation_action: ReactionConfirmationAction,
    reaction: &str,
    action: &str,
    post_thread_id: ShortEventId,
    reactions_target: &str,
    disabled: bool,
) -> Markup {
    let title = match confirmation_action {
        ReactionConfirmationAction::Publish => "Like this post?",
        ReactionConfirmationAction::Remove => "Remove reaction?",
    };
    let prompt = match confirmation_action {
        ReactionConfirmationAction::Publish => "Publish this heart reaction to the post?",
        ReactionConfirmationAction::Remove => "Remove your reaction from this post?",
    };
    let x_target = format!("post-preview-dialog {reactions_target}");
    html! {
        div id="post-preview-dialog" ."o-previewDialog -active" {
            (fragment::dialog_escape_handler("post-preview-dialog"))
            div ."o-previewDialog__content" {
                h4 ."o-previewDialog__title" { (title) }
                p ."o-reactionConfirmation__prompt" { (prompt) }
                div ."o-reactionConfirmation__reaction" aria-hidden="true" { (reaction) }
                div ."o-previewDialog__actionButtons" {
                    (fragment::button("o-previewDialog__cancelButton", "Back")
                        .button_type("button")
                        .onclick("document.querySelector('#post-preview-dialog').classList.remove('-active')")
                        .call())
                    (reaction_confirmation_submit_form(
                        confirmation_action,
                        action,
                        post_thread_id,
                        Some(&x_target),
                        disabled,
                    ))
                }
            }
        }
    }
}

fn nojs_reaction_confirmation(
    confirmation_action: ReactionConfirmationAction,
    reaction: &str,
    action: &str,
    post_thread_id: ShortEventId,
    cancel_url: &str,
    disabled: bool,
) -> Markup {
    let (title, prompt) = match confirmation_action {
        ReactionConfirmationAction::Publish => (
            "Like this post?",
            "Publish this heart reaction to the post?",
        ),
        ReactionConfirmationAction::Remove => {
            ("Remove reaction?", "Remove your reaction from this post?")
        }
    };
    html! {
        section ."o-reactionConfirmation" {
            h1 { (title) }
            p ."o-reactionConfirmation__prompt" { (prompt) }
            div ."o-reactionConfirmation__reaction" aria-hidden="true" { (reaction) }
            div ."o-reactionConfirmation__actions" {
                a ."o-reactionConfirmation__cancel" href=(cancel_url) { "Cancel" }
                (reaction_confirmation_submit_form(
                    confirmation_action,
                    action,
                    post_thread_id,
                    None,
                    disabled,
                ))
            }
        }
    }
}

/// Render the ordinary HTTP confirmation page for publishing a heart reaction.
pub async fn get_heart_reaction_confirmation(
    state: State<SharedState>,
    session: UserSession,
    AjaxRequest(is_ajax): AjaxRequest,
    Query(input): Query<ReactionContextInput>,
    Path((author_id, event_id)): Path<(PostAuthorId, EventPathId)>,
) -> RequestResult<axum::response::Response> {
    let client_handle = state.client(session.id()).await?;
    let client = client_handle.client_ref()?;
    let target = resolve_reaction_target(&client, author_id, event_id).await?;
    let post_thread_id = input
        .post_thread_id
        .unwrap_or_else(|| target.event_id().to_short());
    let cancel_url = post_url(target.rostra_id(), target.event_id().to_short());
    let action = post_heart_reaction_url(target.rostra_id(), target.event_id().to_short());
    let disabled = state.ro_mode(session.session_token()).to_disabled();
    if is_ajax {
        return Ok(Maud(ajax_reaction_confirmation(
            ReactionConfirmationAction::Publish,
            "❤️",
            &action,
            post_thread_id,
            &post_reactions_html_id(post_thread_id, target.event_id().to_short()),
            disabled,
        ))
        .into_response());
    }

    let body = nojs_reaction_confirmation(
        ReactionConfirmationAction::Publish,
        "❤️",
        &action,
        post_thread_id,
        &cancel_url,
        disabled,
    );
    Ok(Maud(
        state
            .render_nojs_full_page(&session, "Publish Reaction", body)
            .await?,
    )
    .into_response())
}

/// Publish a heart reaction after the user submits the confirmation form.
pub async fn post_heart_reaction(
    state: State<SharedState>,
    session: UserSession,
    AjaxRequest(is_ajax): AjaxRequest,
    Path((author_id, event_id)): Path<(PostAuthorId, EventPathId)>,
    Form(input): Form<ReactionContextInput>,
) -> RequestResult<axum::response::Response> {
    let id_secret = state
        .id_secret(session.session_token())
        .ok_or_else(|| ReadOnlyModeSnafu.build())?;
    let client_handle = state.client(session.id()).await?;
    let client = client_handle.client_ref()?;
    let target = resolve_reaction_target(&client, author_id, event_id).await?;

    client
        .social_post(id_secret, "❤️".to_owned(), Some(target), Default::default())
        .await?;

    if is_ajax {
        let post_thread_id = input
            .post_thread_id
            .unwrap_or_else(|| target.event_id().to_short());
        return Ok(Maud(html! {
            (state.render_post_reactions(
                &client,
                target,
                post_thread_id,
                state.ro_mode(session.session_token()),
            ).await)
            div id="post-preview-dialog" ."o-previewDialog" {}
        })
        .into_response());
    }

    Ok(
        axum::response::Redirect::to(&post_url(target.rostra_id(), target.event_id().to_short()))
            .into_response(),
    )
}

/// Render the ordinary HTTP confirmation page for removing one own reaction.
pub async fn get_reaction_delete_confirmation(
    state: State<SharedState>,
    session: UserSession,
    AjaxRequest(is_ajax): AjaxRequest,
    Query(input): Query<ReactionContextInput>,
    Path((author_id, event_id, reaction_event_id)): Path<(PostAuthorId, EventPathId, EventPathId)>,
) -> RequestResult<axum::response::Response> {
    let client_handle = state.client(session.id()).await?;
    let client = client_handle.client_ref()?;
    let target = resolve_reaction_target(&client, author_id, event_id).await?;
    let reaction_event_id = reaction_event_id
        .resolve(client.db())
        .await
        .ok_or_else(post_not_found)?;
    let (reactions, _) = client
        .db()
        .paginate_social_post_reactions_rev(target.event_id().to_short(), None, 1000)
        .await;
    let reaction = find_own_reaction(&reactions, client.rostra_id(), reaction_event_id)
        .ok_or_else(post_not_found)?;
    let reaction_text = reaction
        .content
        .get_reaction()
        .expect("validated reaction must have reaction text");
    let post_thread_id = input
        .post_thread_id
        .unwrap_or_else(|| target.event_id().to_short());
    let cancel_url = post_url(target.rostra_id(), target.event_id().to_short());
    let action = post_reaction_delete_url(
        target.rostra_id(),
        target.event_id().to_short(),
        reaction_event_id,
    );
    let disabled = state.ro_mode(session.session_token()).to_disabled();
    if is_ajax {
        return Ok(Maud(ajax_reaction_confirmation(
            ReactionConfirmationAction::Remove,
            reaction_text,
            &action,
            post_thread_id,
            &post_reactions_html_id(post_thread_id, target.event_id().to_short()),
            disabled,
        ))
        .into_response());
    }

    let body = nojs_reaction_confirmation(
        ReactionConfirmationAction::Remove,
        reaction_text,
        &action,
        post_thread_id,
        &cancel_url,
        disabled,
    );
    Ok(Maud(
        state
            .render_nojs_full_page(&session, "Remove Reaction", body)
            .await?,
    )
    .into_response())
}

/// Delete the precise own reaction selected by the user.
pub async fn delete_reaction(
    state: State<SharedState>,
    session: UserSession,
    AjaxRequest(is_ajax): AjaxRequest,
    Path((author_id, event_id, reaction_event_id)): Path<(PostAuthorId, EventPathId, EventPathId)>,
    Form(input): Form<ReactionContextInput>,
) -> RequestResult<axum::response::Response> {
    let id_secret = state
        .id_secret(session.session_token())
        .ok_or_else(|| ReadOnlyModeSnafu.build())?;
    let client_handle = state.client(session.id()).await?;
    let client = client_handle.client_ref()?;
    let target = resolve_reaction_target(&client, author_id, event_id).await?;
    let reaction_event_id = reaction_event_id
        .resolve(client.db())
        .await
        .ok_or_else(post_not_found)?;
    let (reactions, _) = client
        .db()
        .paginate_social_post_reactions_rev(target.event_id().to_short(), None, 1000)
        .await;
    let reaction = find_own_reaction(&reactions, client.rostra_id(), reaction_event_id)
        .ok_or_else(post_not_found)?;

    client
        .publish_event(
            id_secret,
            SocialPost::new(String::new(), None, Default::default()),
        )
        .replace(reaction.event_id)
        .call()
        .await?;

    if is_ajax {
        let post_thread_id = input
            .post_thread_id
            .unwrap_or_else(|| target.event_id().to_short());
        return Ok(Maud(html! {
            (state.render_post_reactions(
                &client,
                target,
                post_thread_id,
                state.ro_mode(session.session_token()),
            ).await)
            div id="post-preview-dialog" ."o-previewDialog" {}
        })
        .into_response());
    }

    Ok(
        axum::response::Redirect::to(&post_url(target.rostra_id(), target.event_id().to_short()))
            .into_response(),
    )
}

fn render_post_error_id(post_target_id: &str, message: &str) -> Markup {
    html! {
        div id=(post_target_id) ."m-postView" {
            div ."error" { (message) }
        }
    }
}

fn focus_on_edit_post_content(textarea_id: &str) -> Markup {
    html! {
        script {
            (PreEscaped(format!(r#"
                (function() {{
                    document.getElementById('{textarea_id}')?.focus();
                }})()
            "#)))
        }
    }
}

fn render_inline_edit_post_form(
    author_id: RostraId,
    event_id: ShortEventId,
    post_thread_id: ShortEventId,
    post_target_id: &str,
    content: &str,
    error: Option<&str>,
) -> Markup {
    let textarea_id = format!("edit-post-content-{post_thread_id}-{event_id}");
    let save_ajax = fragment::AjaxLoadingAttrs::for_class("m-inlineReply__previewButton");
    let cancel_ajax = fragment::AjaxLoadingAttrs::for_document_class("m-inlineReply__cancelButton");
    let cancel_form_id = format!("edit-post-cancel-{post_thread_id}-{event_id}");
    let preview_form_id = format!("edit-post-preview-form-{post_thread_id}-{event_id}");
    let preview_id = format!("edit-post-preview-{post_thread_id}-{event_id}");

    html! {
        div id=(post_target_id) ."m-postView" {
            div ."m-inlineReply -active" {
                @if let Some(error) = error {
                    div ."error" { (error) }
                }

                form id=(preview_form_id)
                    action="/post/edit_preview"
                    method="post"
                    x-target=(preview_id)
                    style="display: none;"
                {
                    input type="hidden" name="content" value=(content) {}
                    input type="hidden" name="post_thread_id" value=(post_thread_id) {}
                    input type="hidden" name="event_id" value=(event_id) {}
                }

                form ."m-inlineReply__form"
                    action=(post_edit_url(author_id, event_id))
                    method="post"
                    x-target=(format!("{} ajax-scripts", post_target_id))
                    "@ajax:before"=(save_ajax.before)
                    "@ajax:after"=(save_ajax.after)
                {
                    input type="hidden" name="post_thread_id" value=(post_thread_id) {}
                    input type="hidden" name="post_target_id" value=(post_target_id) {}

                    div ."m-inlineReply__textareaWrapper"
                        x-data="textAutocomplete"
                        style="position: relative;"
                    {
                        @let input_handler = format!(r#"
                            handleInput($event);
                            const previewForm = document.getElementById('{preview_form_id}');
                            previewForm.querySelector('input[name=content]').value = $el.value;
                            previewForm.requestSubmit();
                        "#);
                        textarea
                            id=(textarea_id)
                            ."m-inlineReply__content"
                            name="content"
                            placeholder="Edit post..."
                            dir="auto"
                            autocomplete="off"
                            "@input"=(input_handler)
                            "@keydown"="handleKeydown($event)"
                            "x-on:keyup.enter.ctrl"="$el.form.requestSubmit()"
                        { (content) }
                    }

                    div ."m-inlineReply__footer" {
                        div ."m-inlineReply__footerLeft" {
                            (fragment::button("m-inlineReply__cancelButton", "Cancel")
                                .form(&cancel_form_id)
                                .call())
                        }
                        (fragment::button("m-inlineReply__previewButton", "Save").call())
                    }
                }

                form id=(cancel_form_id)
                    action=(post_edit_cancel_url(author_id, event_id))
                    method="get"
                    x-target=(post_target_id)
                    "@ajax:before"=(cancel_ajax.before)
                    "@ajax:after"=(cancel_ajax.after)
                    style="display: none;"
                {
                    input type="hidden" name="post_thread_id" value=(post_thread_id) {}
                    input type="hidden" name="post_target_id" value=(post_target_id) {}
                }

                div id=(preview_id) ."m-inlineReply__preview" {}

                (focus_on_edit_post_content(&textarea_id))
            }
        }
    }
}

pub async fn get_edit_post(
    state: State<SharedState>,
    session: UserSession,
    AjaxRequest(is_ajax): AjaxRequest,
    OriginalUri(original_uri): OriginalUri,
    Path((author_id, event_id)): Path<(PostAuthorId, EventPathId)>,
    Query(query): Query<EditPostQuery>,
) -> RequestResult<impl IntoResponse> {
    let client_handle = state.client(session.id()).await?;
    let client = client_handle.client_ref()?;
    let author_is_full = matches!(author_id, PostAuthorId::Full(_));
    let event_is_full = matches!(event_id, EventPathId::Full(_));
    let author_id = author_id
        .resolve(client.db())
        .await
        .ok_or_else(post_not_found)?;
    let event_id = event_id
        .resolve(client.db())
        .await
        .ok_or_else(post_not_found)?;

    let post_target_id = query
        .post_target_id
        .clone()
        .unwrap_or_else(|| post_html_id(query.post_thread_id, event_id));

    let Some(post_record) = client.db().get_social_post(event_id).await else {
        return Ok(Maud(render_post_error_id(&post_target_id, "Post not found")).into_response());
    };

    if author_id != client.rostra_id() || post_record.author != client.rostra_id() {
        return Ok(Maud(render_post_error_id(
            &post_target_id,
            "You can only edit your own posts",
        ))
        .into_response());
    }
    if author_is_full || event_is_full {
        if let Some(response) =
            redirect_to_canonical(&original_uri, post_edit_url(author_id, event_id))
        {
            return Ok(response);
        }
    }
    if state.ro_mode(session.session_token()).is_ro() {
        return Ok(Maud(render_post_error_id(
            &post_target_id,
            "Editing is disabled in ro-mode",
        ))
        .into_response());
    }

    let content = post_record
        .content
        .djot_content
        .as_deref()
        .unwrap_or_default();

    let form = render_inline_edit_post_form(
        author_id,
        post_record.event_id,
        query.post_thread_id,
        &post_target_id,
        content,
        None,
    );

    if is_ajax {
        Ok(Maud(form).into_response())
    } else {
        Ok(Maud(
            state
                .render_nojs_full_page(&session, "Edit Post", form)
                .await?,
        )
        .into_response())
    }
}

pub async fn get_edit_post_cancel(
    state: State<SharedState>,
    session: UserSession,
    OriginalUri(original_uri): OriginalUri,
    Path((author_id, event_id)): Path<(PostAuthorId, EventPathId)>,
    Query(query): Query<EditPostQuery>,
) -> RequestResult<impl IntoResponse> {
    let client_handle = state.client(session.id()).await?;
    let client = client_handle.client_ref()?;
    let author_is_full = matches!(author_id, PostAuthorId::Full(_));
    let event_is_full = matches!(event_id, EventPathId::Full(_));
    let author_id = author_id
        .resolve(client.db())
        .await
        .ok_or_else(post_not_found)?;
    let event_id = event_id
        .resolve(client.db())
        .await
        .ok_or_else(post_not_found)?;

    let post_target_id = query
        .post_target_id
        .clone()
        .unwrap_or_else(|| post_html_id(query.post_thread_id, event_id));

    let Some(post_record) = client.db().get_social_post(event_id).await else {
        return Ok(Maud(render_post_error_id(&post_target_id, "Post not found")).into_response());
    };
    if post_record.author != author_id {
        return Err(post_not_found());
    }
    if author_is_full || event_is_full {
        if let Some(response) =
            redirect_to_canonical(&original_uri, post_edit_cancel_url(author_id, event_id))
        {
            return Ok(response);
        }
    }

    Ok(Maud(
        state
            .render_post_view(&client, author_id)
            .maybe_persona_tags(Some(&post_record.content.persona_tags()))
            .event_id(post_record.event_id)
            .post_thread_id(query.post_thread_id)
            .maybe_content(post_record.content.djot_content.as_deref())
            .maybe_url(post_record.content.url.as_ref())
            .maybe_title(post_record.content.title.as_deref())
            .reply_count(post_record.reply_count)
            .timestamp(post_record.ts)
            .post_target_id(post_target_id)
            .ro(state.ro_mode(session.session_token()))
            .call()
            .await?,
    )
    .into_response())
}

pub async fn post_edit_post_preview(
    state: State<SharedState>,
    session: UserSession,
    Form(form): Form<EditPostPreviewInput>,
) -> RequestResult<impl IntoResponse> {
    let client_handle = state.client(session.id()).await?;
    let client = client_handle.client_ref()?;
    let self_id = client.rostra_id();
    let preview_id = format!(
        "edit-post-preview-{}-{}",
        form.post_thread_id, form.event_id
    );

    Ok(Maud(html! {
        @if !form.content.is_empty() {
            div id=(preview_id) ."m-inlineReply__preview -active" {
                (state.render_post_context(
                    &client,
                    self_id,
                    )
                    .content(&form.content)
                    .timestamp(rostra_core::Timestamp::now())
                    .ro(state.ro_mode(session.session_token()))
                    .call().await?)
                (re_typeset())
            }
        } @else {
            div id=(preview_id) ."m-inlineReply__preview" {}
        }
    }))
}

pub async fn post_edit_post(
    state: State<SharedState>,
    session: UserSession,
    AjaxRequest(is_ajax): AjaxRequest,
    Path((author_id, event_id)): Path<(PostAuthorId, EventPathId)>,
    Form(form): Form<EditPostInput>,
) -> RequestResult<impl IntoResponse> {
    let client_handle = state.client(session.id()).await?;
    let client = client_handle.client_ref()?;
    let author_id = author_id
        .resolve(client.db())
        .await
        .ok_or_else(post_not_found)?;
    let event_id = event_id
        .resolve(client.db())
        .await
        .ok_or_else(post_not_found)?;

    if form.content.trim().is_empty() {
        return Ok(Maud(html! {
            (render_inline_edit_post_form(
                author_id,
                event_id,
                form.post_thread_id,
                &form.post_target_id,
                &form.content,
                Some("Post content cannot be empty"),
            ))
            div id="ajax-scripts" {}
        }));
    }

    let id_secret = state
        .id_secret(session.session_token())
        .ok_or_else(|| ReadOnlyModeSnafu.build())?;

    let Some(post_record) = client.db().get_social_post(event_id).await else {
        return Ok(Maud(html! {
            (render_post_error_id(&form.post_target_id, "Post not found"))
            div id="ajax-scripts" {}
        }));
    };

    if author_id != client.rostra_id() || post_record.author != client.rostra_id() {
        return Ok(Maud(html! {
            (render_post_error_id(&form.post_target_id, "You can only edit your own posts"))
            div id="ajax-scripts" {}
        }));
    }

    let persona_tags = post_record.content.persona_tags();
    let content = rostra_core::event::SocialPost::new_text(
        form.content.clone(),
        post_record.reply_to,
        persona_tags.clone(),
    );
    let content = if post_record.content.news {
        content.with_news_fields(
            post_record.content.url.clone(),
            post_record.content.title.clone(),
        )
    } else {
        content
    };

    let event = client
        .publish_event(id_secret, content)
        .replace(post_record.event_id)
        .call()
        .await?;
    let new_event_id = event.event_id.to_short();

    if !is_ajax {
        return Ok(Maud(html! {
            (maud::DOCTYPE)
            html {
                head {
                    meta http-equiv="refresh" content=(format!("0;url={}", post_url(author_id, new_event_id))) {}
                }
                body {
                    p { "Post edited. Redirecting..." }
                    a href=(post_url(author_id, new_event_id)) { "Click here if not redirected." }
                }
            }
        }));
    }

    Ok(Maud(html! {
        (state.render_post_view(
            &client,
            author_id,
        )
            .persona_tags(&persona_tags)
            .event_id(new_event_id)
            .post_thread_id(form.post_thread_id)
            .content(&form.content)
            .maybe_url(post_record.content.url.as_ref())
            .maybe_title(post_record.content.title.as_deref())
            .reply_count(post_record.reply_count)
            .timestamp(rostra_core::Timestamp::now())
            .post_target_id(form.post_target_id.clone())
            .ro(state.ro_mode(session.session_token()))
            .call()
            .await?)

        div id="ajax-scripts" {
            script {
                (PreEscaped(r#"
                    window.dispatchEvent(new CustomEvent('notify', {
                        detail: { type: 'success', message: 'Post edited successfully' }
                    }));
                "#))
            }
            (re_typeset())
        }
    }))
}

pub async fn fetch_missing_post(
    state: State<SharedState>,
    session: UserSession,
    method: Method,
    OriginalUri(original_uri): OriginalUri,
    Path((post_thread_id, author_id, event_id)): Path<(EventPathId, PostAuthorId, EventPathId)>,
) -> RequestResult<impl IntoResponse> {
    let client_handle = state.client(session.id()).await?;
    let client = client_handle.client_ref()?;
    let post_thread_is_full = matches!(post_thread_id, EventPathId::Full(_));
    let author_is_full = matches!(author_id, PostAuthorId::Full(_));
    let event_is_full = matches!(event_id, EventPathId::Full(_));
    let post_thread_id = post_thread_id
        .resolve(client.db())
        .await
        .ok_or_else(post_not_found)?;
    let author_id = author_id
        .resolve(client.db())
        .await
        .ok_or_else(post_not_found)?;
    let event_id = event_id
        .resolve(client.db())
        .await
        .ok_or_else(post_not_found)?;
    let event = client.db().get_event(event_id).await;
    if event
        .as_ref()
        .is_some_and(|event| event.author() != author_id)
    {
        return Err(post_not_found());
    }
    if (method == Method::GET || method == Method::HEAD)
        && (post_thread_is_full || author_is_full || event_is_full)
    {
        if event.is_none() {
            return Err(post_not_found());
        }
        if let Some(response) = redirect_to_canonical(
            &original_uri,
            post_fetch_url(post_thread_id, author_id, event_id),
        ) {
            return Ok(response);
        }
    }

    let mut followers_cache = std::collections::BTreeMap::new();

    let content_id = post_content_html_id(post_thread_id, event_id);

    let fetched = client
        .fetch_event_content(author_id, event_id, &mut followers_cache)
        .await
        .context(EventContentStorageSnafu {
            author_id,
            event_id,
        })?;
    if !fetched {
        debug!(
            author = %author_id.to_short(),
            %event_id,
            "Missing post content was unavailable from peers"
        );
    } else {
        // Post was fetched successfully, render the updated content
        let db = client.db();
        let event = db.get_event(event_id).await;
        if event
            .as_ref()
            .is_some_and(|event| event.author() != author_id)
        {
            return Err(post_not_found());
        }
        if let (Some(_event), Some(post_record)) = (event, db.get_social_post(event_id).await) {
            if post_record.author == author_id {
                if let Some(djot_content) = post_record.content.djot_content.as_ref() {
                    let post_content_rendered = state
                        .render_content(&client, post_record.author, djot_content)
                        .await;
                    return Ok(Maud(html! {
                        div #(content_id) ."m-postView__content -present" {
                            (post_content_rendered)
                        }
                    })
                    .into_response());
                }
            }
        }
    }

    // Fetch failed or post still not available
    Ok(Maud(html! {
        div #(content_id) ."m-postView__content -missing" {
            p {
                "Post not found"
            }
        }
    })
    .into_response())
}

#[bon::bon]
impl UiState {
    async fn render_post_reactions(
        &self,
        client: &ClientRef<'_>,
        target: ExternalEventId,
        post_thread_id: ShortEventId,
        ro: RoMode,
    ) -> Markup {
        let (reactions, _) = client
            .db()
            .paginate_social_post_reactions_rev(target.event_id().to_short(), None, 1000)
            .await;
        let mut reaction_social_profiles: HashMap<RostraId, IdSocialProfileRecord> = HashMap::new();

        for reaction_author in reactions
            .iter()
            .map(|reaction| reaction.author)
            .collect::<HashSet<_>>()
        {
            if let Some(reaction_user_profile) =
                self.get_social_profile_opt(reaction_author, client).await
            {
                assert!(
                    reaction_social_profiles
                        .insert(reaction_author, reaction_user_profile)
                        .is_none()
                );
            }
        }

        html! {
            div
                id=(post_reactions_html_id(post_thread_id, target.event_id().to_short()))
                .m-postView__reactions
            {
                @for reaction in reactions {
                    @if let Some(reaction_text) = reaction.content.get_reaction() {
                        @let reaction_author = reaction_social_profiles.get(&reaction.author)
                            .map(|r| r.display_name.clone())
                            .unwrap_or_else(|| reaction.author.to_string());
                        @if reaction.author == client.rostra_id() {
                            @if ro.is_ro() {
                                span .m-postView__reaction ."-own" ."-disabled"
                                    title="Your reaction (read-only mode)"
                                {
                                    (reaction_text)
                                }
                            } @else {
                                form
                                    ."m-postView__reactionForm"
                                    action=(post_reaction_delete_url(
                                        target.rostra_id(),
                                        target.event_id().to_short(),
                                        reaction.event_id,
                                    ))
                                    method="get"
                                    x-target="post-preview-dialog"
                                {
                                    input type="hidden" name="post_thread_id" value=(post_thread_id) {}
                                    button
                                        .m-postView__reaction ."-own"
                                        type="submit"
                                        title=(format!("Your reaction; click to remove ({reaction_author})"))
                                        aria-label=(format!("Remove your {reaction_text} reaction"))
                                    {
                                        (reaction_text)
                                    }
                                }
                            }
                        } @else {
                            span .m-postView__reaction title=(format!("by {reaction_author}")) {
                                (reaction_text)
                            }
                        }
                    }
                }
            }
        }
    }

    /// Render a whole post with all its context (parent, children buttons,
    /// etc.)
    #[allow(clippy::too_many_arguments)]
    #[builder]
    pub async fn render_post_context(
        &self,
        #[builder(start_fn)] client: &ClientRef<'_>,
        #[builder(start_fn)] author: RostraId,
        persona_tags: Option<&BTreeSet<PersonaTag>>,
        reply_to: Option<(
            RostraId,
            ShortEventId,
            Option<&SocialPostRecord<SocialPost>>,
        )>,
        event_id: Option<ShortEventId>,
        /// Post thread ID for HTML element IDs (to disambiguate same post in
        /// multiple places). If not provided, defaults to event_id.
        post_thread_id: Option<ShortEventId>,
        content: Option<&str>,
        url: Option<&Url>,
        title: Option<&str>,
        reply_count: Option<u64>,
        timestamp: Option<Timestamp>,
        extra_buttons: Option<Markup>,
        link_to_post: Option<bool>,
        ro: RoMode,
    ) -> RequestResult<Markup> {
        // Use post_thread_id if provided, otherwise default to event_id
        let post_thread_id = post_thread_id.or(event_id);

        // Generate unique ID for the article element (matches m-postContext class)
        let post_context_id = match (post_thread_id, event_id) {
            (Some(ctx), Some(id)) => format!("post-context-{ctx}-{id}"),
            (None, Some(id)) => format!("post-context-{id}"),
            _ => "post-context-preview".to_string(),
        };
        let post_view = self
            .render_post_view(client, author)
            .maybe_persona_tags(persona_tags)
            .maybe_event_id(event_id)
            .maybe_post_thread_id(post_thread_id)
            .maybe_content(content)
            .maybe_url(url)
            .maybe_title(title)
            .maybe_reply_count(reply_count)
            .maybe_timestamp(timestamp)
            .maybe_extra_buttons(extra_buttons)
            .maybe_link_to_post(link_to_post)
            .ro(ro)
            .call()
            .await?;

        Ok(html! {

            article #(post_context_id)
                ."m-postContext"
             {
                @if let Some((reply_to_author, reply_to_event_id, reply_to_post)) = reply_to {
                    div ."m-postContext__postParent"
                        onclick="this.classList.add('-expanded')"
                    {
                        @let reply_to_tags = reply_to_post.map(|r| r.content.persona_tags());
                        (Box::pin(self.render_post_view(
                            client,
                            reply_to_author,
                            )
                            .maybe_persona_tags(reply_to_tags.as_ref())
                            .event_id(reply_to_event_id)
                            .maybe_post_thread_id(post_thread_id)
                            .ro(ro)
                            .maybe_content(reply_to_post.and_then(|r| r.content.djot_content.as_deref()))
                            .maybe_url(reply_to_post.and_then(|r| r.content.url.as_ref()))
                            .maybe_title(reply_to_post.and_then(|r| r.content.title.as_deref()))
                            .maybe_timestamp(reply_to_post.map(|r| r.ts))
                            .call()
                        ).await?)
                    }
                }

                div ."m-postContext__postView" {
                    (post_view)
                }
            }
        })
    }

    /// Render post without its parents and comments, but with the buttons
    /// etc.)
    #[allow(clippy::too_many_arguments)]
    #[builder]
    pub async fn render_post_view(
        &self,
        #[builder(start_fn)] client: &ClientRef<'_>,
        #[builder(start_fn)] author: RostraId,
        persona_tags: Option<&BTreeSet<PersonaTag>>,
        event_id: Option<ShortEventId>,
        /// Post thread ID for HTML element IDs (to disambiguate same post in
        /// multiple places). If not provided, defaults to event_id.
        post_thread_id: Option<ShortEventId>,
        content: Option<&str>,
        url: Option<&Url>,
        title: Option<&str>,
        reply_count: Option<u64>,
        timestamp: Option<Timestamp>,
        extra_buttons: Option<Markup>,
        post_target_id: Option<String>,
        link_to_post: Option<bool>,
        ro: RoMode,
    ) -> RequestResult<Markup> {
        let external_event_id = event_id.map(|e| ExternalEventId::new(author, e));
        // Use post_thread_id if provided, otherwise default to event_id
        let post_thread_id = post_thread_id.or(event_id);
        let user_profile = self.get_social_profile_opt(author, client).await;

        let fetched_post = if url.is_none() || title.is_none() {
            if let Some(event_id) = event_id {
                client.db().get_social_post(event_id).await
            } else {
                None
            }
        } else {
            None
        };
        let external_url = url.cloned().or_else(|| {
            fetched_post
                .as_ref()
                .and_then(|post| post.content.url.clone())
        });
        let post_title = title.map(str::to_string).or_else(|| {
            fetched_post
                .as_ref()
                .and_then(|post| post.content.title.clone())
        });

        let post_content_rendered = if let Some(content) = content.as_ref() {
            Some(self.render_content(client, author, content).await)
        } else {
            None
        };

        let display_name = if let Some(ref profile) = user_profile {
            profile.display_name.clone()
        } else {
            author.to_short().to_string()
        };
        let post_content_is_missing = post_content_rendered.is_none();

        let post_target_id = post_target_id.or_else(|| {
            post_thread_id
                .zip(event_id)
                .map(|(ctx, id)| post_html_id(ctx, id))
        });

        let clickable_post_url = if link_to_post.unwrap_or(true) {
            event_id.map(|event_id| post_url(author, event_id))
        } else {
            None
        };
        let post_main = html! {
            div ."m-postView__main"
                data-href=[clickable_post_url.as_deref()]
                "@click"=[clickable_post_url.as_ref().map(|_| "if ($el.dataset.href && !event.target.closest('a, button, details, form, textarea, input, select') && !event.target.closest('.m-postContext__postParent:not(.-expanded)')) window.location = $el.dataset.href")]
            {
                div ."m-postView__topRow" {
                    (fragment::avatar("m-postView__userImage", self.avatar_url(author, user_profile.as_ref().map(|p| p.event_id).unwrap_or(ShortEventId::ZERO)), &format!("{display_name}'s avatar")))

                    div ."m-postView__contentSide" {

                        header ."m-postView__header" {
                            span ."m-postView__userHandle" {
                                (self.render_user_handle(event_id, author, user_profile.as_ref()))
                                @if let Some(ts) = timestamp {
                                    time ."m-postView__timestamp" datetime=(format_timestamp_iso(ts)) {
                                        (format_timestamp(ts))
                                    }
                                }
                            }
                            @if let Some(tags) = persona_tags {
                                @if !tags.is_empty() {
                                    div ."m-postView__personaTags" {
                                        @for tag in tags.iter() {
                                            span ."m-postView__personaTag" { (tag.as_str()) }
                                        }
                                    }
                                }
                            }
                        }
                        @if let Some(url) = external_url.as_ref() {
                            div ."m-postView__linkHeader" {
                                a href=(url.as_str()) target="_blank" rel="noopener noreferrer" {
                                    @if let Some(title) = post_title.as_ref() {
                                        (title)
                                    } @else {
                                        (url.as_str())
                                    }
                                }
                            }
                        } @else if let Some(title) = post_title.as_ref() {
                            div ."m-postView__linkHeader" {
                                (title)
                            }
                        }
                    }
                    @if let Some(event_id) = event_id {
                        details ."m-postView__actionMenu" {
                            summary ."m-postView__actionMenuTrigger" { "\u{22EE}" }
                            div ."m-postView__actionMenuDropdown" {
                                a ."m-postView__actionMenuItem" href=(post_url(author, event_id)) {
                                    "Share..."
                                }
                                @if author == client.rostra_id() {
                                    @if let Some(ctx) = post_thread_id {
                                        @let post_target = post_target_id.as_deref().unwrap_or("");
                                        @if ro.is_ro() {
                                            (fragment::button("m-postView__actionMenuItem", "Edit... (ro-mode)")
                                                .disabled(true)
                                                .call())
                                        } @else {
                                            (fragment::ajax_button(
                                                &post_edit_url(author, event_id),
                                                "get",
                                                post_target,
                                                "m-postView__actionMenuItem",
                                                "Edit...",
                                            )
                                            .hidden_inputs(html! {
                                                input type="hidden" name="post_thread_id" value=(ctx) {}
                                                input type="hidden" name="post_target_id" value=(post_target) {}
                                            })
                                            .call())
                                        }

                                        (fragment::ajax_button(
                                            &post_delete_url(author, event_id),
                                            "post",
                                            post_target,
                                            "m-postView__deleteMenuItem",
                                            "Delete",
                                        )
                                        .disabled(ro.to_disabled())
                                        .variant("--danger")
                                        .before_js("if (!confirm('Are you sure you want to delete this post?')) { $event.preventDefault(); return; }")
                                        .call())
                                    }
                                }
                            }
                        }
                    }
                }

                div."m-postView__content"
                    ."-missing"[post_content_rendered.is_none()]
                    ."-present"[post_content_rendered.is_some()]
                    id=[post_thread_id.zip(event_id).map(|(ctx, id)| post_content_html_id(ctx, id))]
                {
                    @if let Some(post_content_rendered) = post_content_rendered {
                        (post_content_rendered)
                    } @else {
                        p { "Post missing" }
                    }
                }
            }

        };

        let button_bar = html! {
            @if let Some(ext_event_id) = external_event_id {
                div ."m-postView__buttonBar" {
                    @if let Some(ctx) = post_thread_id {
                        (self.render_post_reactions(client, ext_event_id, ctx, ro).await)
                    }
                    div ."m-postView__buttons" {
                        @if let Some(extra_buttons) = extra_buttons {
                            (extra_buttons)
                        }
                        @if let Some(reply_count) = reply_count {
                            @if reply_count > 0 {
                                @if let Some(ctx) = post_thread_id {
                                    @let label = if reply_count == 1 { "1 Reply".to_string() } else { format!("{reply_count} Replies") };
                                    @let replies_target = post_replies_html_id(ctx, ext_event_id.event_id().to_short());
                                    (fragment::ajax_form(
                                        &crate::routes::url::replies_url(
                                            ctx,
                                            ext_event_id.event_id().to_short(),
                                        ),
                                        "get",
                                        &replies_target,
                                        fragment::button("m-postView__repliesButton", &label).call(),
                                    )
                                    .after_js("$el.querySelector('button').classList.add('u-hidden')")
                                    .call())
                                }
                            }
                        }
                        @if post_content_is_missing {
                            @if let (Some(ctx), Some(event_id)) = (post_thread_id, event_id) {
                                @let content_target = post_content_html_id(ctx, event_id);
                                (fragment::ajax_button(
                                    &post_fetch_url(ctx, author, event_id),
                                    "post",
                                    &content_target,
                                    "m-postView__fetchButton",
                                    "Fetch",
                                ).call())
                            }
                        }
                        (fragment::ajax_form(
                            &post_heart_reaction_url(
                                ext_event_id.rostra_id(),
                                ext_event_id.event_id().to_short(),
                            ),
                            "get",
                            "post-preview-dialog",
                            html! {
                                button
                                    ."m-postView__heartReactionButton"
                                    ."-disabled"[ro.is_ro()]
                                    type="submit"
                                    disabled[ro.to_disabled()]
                                    aria-label="Like this post"
                                    title="Like this post"
                                {
                                    "❤️"
                                }
                            },
                        )
                        .button_selector("$el.querySelector('.m-postView__heartReactionButton')")
                        .hidden_inputs(html! {
                            input type="hidden" name="post_thread_id" value=(post_thread_id.unwrap_or_else(|| ext_event_id.event_id().to_short())) {}
                        })
                        .form_class("m-postView__heartReactionForm")
                        .call())
                        // Reply button only available when we have a thread context
                        @if let Some(ctx) = post_thread_id {
                            // Target the replies container (placeholders are rendered inside when expanded)
                            @let reply_to_id = ext_event_id.event_id().to_short();
                            @let replies_target = post_replies_html_id(ctx, reply_to_id);
                            (fragment::ajax_button(
                                "/post/inline_reply",
                                "get",
                                &replies_target,
                                "m-postView__replyToButton",
                                "Reply",
                            )
                            .disabled(ro.to_disabled())
                            .hidden_inputs(html! {
                                input type="hidden" name="reply_to" value=(ext_event_id) {}
                                input type="hidden" name="post_thread_id" value=(ctx) {}
                            })
                            .call())
                        }
                    }
                }
            }
        };

        Ok(html! {
            div
                ."m-postView"
                id=[post_target_id.as_deref()]
             {
                div ."m-postView__body" {
                    (post_main)

                    (button_bar)
                }

                // Initially empty replies container - placeholders rendered inside when Reply/Replies clicked
                div ."m-postView__replies"
                    id=[post_thread_id.zip(event_id).map(|(ctx, id)| post_replies_html_id(ctx, id))]
                {}
            }
        })
    }
}
