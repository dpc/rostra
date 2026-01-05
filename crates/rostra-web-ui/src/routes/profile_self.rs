mod extractor;

use axum::extract::State;
use axum::response::IntoResponse;
use maud::{Markup, PreEscaped, html};
use rostra_client::ClientRef;
use rostra_client_db::IdSocialProfileRecord;
use rostra_core::ShortEventId;
use rostra_core::id::{RostraId, ToShort as _};

use super::Maud;
use super::unlock::session::{RoMode, UserSession};
use crate::error::RequestResult;
use crate::{SharedState, UiState};

pub async fn get_self_account_edit(
    state: State<SharedState>,
    session: UserSession,
) -> RequestResult<impl IntoResponse> {
    Ok(Maud(state.render_profile_edit_form(&session).await?))
}

pub async fn post_self_account_edit(
    state: State<SharedState>,
    session: UserSession,
    form: extractor::InputForm,
) -> RequestResult<impl IntoResponse> {
    let existing = state
        .client(session.id())
        .await?
        .client_ref()?
        .db()
        .get_social_profile(session.id())
        .await;

    state
        .client(session.id())
        .await?
        .client_ref()?
        .post_social_profile_update(
            session.id_secret()?,
            form.name,
            form.bio,
            form.avatar.or_else(|| existing.and_then(|e| e.avatar)),
        )
        .await?;

    Ok(Maud(
        state
            .render_self_profile_summary(&session, session.ro_mode())
            .await?,
    ))
}

impl UiState {
    pub async fn get_social_profile(
        &self,
        id: RostraId,
        client: &ClientRef<'_>,
    ) -> IdSocialProfileRecord {
        client.db().get_social_profile(id).await.unwrap_or_else(|| {
            rostra_client_db::IdSocialProfileRecord {
                event_id: ShortEventId::ZERO,
                display_name: id.to_short().to_string(),
                bio: "".into(),
                avatar: None,
            }
        })
    }

    pub async fn get_social_profile_opt(
        &self,
        id: RostraId,
        client: &ClientRef<'_>,
    ) -> Option<IdSocialProfileRecord> {
        client.db().get_social_profile(id).await
    }

    pub fn avatar_url(&self, id: RostraId) -> String {
        format!("/ui/avatar/{id}")
    }

    pub async fn render_self_profile_summary(
        &self,
        user: &UserSession,
        ro: RoMode,
    ) -> RequestResult<Markup> {
        let client = self.client(user.id()).await?;
        let self_id = client.client_ref()?.rostra_id();
        let self_profile = self
            .get_social_profile(self_id, &client.client_ref()?)
            .await;
        Ok(html! {
            div id="self-profile-summary" ."m-profileSummary" {
                script {
                    (PreEscaped(
                    r#"
                    function copyIdToClipboard(event) {
                        const target = event.target;
                        const id = target.getAttribute('data-value');
                        navigator.clipboard.writeText(id);
                        target.classList.add('-active');

                        setTimeout(() => {
                            target.classList.remove('-active');
                        }, 1000);
                    }
                    "#
                    ))
                }
                img ."m-profileSummary__userImage u-userImage"
                    src=(self.avatar_url(self_id))
                    alt="Self avatar"
                    width="32pt"
                    height="32pt"
                    loading="lazy"
                    { }

                div ."m-profileSummary__content" {
                    a ."m-profileSummary__displayName u-displayName"
                        href=(format!("/ui/profile/{}", self_id))
                    {
                        (self_profile.display_name)
                    }
                    div ."m-profileSummary__buttons" {
                        button
                            ."m-profileSummary__copyButton u-button"
                            data-value=(self_id) onclick="copyIdToClipboard(event)"  {
                                span ."m-profileSummary__copyButtonIcon u-buttonIcon" width="1rem" height="1rem" {}
                                "RostraId"
                            }
                        form
                            action="/ui/self/edit"
                            method="get"
                            x-target="self-profile-summary"
                        {
                            button
                                ."m-profileSummary__editButton u-button"
                                ."-disabled"[ro.to_disabled()]
                                type="submit"
                            {
                                span ."m-profileSummary__editButtonIcon u-buttonIcon" width="1rem" height="1rem" {}
                                "Edit"
                            }
                        }
                        form
                            action="/ui/unlock/logout"
                            method="post"
                            style="display: inline;"
                            {
                            button
                                ."m-profileSummary__logoutButton u-button"
                                type="submit"
                                {
                                    span ."m-profileSummary__logoutButtonIcon u-buttonIcon" width="1rem" height="1rem" {}
                                    "Logout"
                                }
                        }
                    }
                }
            }
        })
    }

    pub async fn render_profile_edit_form(&self, user: &UserSession) -> RequestResult<Markup> {
        let client = self.client(user.id()).await?;
        let client_ref = client.client_ref()?;
        let self_profile = self
            .get_social_profile(client_ref.rostra_id(), &client_ref)
            .await;
        Ok(html! {
            form id="self-profile-summary" ."m-profileSummary -edit"
                action="/ui/self/edit"
                method="post"
                x-target="self-profile-summary"
                enctype="multipart/form-data"
            {
                script {
                    (PreEscaped(r#"
                        function previewAvatar(event) {
                            document.querySelector('.m-profileSummary__userImage').src=URL.createObjectURL(event.target.files[0]);
                        }
                    "#))
                }
                label for="avatar-upload" ."m-profileSummary__userImageLabel" {
                    img ."m-profileSummary__userImage u-userImage"
                        src=(self.avatar_url(user.id()))
                        width="32pt"
                        height="32pt" {
                    }
                }
                input # "avatar-upload"
                    ."m-profileSummary__userImageInput"
                    type="file"
                    name="avatar"
                    accept="image/*"
                    style="display: none;"
                    onchange="previewAvatar(event)"
                {}

                div ."m-profileSummary__content" {
                    input ."m-profileSummary__displayName"
                        type="text"
                        name="name"
                        value=(self_profile.display_name) {
                    }

                    div ."m-profileSummary__buttons" {
                        button
                            ."m-profileSummary__saveButton u-button" {
                            span ."m-profileSummary__saveButtonIcon u-buttonIcon" width="1rem" height="1rem" {}
                            "Save"
                        }
                    }
                }

                textarea."m-profileSummary__bioEdit"
                    placeholder="Bio..."
                    rows="8"
                    dir="auto"
                    name="bio"
                    "x-on:keyup.enter.ctrl"="$el.form.requestSubmit()"
                {
                    {(self_profile.bio)}
                }
            }
        })
    }
}
