use std::collections::{HashMap, HashSet};

use axum::extract::{Path, State};
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
use snafu::ResultExt as _;
use tower_cookies::Cookies;

use super::Maud;
use super::timeline::TimelineMode;
use super::unlock::session::{RoMode, UserSession};
use crate::error::{OtherSnafu, RequestResult};
use crate::util::time::format_timestamp;
use crate::{SharedState, UiState};

pub async fn get_single_post(
    state: State<SharedState>,
    session: UserSession,
    mut cookies: Cookies,
    Path((author, event_id)): Path<(RostraId, ShortEventId)>,
) -> RequestResult<impl IntoResponse> {
    let navbar = state.timeline_common_navbar(&session).await?;
    Ok(Maud(
        state
            .render_timeline_page(
                navbar,
                None,
                &session,
                &mut cookies,
                TimelineMode::ProfileSingle(author, event_id),
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

    // Create and publish a delete event with DELETE_PARENT_AUX_CONTENT_FLAG set
    // and parent_aux pointing to the post we want to delete
    client
        .publish_event(
            session.id_secret()?,
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

    // Return empty content to replace the post
    Ok(Maud(html! {
        div ."m-postOverview -deleted" {
            div ."m-postOverview__deletedMessage" {
                "This post has been deleted"
            }
        }
    }))
}

pub async fn fetch_missing_post(
    state: State<SharedState>,
    session: UserSession,
    Path((author_id, event_id)): Path<(RostraId, ShortEventId)>,
) -> RequestResult<impl IntoResponse> {
    let client_handle = state.client(session.id()).await?;
    let client = client_handle.client_ref()?;

    let mut connections_cache = ConnectionCache::new();
    let mut followers_cache = std::collections::BTreeMap::new();

    get_event_content_from_followers(
        client.handle(),
        client.rostra_id(),
        author_id,
        event_id,
        &mut connections_cache,
        &mut followers_cache,
        client.db(),
    )
    .await
    .context(OtherSnafu)?;

    // Post was fetched successfully, render the updated content
    let db = client.db();
    let post_record = db.get_social_post(event_id).await;

    if let Some(post_record) = post_record {
        if let Some(djot_content) = post_record.content.djot_content.as_ref() {
            let post_content_rendered = state
                .render_content(&client, post_record.author, djot_content)
                .await;
            return Ok(Maud(html! {
                div ."m-postOverview__content -present" {
                    p {
                        (post_content_rendered)
                    }
                }
            }));
        }
    }

    // Fetch attempt completed but post still not available
    Ok(Maud(html! {
        div ."m-postOverview__content -missing" {
            p {
                "Post not found"
            }
        }
    }))
}

#[bon::bon]
impl UiState {
    #[allow(clippy::too_many_arguments)]
    #[builder]
    pub async fn render_post_overview(
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
        content: Option<&str>,
        reply_count: Option<u64>,
        timestamp: Option<Timestamp>,
        ro: RoMode,
        // Render the post including a comment, right away
        comment: Option<Markup>,
        // Is the post loaded as a comment to an existing post (already being
        // displayed)
        #[builder(default = false)] is_comment: bool,
    ) -> RequestResult<Markup> {
        let external_event_id = event_id.map(|e| ExternalEventId::new(author, e));
        let user_profile = self.get_social_profile_opt(author, client).await;

        // Note: we are actually not doing pagiantion, and just ignore
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

                    span .m-postOverview__reaction
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
            div ."m-postOverview__main"
            {
                img ."m-postOverview__userImage u-userImage"
                    src=(self.avatar_url(author))
                    alt=(format!("{display_name}'s avatar"))
                    width="32pt"
                    height="32pt"
                    loading="lazy"
                { }

                div ."m-postOverview__contentSide"
                    onclick=[comment.as_ref().map(|_|"this.classList.add('-expanded')" )]
                {
                    header ."m-postOverview__header" {
                        span ."m-postOverview__userHandle" {
                            (self.render_user_handle(event_id, author, user_profile.as_ref()))
                            @if let Some(persona_display_name) = persona_display_name {
                                span ."m-postOverview__personaDisplayName" {
                                    (format!("({})", persona_display_name))
                                }
                            }
                            @if let Some(ts) = timestamp {
                                span ."m-postOverview__timestamp" {
                                    (format_timestamp(ts))
                                }
                            }
                        }
                        @if let Some(event_id) = event_id {
                            a ."m-postOverview__postAnchor" href=(format!("/ui/post/{}/{}", author, event_id)) { "#" }
                        }
                    }

                    div ."m-postOverview__content"
                     ."-missing"[post_content_rendered.is_none()]
                     ."-present"[post_content_rendered.is_some()]
                    {
                        p {
                            @if let Some(post_content_rendered) = post_content_rendered {
                                (post_content_rendered)
                            } @else {
                                p {
                                    "Post missing"
                                }
                            }
                        }
                    }
                }

            }
        };

        let button_bar = html! {
            @if let Some(ext_event_id) = external_event_id {
                div ."m-postOverview__buttonBar" {
                    div .m-postOverview__reactions {
                        (reactions_html)
                    }
                    div ."m-postOverview__buttons" {
                        @if let Some(reply_count) = reply_count {
                            @if reply_count > 0 {
                                button ."m-postOverview__commentsButton u-button"
                                    hx-get={"/ui/comments/"(ext_event_id.event_id().to_short())}
                                    hx-target="next .m-postOverview__comments"
                                    hx-swap="outerHTML"
                                    hx-on::after-request="this.classList.add('u-hidden')"
                                {
                                    span ."m-postOverview__commentsButtonIcon u-buttonIcon" width="1rem" height="1rem" {}
                                    @if reply_count == 1 {
                                        ("1 Reply".to_string())
                                    } @else {
                                        (format!("{} Replies", reply_count))
                                    }
                                }
                            }

                        }
                        @if post_content_is_missing {
                            @if let Some(event_id) = event_id {
                                button ."u-button u-button--small"
                                    hx-post={"/ui/post/"(author)"/"(event_id)"/fetch"}
                                    hx-target="previous .m-postOverview__content"
                                    hx-swap="outerHTML"
                                    hx-indicator="next .htmx-indicator"
                                {
                                    span ."m-postOverview__fetchButtonIcon u-buttonIcon" width="1rem" height="1rem" {}
                                    "Fetch"
                                }
                            }
                        }
                        @if !ro.is_ro() {
                            @if author == client.rostra_id() {
                                button ."m-postOverview__deleteButton u-button u-button--danger"
                                    hx-post={"/ui/post/"(author)"/"(event_id.unwrap())"/delete"}
                                    hx-confirm="Are you sure you want to delete this post?"
                                    hx-target="closest .m-postOverview"
                                    hx-swap="outerHTML"
                                {
                                    span ."m-postOverview__deleteButtonIcon u-buttonIcon" width="1rem" height="1rem" {}
                                    "Delete"
                                }
                            }

                            button ."m-postOverview__replyToButton u-button"
                                disabled[ro.to_disabled()]
                                hx-get={"/ui/post/reply_to?reply_to="(ext_event_id)}
                                hx-target=".m-newPostForm__replyToLine"
                                hx-swap="outerHTML"
                            {
                                span ."m-postOverview__replyToButtonIcon u-buttonIcon" width="1rem" height="1rem" {}
                                "Reply"
                            }
                        }
                    }
                }
            }
        };

        let post_id = format!(
            "post-{}",
            event_id
                .map(|e| e.to_string())
                .unwrap_or_else(|| "preview".to_string()),
        );

        let post = html! {
            article #(post_id)
                ."m-postOverview"
                ."-response"[reply_to.is_some() || is_comment]
                ."-reply-parent"[comment.is_some()]
             {
                (post_main)

                (button_bar)

                div ."m-postOverview__comments"
                    ."-empty"[comment.is_none()]
                {
                    @if let Some(comment) = comment {
                        div ."m-postOverview__commentsItem" {
                            (comment)
                        }
                    }
                }
            }
        };

        Ok(html! {
            @if let Some((reply_to_author, reply_to_event_id, reply_to_post)) = reply_to {
                (Box::pin(self.render_post_overview(
                    client,
                    reply_to_author,
                    )
                    .event_id(reply_to_event_id)
                    .ro(ro)
                    .maybe_content(reply_to_post.and_then(|r| r.content.djot_content.as_deref()))
                    .maybe_timestamp(reply_to_post.map(|r| r.ts))
                    .comment(post)
                    .call()
                ).await?)
            } @else {
                (post)
            }
        })
    }
}
