use std::str::FromStr as _;

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Redirect};
use maud::{Markup, html};
use rostra_client_db::{EventContentStateNew, EventRecord};
use rostra_core::Timestamp;
use rostra_core::id::RostraId;
use serde::Deserialize;

use super::unlock::session::UserSession;
use super::{Maud, fragment};
use crate::error::RequestResult;
use crate::util::time::format_timestamp;
use crate::{SharedState, UiState};

pub async fn get_settings() -> impl IntoResponse {
    Redirect::to("/ui/settings/following")
}

pub async fn get_settings_following(
    state: State<SharedState>,
    session: UserSession,
) -> RequestResult<impl IntoResponse> {
    let client = state.client(session.id()).await?;
    let client_ref = client.client_ref()?;
    let user_id = client_ref.rostra_id();

    let followees = client_ref.db().get_followees(user_id).await;

    let navbar = state.render_settings_navbar(&session, "following").await?;
    let content = state
        .render_following_settings(&session, user_id, followees)
        .await?;

    Ok(Maud(
        state
            .render_settings_page(&session, navbar, content)
            .await?,
    ))
}

pub async fn get_settings_followers(
    state: State<SharedState>,
    session: UserSession,
) -> RequestResult<impl IntoResponse> {
    let client = state.client(session.id()).await?;
    let client_ref = client.client_ref()?;
    let user_id = client_ref.rostra_id();

    let followers = client_ref.db().get_followers(user_id).await;

    let navbar = state.render_settings_navbar(&session, "followers").await?;
    let content = state.render_followers_settings(&session, followers).await?;

    Ok(Maud(
        state
            .render_settings_page(&session, navbar, content)
            .await?,
    ))
}

#[derive(Deserialize)]
pub struct EventExplorerQuery {
    id: Option<String>,
}

pub async fn get_settings_events(
    state: State<SharedState>,
    session: UserSession,
    Query(query): Query<EventExplorerQuery>,
) -> RequestResult<impl IntoResponse> {
    let client = state.client(session.id()).await?;
    let client_ref = client.client_ref()?;
    let user_id = client_ref.rostra_id();

    // Parse the selected identity or default to user's own id
    let selected_id = if let Some(id_str) = &query.id {
        RostraId::from_str(id_str).unwrap_or(user_id)
    } else {
        user_id
    };

    // Get known identities for the dropdown
    let mut known_ids = client_ref.db().get_known_identities().await;
    // Ensure user's own id is in the list
    if !known_ids.contains(&user_id) {
        known_ids.push(user_id);
    }
    // Sort for consistent display
    known_ids.sort_by_cached_key(|a| a.to_string());

    // Get events for the selected identity (limit to 100)
    let events = client_ref.db().get_events_for_id(selected_id, 100).await;

    let navbar = state.render_settings_navbar(&session, "events").await?;
    let content = state
        .render_event_explorer_settings(&session, user_id, selected_id, known_ids, events)
        .await?;

    Ok(Maud(
        state
            .render_settings_page(&session, navbar, content)
            .await?,
    ))
}

impl UiState {
    pub async fn render_settings_page(
        &self,
        _session: &UserSession,
        navbar: Markup,
        content: Markup,
    ) -> RequestResult<Markup> {
        self.render_html_page("Settings", self.render_page_layout(navbar, content))
            .await
    }

    pub async fn render_settings_navbar(
        &self,
        _session: &UserSession,
        active_category: &str,
    ) -> RequestResult<Markup> {
        Ok(html! {
            nav ."o-navBar" {
                div ."o-topNav" {
                    a ."o-topNav__item" href="/ui" {
                        span ."o-topNav__icon -back" {}
                        "Back"
                    }
                }

                div ."o-settingsNav" {
                    a ."o-settingsNav__item"
                        ."-active"[active_category == "following"]
                        href="/ui/settings/following"
                    {
                        "Followees"
                    }
                    a ."o-settingsNav__item"
                        ."-active"[active_category == "followers"]
                        href="/ui/settings/followers"
                    {
                        "Followers"
                    }
                    a ."o-settingsNav__item"
                        ."-active"[active_category == "events"]
                        href="/ui/settings/events"
                    {
                        "Event Explorer"
                    }
                }
            }
        })
    }

    pub async fn render_following_settings(
        &self,
        session: &UserSession,
        _user_id: RostraId,
        followees: Vec<(RostraId, rostra_core::event::PersonaSelector)>,
    ) -> RequestResult<Markup> {
        Ok(html! {
            div ."o-settingsContent" {
                h2 ."o-settingsContent__header" { "Followees" }

                div ."o-settingsContent__section" {
                    h3 ."o-settingsContent__sectionHeader" { "Add" }
                    (self.render_add_followee_form(None))
                }

                div ."o-settingsContent__section" {
                    h3 ."o-settingsContent__sectionHeader" { "People You Follow" }
                    (self.render_followee_list(session, followees).await?)
                }

                // Follow dialog container (shared by all followee items)
                div id="follow-dialog-content" {}
            }
        })
    }

    pub async fn render_followers_settings(
        &self,
        session: &UserSession,
        followers: Vec<RostraId>,
    ) -> RequestResult<Markup> {
        Ok(html! {
            div ."o-settingsContent" {
                h2 ."o-settingsContent__header" { "Followers" }

                div ."o-settingsContent__section" {
                    h3 ."o-settingsContent__sectionHeader" { "People Who Follow You" }
                    (self.render_follower_list(session, followers).await?)
                }
            }
        })
    }

    pub async fn render_followee_list(
        &self,
        session: &UserSession,
        followees: Vec<(RostraId, rostra_core::event::PersonaSelector)>,
    ) -> RequestResult<Markup> {
        let client = self.client(session.id()).await?;
        let client_ref = client.client_ref()?;

        let mut followee_items = Vec::new();
        for (followee_id, _persona_selector) in followees {
            let profile = self.get_social_profile_opt(followee_id, &client_ref).await;
            let display_name = profile
                .as_ref()
                .map(|p| p.display_name.clone())
                .unwrap_or_else(|| followee_id.to_string());
            followee_items.push((followee_id, display_name));
        }

        // Sort by display name
        followee_items.sort_by_cached_key(|a| a.1.to_lowercase());

        Ok(html! {
            div id="followee-list" ."m-followeeList" {
                @if followee_items.is_empty() {
                    p ."o-settingsContent__empty" {
                        "You are not following anyone yet."
                    }
                } @else {
                    @for (followee_id, display_name) in &followee_items {
                        div ."m-followeeList__item" {
                            img ."m-followeeList__avatar u-userImage"
                                src=(self.avatar_url(*followee_id))
                                alt="Avatar"
                                width="32"
                                height="32"
                                loading="lazy"
                                {}
                            a ."m-followeeList__name"
                                href=(format!("/ui/profile/{}", followee_id))
                            {
                                (display_name)
                            }
                            (fragment::ajax_button(
                                &format!("/ui/profile/{followee_id}/follow"),
                                "get",
                                "follow-dialog-content",
                                "m-followeeList__followButton",
                                "Following...",
                            )
                            .disabled(session.ro_mode().to_disabled())
                            .hidden_inputs(html! { input type="hidden" name="following" value="true" {} })
                            .form_class("m-followeeList__actions")
                            .call())
                        }
                    }
                }
            }
        })
    }

    pub async fn render_follower_list(
        &self,
        session: &UserSession,
        followers: Vec<RostraId>,
    ) -> RequestResult<Markup> {
        let client = self.client(session.id()).await?;
        let client_ref = client.client_ref()?;

        let mut follower_items = Vec::new();
        for follower_id in followers {
            let profile = self.get_social_profile_opt(follower_id, &client_ref).await;
            let display_name = profile
                .as_ref()
                .map(|p| p.display_name.clone())
                .unwrap_or_else(|| follower_id.to_string());
            follower_items.push((follower_id, display_name));
        }

        // Sort by display name
        follower_items.sort_by_cached_key(|a| a.1.to_lowercase());

        Ok(html! {
            div ."m-followeeList" {
                @if follower_items.is_empty() {
                    p ."o-settingsContent__empty" {
                        "No one is following you yet (that you know of)."
                    }
                } @else {
                    @for (follower_id, display_name) in &follower_items {
                        div ."m-followeeList__item" {
                            img ."m-followeeList__avatar u-userImage"
                                src=(self.avatar_url(*follower_id))
                                alt="Avatar"
                                width="32"
                                height="32"
                                loading="lazy"
                                {}
                            a ."m-followeeList__name"
                                href=(format!("/ui/profile/{}", follower_id))
                            {
                                (display_name)
                            }
                        }
                    }
                }
            }
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn render_event_explorer_settings(
        &self,
        session: &UserSession,
        user_id: RostraId,
        selected_id: RostraId,
        known_ids: Vec<RostraId>,
        events: Vec<(EventRecord, Timestamp, Option<EventContentStateNew>)>,
    ) -> RequestResult<Markup> {
        let client = self.client(session.id()).await?;
        let client_ref = client.client_ref()?;

        // Build display names for known ids
        let mut id_display_names = Vec::new();
        for id in &known_ids {
            let profile = self.get_social_profile_opt(*id, &client_ref).await;
            let display_name = profile
                .as_ref()
                .map(|p| p.display_name.clone())
                .unwrap_or_else(|| id.to_string());
            let is_self = *id == user_id;
            id_display_names.push((*id, display_name, is_self));
        }

        Ok(html! {
            div ."o-settingsContent" {
                h2 ."o-settingsContent__header" { "Event Explorer" }

                div ."o-settingsContent__section" {
                    h3 ."o-settingsContent__sectionHeader" { "Select Identity" }

                    form ."m-eventExplorer__form" method="get" action="/ui/settings/events" {
                        select ."m-eventExplorer__select" name="id" onchange="this.form.submit()" {
                            @for (id, display_name, is_self) in &id_display_names {
                                option value=(id.to_string()) selected[*id == selected_id] {
                                    @if *is_self {
                                        (format!("{} (you)", display_name))
                                    } @else {
                                        (display_name)
                                    }
                                }
                            }
                        }
                        noscript {
                            button type="submit" { "Load" }
                        }
                    }
                }

                div ."o-settingsContent__section" {
                    h3 ."o-settingsContent__sectionHeader" {
                        "Events ("(events.len())" most recent)"
                    }

                    @if events.is_empty() {
                        p ."o-settingsContent__empty" {
                            "No events found for this identity."
                        }
                    } @else {
                        div ."m-eventExplorer__list" {
                            @for (event_record, ts, content_state) in &events {
                                (self.render_event_row(event_record, *ts, content_state.as_ref()))
                            }
                        }
                    }
                }
            }
        })
    }

    fn render_event_row(
        &self,
        event_record: &EventRecord,
        ts: Timestamp,
        content_state: Option<&EventContentStateNew>,
    ) -> Markup {
        let event = &event_record.signed.event;
        let event_id = event_record.signed.compute_short_id();
        let event_id_str = event_id.to_string();

        // Format timestamp (ts.0 is already in seconds)
        let datetime = time::OffsetDateTime::from_unix_timestamp(ts.0 as i64)
            .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
        let time_str = datetime
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| ts.to_string());

        // Format flags
        let mut flags = Vec::new();
        if event.is_delete_parent_aux_content_set() {
            flags.push("DEL");
        }
        if event.is_singleton() {
            flags.push("SINGLETON");
        }

        // Format content state
        // In the new model, events_content_state only contains deleted/pruned states.
        // If None, content is either available in content_store or missing.
        let content_state_str = match content_state {
            Some(EventContentStateNew::Deleted { .. }) => "Deleted",
            Some(EventContentStateNew::Pruned) => "Pruned",
            None => "", // Content state not tracked (check content_store for availability)
        };

        // Format parents
        let parent_prev: Option<rostra_core::ShortEventId> = event.parent_prev.into();
        let parent_aux: Option<rostra_core::ShortEventId> = event.parent_aux.into();

        // Content hash
        let content_hash = event.content_hash.to_string();

        html! {
            div ."m-eventExplorer__row" id=(format!("ev-{event_id_str}")) {
                // Row 1: KIND, ID, Flags, Timestamp (spans full width)
                div ."m-eventExplorer__rowHeader" {
                    span ."m-eventExplorer__kind" { (event.kind) }
                    span ."m-eventExplorer__eventId" { (event_id_str) }
                    @if !flags.is_empty() {
                        span ."m-eventExplorer__flags" {
                            "Flags: "
                            @for (i, flag) in flags.iter().enumerate() {
                                @if 0 < i { ", " }
                                span ."m-eventExplorer__flag" { (flag) }
                            }
                        }
                    }
                    span ."m-eventExplorer__timestamp" title=(time_str) {
                        (format_timestamp(ts))
                    }
                }

                // Row 2: Content info (grid items)
                span ."m-eventExplorer__label" { "Content:" }
                span ."m-eventExplorer__contentHash" title=(content_hash) {
                    (&content_hash[..16])
                }
                span ."m-eventExplorer__contentSize" {
                    (rostra_util_fmt::format_bytes(u32::from(event.content_len) as u64))
                }
                span ."m-eventExplorer__contentState" data-state=(content_state_str.to_lowercase()) {
                    (content_state_str)
                }

                // Row 3: Parents (grid items)
                span ."m-eventExplorer__label" { "Parents:" }
                span ."m-eventExplorer__parentPrev" {
                    @if let Some(prev) = parent_prev {
                        a ."m-eventExplorer__parentLink"
                            href=(format!("#ev-{prev}"))
                            title=(prev.to_string())
                        {
                            (prev.to_string())
                        }
                    } @else {
                        span ."m-eventExplorer__parentNone" { "none" }
                    }
                }
                span ."m-eventExplorer__parentAux" {
                    @if let Some(aux) = parent_aux {
                        a ."m-eventExplorer__parentLink"
                            href=(format!("#ev-{aux}"))
                            title=(aux.to_string())
                        {
                            (aux.to_string())
                        }
                    } @else {
                        span ."m-eventExplorer__parentNone" { "none" }
                    }
                }
            }
        }
    }
}
