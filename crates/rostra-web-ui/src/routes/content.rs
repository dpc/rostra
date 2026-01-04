use std::borrow::Cow;
use std::str::FromStr as _;

use jotdown::r#async::{AsyncRender, AsyncRenderOutput, AsyncRenderOutputExt};
use jotdown::html::filters::AsyncSanitizeExt;
use jotdown::{Attributes, Container, Event};
use maud::{Markup, PreEscaped};
use rostra_client::ClientRef;
use rostra_core::ShortEventId;
use rostra_core::id::RostraId;
use url::Url;

use crate::UiState;

/// Tracks what type of container we're in for proper Event::End handling
#[derive(Clone)]
enum ContainerKind {
    ProfileLink(RostraId, Option<String>),
    Other,
}

/// Filter that transforms rostra: profile links to UI profile links
/// and replaces link text with @username format
pub(crate) struct RostraProfileLinks<'c, R> {
    client: ClientRef<'c>,
    inner: R,
    container_stack: Vec<ContainerKind>,
}

impl<'c, R> RostraProfileLinks<'c, R> {
    fn new(inner: R, client: ClientRef<'c>) -> Self {
        Self {
            client,
            inner,
            container_stack: vec![],
        }
    }
}

#[async_trait::async_trait]
impl<'s, 'c, R> AsyncRender<'s> for RostraProfileLinks<'c, R>
where
    'c: 's,
    R: AsyncRender<'s> + Send,
{
    type Error = R::Error;

    async fn emit(&mut self, event: Event<'s>) -> Result<(), Self::Error> {
        match event {
            Event::Start(Container::Link(s, jotdown::LinkType::AutoLink), attr) => {
                if let Some(rostra_id) = UiState::extra_rostra_id_link(&s) {
                    let display_name = self
                        .client
                        .db()
                        .get_social_profile(rostra_id)
                        .await
                        .map(|record| record.display_name);
                    self.container_stack
                        .push(ContainerKind::ProfileLink(rostra_id, display_name));
                    self.inner
                        .emit(Event::Start(
                            Container::Link(
                                format!("/ui/profile/{rostra_id}").into(),
                                jotdown::LinkType::Span(jotdown::SpanLinkType::Inline),
                            ),
                            attr,
                        ))
                        .await
                } else {
                    self.container_stack.push(ContainerKind::Other);
                    self.inner
                        .emit(Event::Start(
                            Container::Link(s, jotdown::LinkType::AutoLink),
                            attr,
                        ))
                        .await
                }
            }
            Event::Start(container, attr) => {
                self.container_stack.push(ContainerKind::Other);
                self.inner.emit(Event::Start(container, attr)).await
            }
            Event::Str(s) => {
                // Check if we're inside a profile link
                let in_profile_link = self
                    .container_stack
                    .iter()
                    .any(|c| matches!(c, ContainerKind::ProfileLink(_, _)));
                if in_profile_link {
                    // Find the profile ID from the stack
                    if let Some(ContainerKind::ProfileLink(rostra_id, display_name)) = self
                        .container_stack
                        .iter()
                        .find(|c| matches!(c, ContainerKind::ProfileLink(_, _)))
                    {
                        self.inner
                            .emit(Event::Str(Cow::Owned(format!(
                                "@{}",
                                display_name
                                    .clone()
                                    .unwrap_or_else(|| rostra_id.to_string())
                            ))))
                            .await
                    } else {
                        self.inner.emit(Event::Str(s)).await
                    }
                } else {
                    self.inner.emit(Event::Str(s)).await
                }
            }
            Event::End => {
                self.container_stack.pop();
                self.inner.emit(Event::End).await
            }
            event => self.inner.emit(event).await,
        }
    }
}

#[async_trait::async_trait]
impl<'s, 'c, R> AsyncRenderOutput<'s> for RostraProfileLinks<'c, R>
where
    'c: 's,
    R: AsyncRenderOutput<'s> + Send,
{
    type Output = R::Output;

    fn into_output(self) -> Self::Output {
        self.inner.into_output()
    }
}

/// Tracks the type of image transformation being applied
enum ImageTransform<'s> {
    /// Regular rostra media link
    RostraMedia,
    /// External embeddable media (YouTube, etc.) - stores the HTML and alt text
    EmbeddableMedia(String, String),
    /// Regular external image - stores the URL, link type, and alt text
    ExternalImage(Cow<'s, str>, jotdown::SpanLinkType, String),
}

/// Filter that transforms images:
/// - rostra-media: links to /ui/media/{author_id}/{event_id}
/// - External embeddable media (YouTube) to lazy-loaded iframes
/// - Other external images to lazy-loaded images with "Load" messages
pub(crate) struct RostraImages<'s, R> {
    inner: R,
    author_id: RostraId,
    /// Stack tracking image transformations in progress
    /// Also tracks other containers as None to maintain proper nesting
    container_stack: Vec<Option<ImageTransform<'s>>>,
}

impl<'s, R> RostraImages<'s, R> {
    fn new(inner: R, author_id: RostraId) -> Self {
        Self {
            inner,
            author_id,
            container_stack: vec![],
        }
    }
}

#[async_trait::async_trait]
impl<'s, R> AsyncRender<'s> for RostraImages<'s, R>
where
    R: AsyncRender<'s> + Send,
{
    type Error = R::Error;

    async fn emit(&mut self, event: Event<'s>) -> Result<(), Self::Error> {
        match event {
            Event::Start(Container::Image(s, link_type), attr) => {
                if let Some(event_id) = UiState::extra_rostra_media_link(&s) {
                    // Transform rostra-media: links to /ui/media/ URLs
                    self.container_stack.push(Some(ImageTransform::RostraMedia));
                    self.inner
                        .emit(Event::Start(
                            Container::Image(
                                format!("/ui/media/{}/{}", self.author_id, event_id).into(),
                                jotdown::SpanLinkType::Inline,
                            ),
                            attr,
                        ))
                        .await
                } else {
                    // External image - check if it's embeddable media
                    if let Some(html) = maybe_embed_media_html(&s) {
                        self.container_stack
                            .push(Some(ImageTransform::EmbeddableMedia(html, String::new())));
                    } else {
                        self.container_stack
                            .push(Some(ImageTransform::ExternalImage(
                                s.clone(),
                                link_type,
                                String::new(),
                            )));
                    }
                    // Start the lazy-load wrapper div
                    self.inner
                        .emit(Event::Start(
                            Container::Div {
                                class: "lazyload-wrapper".into(),
                            },
                            jotdown::Attributes::try_from(
                                "{ onclick=\"this.classList.add('-expanded')\" }",
                            )
                            .expect("Can't fail"),
                        ))
                        .await
                }
            }
            Event::Start(container, attr) => {
                self.container_stack.push(None);
                self.inner.emit(Event::Start(container, attr)).await
            }
            Event::Str(s) => {
                // If we're inside an image transformation, capture the alt text
                if let Some(Some(transform)) = self.container_stack.last_mut() {
                    match transform {
                        ImageTransform::RostraMedia => {
                            // For rostra media, pass through the str
                            self.inner.emit(Event::Str(s)).await
                        }
                        ImageTransform::EmbeddableMedia(_, alt) => {
                            // Capture alt text, skip emitting the str for now
                            *alt = s.to_string();
                            Ok(())
                        }
                        ImageTransform::ExternalImage(_, _, alt) => {
                            // Capture alt text, skip emitting the str for now
                            *alt = s.to_string();
                            Ok(())
                        }
                    }
                } else {
                    self.inner.emit(Event::Str(s)).await
                }
            }
            Event::End => {
                if let Some(Some(transform)) = self.container_stack.pop() {
                    match transform {
                        ImageTransform::RostraMedia => {
                            // Just emit End for rostra media
                            self.inner.emit(Event::End).await
                        }
                        ImageTransform::EmbeddableMedia(html, alt) => {
                            // Emit the load message and embedded HTML
                            let alt = alt.trim();
                            let load_msg = if alt.is_empty() {
                                "Load external media".to_string()
                            } else {
                                format!("Load \"{alt}\"")
                            };

                            self.inner
                                .emit(Event::Start(Container::Paragraph, Attributes::new()))
                                .await?;
                            self.inner.emit(Event::Str(load_msg.into())).await?;
                            self.inner.emit(Event::End).await?;
                            self.inner
                                .emit(Event::Start(
                                    Container::RawInline {
                                        format: "html".into(),
                                    },
                                    Attributes::try_from("{ loading=lazy }").expect("Can't fail"),
                                ))
                                .await?;
                            self.inner.emit(Event::Str(html.into())).await?;
                            self.inner.emit(Event::End).await?;
                            // Close the div
                            self.inner.emit(Event::End).await
                        }
                        ImageTransform::ExternalImage(s, link_type, alt) => {
                            // Emit load message and the actual image
                            let alt = alt.trim();
                            let load_msg = if alt.is_empty() {
                                format!("Load: {s}")
                            } else {
                                format!("Load \"{alt}\": {s}")
                            };

                            self.inner
                                .emit(Event::Start(Container::Paragraph, Attributes::new()))
                                .await?;
                            self.inner.emit(Event::Str(load_msg.into())).await?;
                            self.inner.emit(Event::End).await?;
                            self.inner
                                .emit(Event::Start(
                                    Container::Image(s.clone(), link_type),
                                    Attributes::try_from("{ loading=lazy }").expect("Can't fail"),
                                ))
                                .await?;
                            self.inner.emit(Event::Str(alt.to_string().into())).await?;
                            self.inner.emit(Event::End).await?;
                            // Close the div
                            self.inner.emit(Event::End).await
                        }
                    }
                } else {
                    self.container_stack.pop();
                    self.inner.emit(Event::End).await
                }
            }
            event => self.inner.emit(event).await,
        }
    }
}

#[async_trait::async_trait]
impl<'s, R> AsyncRenderOutput<'s> for RostraImages<'s, R>
where
    R: AsyncRenderOutput<'s> + Send,
{
    type Output = R::Output;

    fn into_output(self) -> Self::Output {
        self.inner.into_output()
    }
}

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
        // Images -> Sanitize
        let renderer = jotdown::html::tokio::Renderer::default()
            .rostra_profile_links(client.clone())
            .rostra_images(author_id)
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
