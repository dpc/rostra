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
        self.render_content_with_external_media(client, author_id, content, true)
            .await
    }

    /// Render private content with navigation-only external media cards.
    pub(crate) async fn render_private_content(
        &self,
        client: &ClientRef<'_>,
        author_id: RostraId,
        content: &str,
    ) -> Markup {
        self.render_content_with_external_media(client, author_id, content, false)
            .await
    }

    async fn render_content_with_external_media(
        &self,
        client: &ClientRef<'_>,
        author_id: RostraId,
        content: &str,
        allow_external_media: bool,
    ) -> Markup {
        // Compose filters: ProfileLinks -> Media -> (Prism + Sanitize via
        // make_base_renderer)
        let renderer = make_base_renderer(
            jotup::html::tokio::Renderer::default()
                .rostra_profile_links(client.clone())
                .rostra_media(client.clone(), author_id)
                .external_media(allow_external_media),
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

    /// Extract rostra media id from a link `s`
    pub(crate) fn extract_rostra_media_link(s: &str) -> Option<ShortEventId> {
        rostra_djot::links::extract_rostra_media_link(s)
    }
}

enum ExternalMedia<'s> {
    YT(Cow<'s, str>),
    XPost,
}

fn extract_media(url: &Url) -> Option<ExternalMedia<'_>> {
    if x_post_id(url).is_some() {
        return Some(ExternalMedia::XPost);
    }
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

/// Recognize credential-free Twitter/X post URLs, never arbitrary embed URLs.
pub(super) fn x_post_id(url: &Url) -> Option<&str> {
    if !matches!(url.scheme(), "http" | "https")
        || !matches!(
            url.host_str()?,
            "x.com" | "www.x.com" | "twitter.com" | "www.twitter.com" | "mobile.twitter.com"
        )
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        return None;
    }
    let parts: Vec<_> = url.path_segments()?.collect();
    let (user, id, suffix) = match parts.as_slice() {
        [user, "status", id, suffix @ ..] => (*user, *id, suffix),
        ["i", "web", "status", id, suffix @ ..] => ("i", *id, suffix),
        _ => return None,
    };
    if user.is_empty()
        || user.len() > 15
        || !user.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
        || id.is_empty()
        || id.len() > 20
        || !id.bytes().all(|c| c.is_ascii_digit())
        || id.parse::<u64>().ok()? == 0
        || !matches!(
            suffix,
            [] | [""] | ["photo" | "video", "1" | "2" | "3" | "4"]
        )
    {
        return None;
    }
    Some(id)
}

/// Render a post through the provider's cross-origin frame, never app-origin
/// JS.
///
/// Keep the fixed provider origin: allowing its own origin inside this sandbox
/// is not safe for a srcdoc or same-origin replacement. Private-page CSP blocks
/// the initial third-party frame navigation.
fn x_post_html(id: &str, title: &str, allow_external_media: bool) -> String {
    let canonical = format!("https://twitter.com/i/web/status/{id}");
    external_media_card(
        &canonical,
        title,
        "Load X media",
        "-video",
        allow_external_media,
        maud::html! {
                iframe class="m-xPostFrame" data-x-post title="X / Twitter post" width="100%" height="200" hidden
                    loading="lazy" referrerpolicy="no-referrer"
                    sandbox="allow-scripts allow-same-origin allow-popups allow-popups-to-escape-sandbox"
                    data-src=(format!("https://platform.twitter.com/embed/Tweet.html?id={id}&dnt=true&embedId=rostra-x-{id}")) {}
        },
    )
}

/// Generate trusted markup with escaped data and an ordinary navigation
/// fallback.
fn external_media_card(
    url: &str,
    title: &str,
    action: &str,
    icon: &str,
    allow_external_media: bool,
    media: Markup,
) -> String {
    maud::html! {
        div class="lazyload-wrapper" {
            a class=(format!("lazyload-message {icon}")) href=(url) rel="noopener noreferrer"
                x-data=[allow_external_media.then_some("{}")]
                "@click"=[allow_external_media.then_some("expandExternalMedia($event)")] { (action) }
            @if allow_external_media { (media) }
            @if !title.trim().is_empty() {
                p class="lazyload-caption" { (title.trim()) }
            }
        }
    }.into_string()
}

/// Whether an external URL uses the renderer's image-style embed syntax.
pub(super) fn is_external_embed(url: &Url) -> bool {
    extract_media(url).is_some()
}

/// Extract the video ID using the existing YouTube embed recognition.
pub(super) fn youtube_video_id(url: &Url) -> Option<Cow<'_, str>> {
    match extract_media(url)? {
        ExternalMedia::YT(id) => Some(id),
        ExternalMedia::XPost => None,
    }
}

fn maybe_embed_media_html(s: &str) -> Option<Markup> {
    let Ok(url) = Url::parse(s) else {
        return None;
    };

    match extract_media(&url)? {
        ExternalMedia::YT(vid) => {
            let mut embed = Url::parse("https://www.youtube.com/embed/").expect("fixed URL");
            embed
                .path_segments_mut()
                .expect("base URL")
                .pop_if_empty()
                .push(&vid);
            Some(maud::html! {
                iframe hidden loading="lazy" title="YouTube video" width="100%"
                    style="aspect-ratio: 16 / 9;" data-src=(embed.as_str())
                    referrerpolicy="no-referrer" frameborder="0" {}
            })
        }
        ExternalMedia::XPost => None,
    }
}

#[cfg(test)]
mod tests;
