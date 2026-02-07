use std::borrow::Cow;

use jotup::r#async::{AsyncRender, AsyncRenderOutputExt};
use jotup::html::filters::AsyncSanitizeExt;
use maud::{Markup, PreEscaped};
use rostra_client::ClientRef;
use rostra_core::ShortEventId;
use rostra_core::id::RostraId;
use url::Url;

use crate::UiState;

mod filters;

use filters::{PrismCodeBlocks, RostraMedia, RostraProfileLinks, SanitizeUrls};

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

    /// Transform media elements (rostra-media: links rendered based on mime
    /// type)
    fn rostra_media<'s, 'c>(
        self,
        client: ClientRef<'c>,
        author_id: RostraId,
    ) -> RostraMedia<'s, 'c, Self>
    where
        Self: Sized + AsyncRender<'s>,
    {
        RostraMedia::new(self, client, author_id)
    }

    /// Add Prism.js classes to code blocks for syntax highlighting
    fn prism_code_blocks(self) -> PrismCodeBlocks<Self>
    where
        Self: Sized,
    {
        PrismCodeBlocks::new(self)
    }

    /// Sanitize dangerous URL protocols (javascript:, vbscript:, data:)
    fn sanitize_urls(self) -> SanitizeUrls<Self>
    where
        Self: Sized,
    {
        SanitizeUrls::new(self)
    }
}

impl<'s, R> RostraRenderExt for R where R: Sized + AsyncRender<'s> {}

/// Apply standard output filters (URL sanitization + syntax highlighting + XSS
/// sanitization).
///
/// This is the final processing step for all content rendering. Takes an inner
/// renderer and wraps it with URL sanitization, prism code blocks, and HTML
/// sanitization.
///
/// - Production:
///   `make_base_renderer(Renderer::default().profile_links().media())`
/// - Tests: `make_base_renderer(Renderer::default())`
pub(crate) fn make_base_renderer<'s, R>(
    renderer: R,
) -> jotup::html::filters::AsyncSanitize<PrismCodeBlocks<SanitizeUrls<R>>>
where
    R: AsyncRender<'s> + Send,
    SanitizeUrls<R>: AsyncRender<'s> + Send,
    PrismCodeBlocks<SanitizeUrls<R>>: AsyncRender<'s> + Send,
{
    renderer.sanitize_urls().prism_code_blocks().sanitize()
}

impl UiState {
    pub(crate) async fn render_content(
        &self,
        client: &ClientRef<'_>,
        author_id: RostraId,
        content: &str,
    ) -> Markup {
        // Compose filters: ProfileLinks -> Media -> (Prism + Sanitize via
        // make_base_renderer)
        let renderer = make_base_renderer(
            jotup::html::tokio::Renderer::default()
                .rostra_profile_links(client.clone())
                .rostra_media(client.clone(), author_id),
        );

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
        let renderer = jotup::html::tokio::Renderer::default()
            .rostra_profile_links(client)
            .sanitize();

        let out = renderer
            .render_into_document(content)
            .await
            .expect("Rendering failed");

        PreEscaped(String::from_utf8(out.into_inner()).expect("djot output is always valid utf8"))
    }

    /// Extract rostra id from a link `s`
    pub(crate) fn extract_rostra_id_link(s: &str) -> Option<RostraId> {
        rostra_djot::links::extract_rostra_id_link(s)
    }

    /// Extract rostra media id from a link `s`
    pub(crate) fn extract_rostra_media_link(s: &str) -> Option<ShortEventId> {
        rostra_djot::links::extract_rostra_media_link(s)
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
            "<iframe loading=\"lazy\" width=\"100%\" style=\"aspect-ratio: 16 / 9;\" \
data-src=\"https://www.youtube.com/embed/{vid}\" frameborder=\"0\"></iframe>"
        )),
    }
}

#[cfg(test)]
mod tests;
