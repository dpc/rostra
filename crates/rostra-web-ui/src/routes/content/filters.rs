use std::borrow::Cow;

use jotup::r#async::{AsyncRender, AsyncRenderOutput};
use jotup::{AttributeKind, AttributeValue, Attributes, Container, Event};
use rostra_client::ClientRef;
use rostra_core::id::RostraId;

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
    pub(crate) fn new(inner: R, client: ClientRef<'c>) -> Self {
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
            Event::Start(Container::Link(s, jotup::LinkType::AutoLink), attr) => {
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
                                jotup::LinkType::Span(jotup::SpanLinkType::Inline),
                            ),
                            attr,
                        ))
                        .await
                } else {
                    self.container_stack.push(ContainerKind::Other);
                    self.inner
                        .emit(Event::Start(
                            Container::Link(s, jotup::LinkType::AutoLink),
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
    ExternalImage(Cow<'s, str>, jotup::SpanLinkType, String),
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
    pub(crate) fn new(inner: R, author_id: RostraId) -> Self {
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
                                jotup::SpanLinkType::Inline,
                            ),
                            attr,
                        ))
                        .await
                } else {
                    // External image - check if it's embeddable media
                    if let Some(html) = super::maybe_embed_media_html(&s) {
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
                            jotup::Attributes::try_from(
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
                                .emit(Event::Start(
                                    Container::Paragraph,
                                    Attributes::try_from("{ class=\"lazyload-message -video\" }")
                                        .expect("valid"),
                                ))
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
                                .emit(Event::Start(
                                    Container::Paragraph,
                                    Attributes::try_from("{ class=\"lazyload-message -image\" }")
                                        .expect("valid"),
                                ))
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

/// Filter that adds Prism.js classes to code blocks for syntax highlighting
pub(crate) struct PrismCodeBlocks<R> {
    inner: R,
}

impl<R> PrismCodeBlocks<R> {
    pub(crate) fn new(inner: R) -> Self {
        Self { inner }
    }
}

#[async_trait::async_trait]
impl<'s, R> AsyncRender<'s> for PrismCodeBlocks<R>
where
    R: AsyncRender<'s> + Send,
{
    type Error = R::Error;

    async fn emit(&mut self, event: Event<'s>) -> Result<(), Self::Error> {
        match event {
            Event::Start(Container::CodeBlock { language }, attr) => {
                // Add language-xxx class for Prism.js
                let new_attr = if !language.is_empty() {
                    let class_value = format!("language-{}", language.trim());
                    // Create attributes with owned strings
                    let mut attrs = attr;
                    attrs.push((
                        AttributeKind::Pair {
                            key: Cow::Borrowed("class"),
                        },
                        AttributeValue::from(Cow::Owned(class_value)),
                    ));
                    attrs
                } else {
                    attr
                };
                self.inner
                    .emit(Event::Start(Container::CodeBlock { language }, new_attr))
                    .await
            }
            event => self.inner.emit(event).await,
        }
    }
}

#[async_trait::async_trait]
impl<'s, R> AsyncRenderOutput<'s> for PrismCodeBlocks<R>
where
    R: AsyncRenderOutput<'s> + Send,
{
    type Output = R::Output;

    fn into_output(self) -> Self::Output {
        self.inner.into_output()
    }
}
