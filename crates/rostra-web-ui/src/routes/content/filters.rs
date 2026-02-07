use std::borrow::Cow;

use jotup::r#async::{AsyncRender, AsyncRenderOutput};
use jotup::{AttributeKind, AttributeValue, Attributes, Container, Event};
use rostra_client::ClientRef;
use rostra_core::ShortEventId;
use rostra_core::event::content_kind;
use rostra_core::id::RostraId;

use crate::UiState;

/// Escape HTML special characters for use in attributes and text
fn escape_html(s: &str) -> String {
    s.chars()
        .flat_map(|c| match c {
            '&' => vec!['&', 'a', 'm', 'p', ';'],
            '<' => vec!['&', 'l', 't', ';'],
            '>' => vec!['&', 'g', 't', ';'],
            '"' => vec!['&', 'q', 'u', 'o', 't', ';'],
            '\'' => vec!['&', '#', '3', '9', ';'],
            c => vec![c],
        })
        .collect()
}

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
                if let Some(rostra_id) = UiState::extract_rostra_id_link(&s) {
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

/// Tracks the type of media transformation being applied
enum MediaTransform<'s> {
    /// Rostra media link
    RostraMedia { event_id: ShortEventId, alt: String },
    /// External embeddable media (YouTube, etc.)
    EmbeddableMedia {
        html: String,
        hostname: String,
        alt: String,
    },
    /// Regular external image
    ExternalImage {
        url: Cow<'s, str>,
        link_type: jotup::SpanLinkType,
        alt: String,
    },
}

/// Filter that transforms media elements:
/// - rostra-media: links rendered based on mime type (image/video/download)
/// - External embeddable media (YouTube) to lazy-loaded iframes
/// - Other external images to lazy-loaded images with "Load" messages
pub(crate) struct RostraMedia<'s, 'c, R> {
    client: ClientRef<'c>,
    inner: R,
    author_id: RostraId,
    /// Stack tracking media transformations in progress
    /// Also tracks other containers as None to maintain proper nesting
    container_stack: Vec<Option<MediaTransform<'s>>>,
}

impl<'s, 'c, R> RostraMedia<'s, 'c, R> {
    pub(crate) fn new(inner: R, client: ClientRef<'c>, author_id: RostraId) -> Self {
        Self {
            client,
            inner,
            author_id,
            container_stack: vec![],
        }
    }
}

#[async_trait::async_trait]
impl<'s, 'c, R> AsyncRender<'s> for RostraMedia<'s, 'c, R>
where
    'c: 's,
    R: AsyncRender<'s> + Send,
{
    type Error = R::Error;

    async fn emit(&mut self, event: Event<'s>) -> Result<(), Self::Error> {
        match event {
            Event::Start(Container::Image(s, link_type), _attr) => {
                if let Some(event_id) = UiState::extract_rostra_media_link(&s) {
                    // Store event_id to look up content later
                    self.container_stack.push(Some(MediaTransform::RostraMedia {
                        event_id,
                        alt: String::new(),
                    }));
                    // Don't emit Start yet - we'll emit everything in End
                    Ok(())
                } else {
                    // External image - check if it's embeddable media
                    if let Some(html) = super::maybe_embed_media_html(&s) {
                        let hostname = url::Url::parse(&s)
                            .ok()
                            .and_then(|u| u.host_str().map(|h| h.to_string()))
                            .unwrap_or_default();
                        self.container_stack
                            .push(Some(MediaTransform::EmbeddableMedia {
                                html,
                                hostname,
                                alt: String::new(),
                            }));
                    } else {
                        self.container_stack
                            .push(Some(MediaTransform::ExternalImage {
                                url: s.clone(),
                                link_type,
                                alt: String::new(),
                            }));
                    }
                    // Start the lazy-load wrapper div
                    self.inner
                        .emit(Event::Start(
                            Container::Div {
                                class: "lazyload-wrapper".into(),
                            },
                            jotup::Attributes::try_from(
                                "{ onclick=\"this.classList.add('-expanded'); this.lastElementChild.src = this.lastElementChild.dataset.src\" }",
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
                // If we're inside a media transformation, capture the alt text
                if let Some(Some(transform)) = self.container_stack.last_mut() {
                    match transform {
                        MediaTransform::RostraMedia { alt, .. }
                        | MediaTransform::EmbeddableMedia { alt, .. }
                        | MediaTransform::ExternalImage { alt, .. } => {
                            // Append to alt text (may be split across multiple Str events)
                            alt.push_str(&s);
                            Ok(())
                        }
                    }
                } else {
                    self.inner.emit(Event::Str(s)).await
                }
            }
            // Handle smart punctuation within media alt text
            Event::LeftSingleQuote
            | Event::RightSingleQuote
            | Event::LeftDoubleQuote
            | Event::RightDoubleQuote
            | Event::Ellipsis
            | Event::EnDash
            | Event::EmDash => {
                if let Some(Some(transform)) = self.container_stack.last_mut() {
                    // Append the punctuation character to alt text
                    // Using Unicode escape sequences for curly quotes
                    let ch = match event {
                        Event::LeftSingleQuote => '\u{2018}',  // '
                        Event::RightSingleQuote => '\u{2019}', // '
                        Event::LeftDoubleQuote => '\u{201C}',  // "
                        Event::RightDoubleQuote => '\u{201D}', // "
                        Event::Ellipsis => '\u{2026}',         // …
                        Event::EnDash => '\u{2013}',           // –
                        Event::EmDash => '\u{2014}',           // —
                        _ => unreachable!(),
                    };
                    match transform {
                        MediaTransform::RostraMedia { alt, .. }
                        | MediaTransform::EmbeddableMedia { alt, .. }
                        | MediaTransform::ExternalImage { alt, .. } => {
                            alt.push(ch);
                        }
                    }
                    Ok(())
                } else {
                    self.inner.emit(event).await
                }
            }
            // Handle whitespace/break events within media alt text
            Event::NonBreakingSpace | Event::Softbreak => {
                if let Some(Some(transform)) = self.container_stack.last_mut() {
                    match transform {
                        MediaTransform::RostraMedia { alt, .. }
                        | MediaTransform::EmbeddableMedia { alt, .. }
                        | MediaTransform::ExternalImage { alt, .. } => {
                            alt.push(' ');
                        }
                    }
                    Ok(())
                } else {
                    self.inner.emit(event).await
                }
            }
            Event::Hardbreak => {
                if let Some(Some(transform)) = self.container_stack.last_mut() {
                    match transform {
                        MediaTransform::RostraMedia { alt, .. }
                        | MediaTransform::EmbeddableMedia { alt, .. }
                        | MediaTransform::ExternalImage { alt, .. } => {
                            alt.push('\n');
                        }
                    }
                    Ok(())
                } else {
                    self.inner.emit(event).await
                }
            }
            // Handle symbol syntax (:sym:) within media alt text
            Event::Symbol(s) => {
                if let Some(Some(transform)) = self.container_stack.last_mut() {
                    match transform {
                        MediaTransform::RostraMedia { alt, .. }
                        | MediaTransform::EmbeddableMedia { alt, .. }
                        | MediaTransform::ExternalImage { alt, .. } => {
                            // Render as :symbol_name:
                            alt.push(':');
                            alt.push_str(&s);
                            alt.push(':');
                        }
                    }
                    Ok(())
                } else {
                    self.inner.emit(Event::Symbol(s)).await
                }
            }
            // Escape events are markers for escaped characters, skip them in alt text
            Event::Escape => {
                if self.container_stack.last().is_some_and(|t| t.is_some()) {
                    // Inside media transform, just skip the escape marker
                    Ok(())
                } else {
                    self.inner.emit(Event::Escape).await
                }
            }
            Event::End => {
                if let Some(Some(transform)) = self.container_stack.pop() {
                    match transform {
                        MediaTransform::RostraMedia { event_id, alt } => {
                            // Look up the content to get mime type
                            let url = format!("/ui/media/{}/{}", self.author_id, event_id);
                            let alt = alt.trim();
                            let display_name = if alt.is_empty() { "media" } else { alt };

                            // Sanitize filename from alt text
                            let filename: String = display_name
                                .chars()
                                .map(|c| {
                                    if c.is_ascii_alphanumeric() || c == '.' || c == '_' {
                                        c
                                    } else {
                                        '-'
                                    }
                                })
                                .collect();

                            let url_escaped = escape_html(&url);
                            let alt_escaped = escape_html(alt);
                            let filename_escaped = escape_html(&filename);
                            let display_escaped = escape_html(display_name);

                            // Look up content from database
                            let html = if let Some(content) =
                                self.client.db().get_event_content(event_id).await
                            {
                                if let Ok(media) =
                                    content.deserialize_cbor::<content_kind::SocialMedia>()
                                {
                                    if media.mime.starts_with("image/") {
                                        // Render as image
                                        format!(
                                            r#"<span class="m-rostraMedia"><img src="{url_escaped}" alt="{alt_escaped}"/></span>"#
                                        )
                                    } else if media.mime.starts_with("video/") {
                                        // Render as video player - plays when visible via
                                        // IntersectionObserver
                                        format!(
                                            r#"<span class="m-rostraMedia"><video src="{url_escaped}" controls muted playsinline class="m-rostraMedia__video"></video></span>"#
                                        )
                                    } else {
                                        // Render as download link
                                        format!(
                                            r#"<span class="m-rostraMedia"><a href="{url_escaped}" download="{filename_escaped}" class="m-rostraMedia__download"><span class="m-rostraMedia__downloadIcon"></span>{display_escaped}</a></span>"#
                                        )
                                    }
                                } else {
                                    // Failed to deserialize - show as download
                                    format!(
                                        r#"<span class="m-rostraMedia"><a href="{url_escaped}" download="{filename_escaped}" class="m-rostraMedia__download"><span class="m-rostraMedia__downloadIcon"></span>{display_escaped}</a></span>"#
                                    )
                                }
                            } else {
                                // Content not available yet
                                r#"<span class="m-rostraMedia -unavailable"><span class="m-rostraMedia__unavailableIcon"></span><span class="m-rostraMedia__unavailableText">Content not available yet</span></span>"#.to_string()
                            };

                            self.inner
                                .emit(Event::Start(
                                    Container::RawInline {
                                        format: "html".into(),
                                    },
                                    Attributes::new(),
                                ))
                                .await?;
                            self.inner.emit(Event::Str(html.into())).await?;
                            self.inner.emit(Event::End).await
                        }
                        MediaTransform::EmbeddableMedia {
                            html,
                            hostname,
                            alt,
                        } => {
                            // Emit the load message and embedded HTML
                            let alt = alt.trim();
                            let load_msg = if hostname.is_empty() {
                                if alt.is_empty() {
                                    "Load external media".to_string()
                                } else {
                                    format!("Load \"{alt}\"")
                                }
                            } else if alt.is_empty() {
                                format!("Load {hostname} media")
                            } else {
                                format!("Load {hostname} media: \"{alt}\"")
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
                        MediaTransform::ExternalImage {
                            url,
                            link_type,
                            alt,
                        } => {
                            // Emit load message and the actual image
                            let alt = alt.trim();
                            let hostname = url::Url::parse(&url)
                                .ok()
                                .and_then(|u| u.host_str().map(|h| h.to_string()))
                                .unwrap_or_default();
                            let load_msg = if hostname.is_empty() {
                                if alt.is_empty() {
                                    "Load external image".to_string()
                                } else {
                                    format!("Load image: \"{alt}\"")
                                }
                            } else if alt.is_empty() {
                                format!("Load {hostname} image")
                            } else {
                                format!("Load {hostname} image: \"{alt}\"")
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
                            // Use placeholder src and data-src to prevent eager loading
                            // 1x1 transparent gif as placeholder
                            const PLACEHOLDER: &str = "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7";
                            let mut attrs = Attributes::new();
                            attrs.push((
                                AttributeKind::Pair {
                                    key: Cow::Borrowed("data-src"),
                                },
                                AttributeValue::from(url.clone()),
                            ));
                            self.inner
                                .emit(Event::Start(
                                    Container::Image(PLACEHOLDER.into(), link_type),
                                    attrs,
                                ))
                                .await?;
                            self.inner.emit(Event::Str(alt.to_string().into())).await?;
                            self.inner.emit(Event::End).await?;
                            // Close the div
                            self.inner.emit(Event::End).await
                        }
                    }
                } else {
                    // Not a media transform - just pass through the End event
                    // Note: the pop already happened in the if-let condition above
                    self.inner.emit(Event::End).await
                }
            }
            event => self.inner.emit(event).await,
        }
    }
}

#[async_trait::async_trait]
impl<'s, 'c, R> AsyncRenderOutput<'s> for RostraMedia<'s, 'c, R>
where
    'c: 's,
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

/// Filter that sanitizes dangerous URL protocols (javascript:, vbscript:,
/// data:) in links and images. All data: URLs are blocked out of caution.
pub(crate) struct SanitizeUrls<R> {
    inner: R,
}

impl<R> SanitizeUrls<R> {
    pub(crate) fn new(inner: R) -> Self {
        Self { inner }
    }
}

impl<R> SanitizeUrls<R> {
    /// Check if a URL uses a dangerous protocol that could execute code
    fn is_dangerous_url(url: &str) -> bool {
        let url_lower = url.trim().to_lowercase();
        url_lower.starts_with("javascript:")
            || url_lower.starts_with("vbscript:")
            || url_lower.starts_with("data:")
    }
}

#[async_trait::async_trait]
impl<'s, R> AsyncRender<'s> for SanitizeUrls<R>
where
    R: AsyncRender<'s> + Send,
{
    type Error = R::Error;

    async fn emit(&mut self, event: Event<'s>) -> Result<(), Self::Error> {
        match event {
            Event::Start(Container::Link(url, link_type), attr) => {
                if Self::is_dangerous_url(&url) {
                    // Replace dangerous URL with safe "#"
                    self.inner
                        .emit(Event::Start(
                            Container::Link(Cow::Borrowed("#"), link_type),
                            attr,
                        ))
                        .await
                } else {
                    self.inner
                        .emit(Event::Start(Container::Link(url, link_type), attr))
                        .await
                }
            }
            Event::Start(Container::Image(url, link_type), attr) => {
                if Self::is_dangerous_url(&url) {
                    // Replace dangerous URL with empty string (broken image is safer)
                    self.inner
                        .emit(Event::Start(
                            Container::Image(Cow::Borrowed(""), link_type),
                            attr,
                        ))
                        .await
                } else {
                    self.inner
                        .emit(Event::Start(Container::Image(url, link_type), attr))
                        .await
                }
            }
            event => self.inner.emit(event).await,
        }
    }
}

#[async_trait::async_trait]
impl<'s, R> AsyncRenderOutput<'s> for SanitizeUrls<R>
where
    R: AsyncRenderOutput<'s> + Send,
{
    type Output = R::Output;

    fn into_output(self) -> Self::Output {
        self.inner.into_output()
    }
}
