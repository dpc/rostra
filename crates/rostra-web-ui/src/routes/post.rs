use std::collections::{HashMap, HashSet};

use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use maud::{Markup, html};
use rostra_client::ClientRef;
use rostra_client::connection_cache::ConnectionCache;
use rostra_client::util::rpc::get_event_content_from_followers;
use rostra_client_db::IdSocialProfileRecord;
use rostra_client_db::social::SocialPostRecord;
use rostra_core::event::SocialPost;
use rostra_core::id::{RostraId, ToShort as _};
use rostra_core::{ExternalEventId, ShortEventId, Timestamp};
use serde::Deserialize;
use snafu::ResultExt as _;
use tower_cookies::Cookies;

use super::timeline::TimelineMode;
use super::unlock::session::{RoMode, UserSession};
use super::{Maud, fragment};
use crate::error::{OtherSnafu, ReadOnlyModeSnafu, RequestResult};
use crate::util::extractors::AjaxRequest;
use crate::util::time::format_timestamp;
use crate::{SharedState, UiState};

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

pub async fn get_single_post(
    state: State<SharedState>,
    session: UserSession,
    mut cookies: Cookies,
    AjaxRequest(is_ajax): AjaxRequest,
    Query(query): Query<SinglePostQuery>,
    Path((author, event_id)): Path<(RostraId, ShortEventId)>,
) -> RequestResult<impl IntoResponse> {
    // Render raw post if it's an AJAX request or raw=true query parameter
    if is_ajax || query.raw {
        let client_handle = state.client(session.id()).await?;
        let client_ref = client_handle.client_ref()?;
        let db = client_ref.db();

        // Get the post record
        let post_record = db.get_social_post(event_id).await;

        if let Some(post_record) = post_record {
            return Ok(Maud(
                state
                    .render_post_context(&client_ref, author)
                    .event_id(event_id)
                    .post_thread_id(event_id)
                    .maybe_content(post_record.content.djot_content.as_deref())
                    .timestamp(post_record.ts)
                    .ro(state.ro_mode(session.session_token()))
                    .call()
                    .await?,
            ));
        } else {
            // Post not found, return error message
            return Ok(Maud(html! {
                div ."error" {
                    "Post not found"
                }
            }));
        }
    }

    // Default behavior: render full timeline page
    let navbar = state.timeline_common_navbar(&session).await?;
    Ok(Maud(
        state
            .render_timeline_page(
                navbar,
                None,
                &session,
                &mut cookies,
                TimelineMode::ProfileSingle(author, event_id),
                is_ajax,
            )
            .await?,
    ))
}

pub async fn delete_post(
    state: State<SharedState>,
    session: UserSession,
    Path((author_id, event_id)): Path<(RostraId, ShortEventId)>,
) -> RequestResult<impl IntoResponse> {
    let client_handle = state.client(session.id()).await?;
    let client = client_handle.client_ref()?;

    // Verify the post is authored by the current user
    if author_id != client.rostra_id() {
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
            rostra_core::event::SocialPost {
                djot_content: None,
                persona: rostra_core::event::PersonaId(0),
                reply_to: None,
                reaction: None,
            },
        )
        .replace(event_id)
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

pub async fn fetch_missing_post(
    state: State<SharedState>,
    session: UserSession,
    Path((post_thread_id, author_id, event_id)): Path<(ShortEventId, RostraId, ShortEventId)>,
) -> RequestResult<impl IntoResponse> {
    let client_handle = state.client(session.id()).await?;
    let client = client_handle.client_ref()?;

    let connections_cache = ConnectionCache::new();
    let mut followers_cache = std::collections::BTreeMap::new();

    get_event_content_from_followers(
        client.handle(),
        client.rostra_id(),
        author_id,
        event_id,
        &connections_cache,
        &mut followers_cache,
        client.db(),
    )
    .await
    .context(OtherSnafu)?;

    // Post was fetched successfully, render the updated content
    let db = client.db();
    let post_record = db.get_social_post(event_id).await;

    let content_id = post_content_html_id(post_thread_id, event_id);

    if let Some(post_record) = post_record {
        if let Some(djot_content) = post_record.content.djot_content.as_ref() {
            let post_content_rendered = state
                .render_content(&client, post_record.author, djot_content)
                .await;
            return Ok(Maud(html! {
                div #(content_id) ."m-postView__content -present" {
                    (post_content_rendered)
                }
            }));
        }
    }

    // Fetch attempt completed but post still not available
    Ok(Maud(html! {
        div #(content_id) ."m-postView__content -missing" {
            p {
                "Post not found"
            }
        }
    }))
}

#[bon::bon]
impl UiState {
    /// Render a whole post with all its context (parent, children buttons,
    /// etc.)
    #[allow(clippy::too_many_arguments)]
    #[builder]
    pub async fn render_post_context(
        &self,
        #[builder(start_fn)] client: &ClientRef<'_>,
        #[builder(start_fn)] author: RostraId,
        persona_display_name: Option<&str>,
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
        reply_count: Option<u64>,
        timestamp: Option<Timestamp>,
        ro: RoMode,
    ) -> RequestResult<Markup> {
        // Note: we are actually not doing pagination, and just ignore
        // everything after first page
        let (reactions, _) = if let Some(event_id) = event_id {
            client
                .db()
                .paginate_social_post_reactions_rev(event_id, None, 1000)
                .await
        } else {
            (vec![], None)
        };

        let mut reaction_social_profiles: HashMap<RostraId, IdSocialProfileRecord> = HashMap::new();

        for reaction_author in reactions
            .iter()
            .map(|reaction| reaction.author)
            // collect to deduplicate
            .collect::<HashSet<_>>()
        {
            // TODO: make a batched request for all profiles in one go
            if let Some(reaction_user_profile) =
                self.get_social_profile_opt(reaction_author, client).await
            {
                // HashSet above must have deduped it
                assert!(
                    reaction_social_profiles
                        .insert(reaction_author, reaction_user_profile)
                        .is_none()
                );
            }
        }

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
            .maybe_persona_display_name(persona_display_name)
            .maybe_event_id(event_id)
            .maybe_post_thread_id(post_thread_id)
            .maybe_content(content)
            .maybe_reply_count(reply_count)
            .maybe_timestamp(timestamp)
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
                        (Box::pin(self.render_post_view(
                            client,
                            reply_to_author,
                            )
                            .event_id(reply_to_event_id)
                            .maybe_post_thread_id(post_thread_id)
                            .ro(ro)
                            .maybe_content(reply_to_post.and_then(|r| r.content.djot_content.as_deref()))
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
        persona_display_name: Option<&str>,
        event_id: Option<ShortEventId>,
        /// Post thread ID for HTML element IDs (to disambiguate same post in
        /// multiple places). If not provided, defaults to event_id.
        post_thread_id: Option<ShortEventId>,
        content: Option<&str>,
        reply_count: Option<u64>,
        timestamp: Option<Timestamp>,
        ro: RoMode,
    ) -> RequestResult<Markup> {
        let external_event_id = event_id.map(|e| ExternalEventId::new(author, e));
        // Use post_thread_id if provided, otherwise default to event_id
        let post_thread_id = post_thread_id.or(event_id);
        let user_profile = self.get_social_profile_opt(author, client).await;

        // Note: we are actually not doing pagination, and just ignore
        // everything after first page
        let (reactions, _) = if let Some(event_id) = event_id {
            client
                .db()
                .paginate_social_post_reactions_rev(event_id, None, 1000)
                .await
        } else {
            (vec![], None)
        };

        let mut reaction_social_profiles: HashMap<RostraId, IdSocialProfileRecord> = HashMap::new();

        for reaction_author in reactions
            .iter()
            .map(|reaction| reaction.author)
            // collect to deduplicate
            .collect::<HashSet<_>>()
        {
            // TODO: make a batched request for all profiles in one go
            if let Some(reaction_user_profile) =
                self.get_social_profile_opt(reaction_author, client).await
            {
                // HashSet above must have deduped it
                assert!(
                    reaction_social_profiles
                        .insert(reaction_author, reaction_user_profile)
                        .is_none()
                );
            }
        }

        let reactions_html = html! {
            @for reaction in reactions {
                @if let Some(reaction_text) = reaction.content.get_reaction() {

                    span .m-postView__reaction
                        title=(
                            format!("by {}",
                                reaction_social_profiles.get(&reaction.author)
                                    .map(|r| r.display_name.clone())
                                    .unwrap_or_else(|| reaction.author.to_string())
                            )
                        )
                    {
                        (reaction_text)
                    }
                }
            }
        };

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

        let post_main = html! {
            div ."m-postView__main"
            {
                img ."m-postView__userImage u-userImage"
                    src=(self.avatar_url(author))
                    alt=(format!("{display_name}'s avatar"))
                    width="32pt"
                    height="32pt"
                    loading="lazy"
                { }

                div ."m-postView__contentSide" {

                    header ."m-postView__header" {
                        span ."m-postView__userHandle" {
                            (self.render_user_handle(event_id, author, user_profile.as_ref()))
                            @if let Some(persona_display_name) = persona_display_name {
                                span ."m-postView__personaDisplayName" {
                                    (format!("({})", persona_display_name))
                                }
                            }
                            @if let Some(ts) = timestamp {
                                span ."m-postView__timestamp" {
                                    (format_timestamp(ts))
                                }
                            }
                        }
                        @if let Some(event_id) = event_id {
                            details ."m-postView__actionMenu" {
                                summary ."m-postView__actionMenuTrigger" { "\u{22EE}" }
                                div ."m-postView__actionMenuDropdown" {
                                    a ."m-postView__actionMenuItem" href=(format!("/post/{}/{}", author, event_id)) {
                                        "Share..."
                                    }
                                    @if author == client.rostra_id() {
                                        @if let Some(ctx) = post_thread_id {
                                            @let post_target = post_html_id(ctx, event_id);
                                            (fragment::ajax_button(
                                                &format!("/post/{author}/{event_id}/delete"),
                                                "post",
                                                &post_target,
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
            }

        };

        let button_bar = html! {
            @if let Some(ext_event_id) = external_event_id {
                div ."m-postView__buttonBar" {
                    div .m-postView__reactions {
                        (reactions_html)
                    }
                    div ."m-postView__buttons" {
                        @if let Some(reply_count) = reply_count {
                            @if reply_count > 0 {
                                @if let Some(ctx) = post_thread_id {
                                    @let label = if reply_count == 1 { "1 Reply".to_string() } else { format!("{reply_count} Replies") };
                                    @let replies_target = post_replies_html_id(ctx, ext_event_id.event_id().to_short());
                                    (fragment::ajax_form(
                                        &format!("/replies/{}/{}", ctx, ext_event_id.event_id().to_short()),
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
                                    &format!("/post/{ctx}/{author}/{event_id}/fetch"),
                                    "post",
                                    &content_target,
                                    "m-postView__fetchButton",
                                    "Fetch",
                                ).call())
                            }
                        }
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
                id=[post_thread_id.zip(event_id).map(|(ctx, id)| post_html_id(ctx, id))]
             {
                (post_main)

                (button_bar)

                // Initially empty replies container - placeholders rendered inside when Reply/Replies clicked
                div ."m-postView__replies"
                    id=[post_thread_id.zip(event_id).map(|(ctx, id)| post_replies_html_id(ctx, id))]
                {}
            }
        })
    }
}
