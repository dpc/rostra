use std::borrow::Cow;
use std::str::FromStr as _;

use jotdown::r#async::{AsyncRender, AsyncRenderOutputExt};
use jotdown::html::filters::AsyncSanitizeExt;
use maud::{Markup, PreEscaped};
use rostra_client::ClientRef;
use rostra_core::ShortEventId;
use rostra_core::id::RostraId;
use url::Url;

use crate::UiState;

mod filters;

use filters::{PrismCodeBlocks, RostraImages, RostraProfileLinks};

/// Extension trait for adding rostra-specific rendering transformations
pub trait RostraRenderExt {
    /// Transform rostra: profile links to UI profile links with @username
    /// format
    fn rostra_profile_links<'c>(self, client: ClientRef<'c>) -> RostraProfileLinks<'c, Self>
    where
        Self: Sized,
    {
        RostraProfileLinks::new(self, client)
    }

    /// Transform images (rostra-media: links and external media with lazy
    /// loading)
    fn rostra_images<'s>(self, author_id: RostraId) -> RostraImages<'s, Self>
    where
        Self: Sized + AsyncRender<'s>,
    {
        RostraImages::new(self, author_id)
    }

    /// Add Prism.js classes to code blocks for syntax highlighting
    fn prism_code_blocks(self) -> PrismCodeBlocks<Self>
    where
        Self: Sized,
    {
        PrismCodeBlocks::new(self)
    }
}

impl<'s, R> RostraRenderExt for R where R: Sized + AsyncRender<'s> {}

impl UiState {
    pub(crate) async fn render_content(
        &self,
        client: &ClientRef<'_>,
        author_id: RostraId,
        content: &str,
    ) -> Markup {
        // Compose the filters using extension traits: Renderer -> ProfileLinks ->
        // Images -> PrismCodeBlocks -> Sanitize
        let renderer = jotdown::html::tokio::Renderer::default()
            .rostra_profile_links(client.clone())
            .rostra_images(author_id)
            .prism_code_blocks()
            .sanitize();

        let out = renderer
            .render_into_document(content)
            .await
            .expect("Rendering failed");

        PreEscaped(String::from_utf8(out.into_inner()).expect("djot output is always valid utf8"))
    }

    /// Render bio content with only sanitization (no profile links or image
    /// transformations)
    pub(crate) async fn render_bio(&self, client: ClientRef<'_>, content: &str) -> Markup {
        // Only sanitize for bio - no profile links or image transforms
        let renderer = jotdown::html::tokio::Renderer::default()
            .rostra_profile_links(client)
            .sanitize();

        let out = renderer
            .render_into_document(content)
            .await
            .expect("Rendering failed");

        PreEscaped(String::from_utf8(out.into_inner()).expect("djot output is always valid utf8"))
    }

    /// Extra rostra id from a link `s`
    pub(crate) fn extra_rostra_id_link(s: &str) -> Option<RostraId> {
        if let Some(s) = s.strip_prefix("rostra:") {
            RostraId::from_str(s).ok()
        } else {
            None
        }
    }

    /// Extra rostra id from a link `s`
    pub(crate) fn extra_rostra_media_link(s: &str) -> Option<ShortEventId> {
        if let Some(s) = s.strip_prefix("rostra-media:") {
            ShortEventId::from_str(s).ok()
        } else {
            None
        }
    }
}

enum ExternalMedia<'s> {
    YT(Cow<'s, str>),
}

fn extract_media(url: &Url) -> Option<ExternalMedia<'_>> {
    let host_str = url.host_str()?;
    match host_str {
        "youtube.com" | "www.youtube.com" => {
            let vid = url.query_pairs().find(|(k, _)| k == "v")?.1;

            Some(ExternalMedia::YT(vid))
        }
        "youtu.be" => {
            let vid = url.path_segments()?.next_back()?;

            Some(ExternalMedia::YT(vid.into()))
        }
        _ => None,
    }
}

fn maybe_embed_media_html(s: &str) -> Option<String> {
    let Ok(url) = Url::parse(s) else {
        return None;
    };

    match extract_media(&url)? {
        ExternalMedia::YT(vid) => Some(format!(
            "<iframe loading=lazy width=\"100%\" style=\"aspect-ratio: 16 / 9;\"
 src=\"https://www.youtube.com/embed/{vid}\" frameborder=\"0\"></iframe>"
        )),
    }
}

#[cfg(test)]
mod tests;
