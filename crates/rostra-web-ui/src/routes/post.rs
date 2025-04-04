use std::collections::{HashMap, HashSet};

use axum::extract::{Path, State};
use axum::response::IntoResponse;
use maud::{Markup, html};
use rostra_client::ClientRef;
use rostra_client_db::IdSocialProfileRecord;
use rostra_client_db::social::SocialPostRecord;
use rostra_core::event::SocialPost;
use rostra_core::id::{RostraId, ToShort as _};
use rostra_core::{ExternalEventId, ShortEventId};
use tower_cookies::Cookies;

use super::Maud;
use super::timeline::TimelineMode;
use super::unlock::session::{RoMode, UserSession};
use crate::error::RequestResult;
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

#[bon::bon]
impl UiState {
    #[allow(clippy::too_many_arguments)]
    #[builder]
    pub async fn render_post_overview(
        &self,
        #[builder(start_fn)] client: &ClientRef<'_>,
        #[builder(start_fn)] author: RostraId,
        persona_display_name: Option<&str>,
        reply_to: Option<(RostraId, Option<&SocialPostRecord<SocialPost>>)>,
        event_id: Option<ShortEventId>,
        content: Option<&str>,
        reply_count: Option<u64>,
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
            Some(self.render_content(client, content).await)
        } else {
            None
        };

        let display_name = if let Some(ref profile) = user_profile {
            profile.display_name.clone()
        } else {
            author.to_short().to_string()
        };
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
                                "Post missing"
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
            @if let Some((reply_to_author, reply_to_post)) = reply_to {
                @if let Some(reply_to_post) = reply_to_post {
                    @if let Some(djot_content) = reply_to_post.content.djot_content.as_ref() {
                        (Box::pin(self.render_post_overview(
                            client,
                            reply_to_post.author
                            )
                            .event_id(reply_to_post.event_id)
                            .content(djot_content)
                            .ro(ro)
                            .comment(post)
                            .call()
                        )
                        .await?)
                    }
                } @else {
                    (Box::pin(self.render_post_overview(
                        client,
                        reply_to_author,
                        )
                        .ro(ro).comment(post)
                        .call()
                    ).await?)
                }
            } @else {
                (post)
            }
        })
    }
}
