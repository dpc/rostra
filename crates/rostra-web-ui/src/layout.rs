use maud::{DOCTYPE, Markup, html};
use rostra_util::is_rostra_dev_mode_set;

use crate::UiState;
use crate::error::RequestResult;
use crate::routes::unlock::session::UserSession;

#[cfg(test)]
mod tests;

/// Resource sets control rich-content assets and the public-page footer
/// independently.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageResources {
    Standard,
    Private,
    PrivateRich,
}

impl PageResources {
    fn includes_rich_content(self) -> bool {
        matches!(self, Self::Standard | Self::PrivateRich)
    }
}

/// Feed discovery links for inclusion in HTML head
pub struct FeedLinks {
    pub title: String,
    pub atom_url: String,
}

/// Open Graph meta tags for rich link previews
pub struct OpenGraphMeta {
    pub title: String,
    pub description: String,
    pub url: String,
    pub image: Option<String>,
}

impl UiState {
    /// Html page header
    pub(crate) fn render_html_head(
        page_title: &str,
        feed_links: Option<&FeedLinks>,
        og: Option<&OpenGraphMeta>,
        json_ld: Option<&str>,
        noindex: bool,
        resources: PageResources,
    ) -> Markup {
        html! {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1.0, interactive-widget=resizes-content";
                meta name="color-scheme" content="light dark";
                @if noindex {
                    meta name="robots" content="noindex";
                }
                link rel="stylesheet" type="text/css" href="/assets/style.css";
                @if resources.includes_rich_content() {
                // Prism.js themes - conditionally loaded based on color scheme
                link rel="stylesheet" type="text/css" href="/assets/libs/prismjs/prism.min.css" media="(prefers-color-scheme: light)";
                link rel="stylesheet" type="text/css" href="/assets/libs/prismjs/prism-tomorrow.min.css" media="(prefers-color-scheme: dark)";
                link rel="stylesheet" type="text/css" href="/assets/libs/prismjs/prism-toolbar.min.css";
                }
                @if is_rostra_dev_mode_set() {
                    link rel="icon" type="image/svg+xml" href="/assets/favicon-dev.svg";
                } @else {
                    link rel="icon" type="image/svg+xml" href="/assets/favicon.svg";
                }
                title { (page_title) }
                // Feed discovery links
                @if let Some(links) = feed_links {
                    link rel="alternate" type="application/atom+xml"
                         title=(links.title) href=(links.atom_url);
                }
                // Meta description (from OG or fallback)
                @if let Some(og) = og {
                    meta name="description" content=(og.description);
                    link rel="canonical" href=(og.url);
                    meta property="og:type" content="article";
                    meta property="og:title" content=(og.title);
                    meta property="og:description" content=(og.description);
                    meta property="og:url" content=(og.url);
                    @if let Some(ref image) = og.image {
                        meta property="og:image" content=(image);
                    }
                    // Twitter Card meta tags
                    meta name="twitter:card" content="summary";
                    meta name="twitter:title" content=(og.title);
                    meta name="twitter:description" content=(og.description);
                    @if let Some(ref image) = og.image {
                        meta name="twitter:image" content=(image);
                    }
                } @else {
                    meta name="description" content="Rostra — a peer-to-peer social network";
                }
                // JSON-LD structured data
                @if let Some(json_ld) = json_ld {
                    script type="application/ld+json" {
                        (maud::PreEscaped(json_ld.replace('<', "\\u003c")))
                    }
                }
                noscript { link rel="stylesheet" href="/assets/nojs.css"; }
                // Load Alpine.js right away so it's immediately available, use defer to make it
                // non-blocking. ALL plugins must load BEFORE Alpine core.
                script defer src="/assets/libs/alpinejs-persist@3.14.3.js" {}
                script defer src="/assets/libs/alpinejs-intersect@3.14.3.js" {}
                script defer src="/assets/libs/alpinejs-morph@3.14.3.js" {}
                script defer src="/assets/libs/alpine-ajax@0.12.6.js" {}
                script defer src="/assets/app.js" {}
                script defer src="/assets/libs/alpinejs@3.14.3.js" {}
                @if resources.includes_rich_content() {
                // Load Prism.js for code highlighting
                // Note: C must load before C++ since C++ extends C
                script defer src="/assets/libs/prismjs/prism-core.min.js" {}
                script defer src="/assets/libs/prismjs/prism-c.min.js" {}
                script defer src="/assets/libs/prismjs/prism-cpp.min.js" {}
                script defer src="/assets/libs/prismjs/prism-javascript.min.js" {}
                script defer src="/assets/libs/prismjs/prism-python.min.js" {}
                script defer src="/assets/libs/prismjs/prism-rust.min.js" {}
                script defer src="/assets/libs/prismjs/prism-java.min.js" {}
                script defer src="/assets/libs/prismjs/prism-bash.min.js" {}
                script defer src="/assets/libs/prismjs/prism-json.min.js" {}
                script defer src="/assets/libs/prismjs/prism-yaml.min.js" {}
                script defer src="/assets/libs/prismjs/prism-markdown.min.js" {}
                script defer src="/assets/libs/prismjs/prism-sql.min.js" {}
                // Prism.js plugins - toolbar must load before copy-to-clipboard
                script defer src="/assets/libs/prismjs/prism-toolbar.min.js" {}
                script defer src="/assets/libs/prismjs/prism-copy-to-clipboard.min.js" {}
                script defer src="/assets/libs/mathjax-3.2.2/tex-mml-chtml.js" {}
                }
                @if resources == PageResources::PrivateRich {
                script type="module" src="/assets/emoji-init.js" {}
                }
            }
        }
    }

    pub async fn render_html_page(
        &self,
        title: &str,
        content: Markup,
        feed_links: Option<&FeedLinks>,
        og: Option<&OpenGraphMeta>,
        json_ld: Option<&str>,
        noindex: bool,
    ) -> RequestResult<Markup> {
        Ok(html! {
            (DOCTYPE)
            html lang="en" {
                (Self::render_html_head(title, feed_links, og, json_ld, noindex, PageResources::Standard))
                (render_html_body(content, "", PageResources::Standard))
            }
        })
    }

    /// Renders a standard two-column page layout with navbar and main content
    pub fn render_page_layout(&self, navbar: Markup, main_content: Markup) -> Markup {
        html! {
            (navbar)
            main ."o-mainBar" {
                (main_content)
            }
        }
    }

    /// Renders a full no-JS page with a timeline-like layout.
    ///
    /// Used by handlers to render a complete page when JavaScript is disabled.
    pub async fn render_nojs_full_page(
        &self,
        session: &UserSession,
        title: &str,
        body: Markup,
    ) -> RequestResult<Markup> {
        let navbar = self
            .timeline_common_navbar()
            .session(session)
            .call()
            .await?;

        let main_content = html! {
            div ."o-mainBarTimeline" {
                (Self::render_page_tab_bar(title))
                (body)
            }
        };

        let page_layout = self.render_page_layout(navbar, main_content);
        self.render_html_page(title, page_layout, None, None, None, false)
            .await
    }

    /// Renders a tab bar with a back button and a title tab.
    pub fn render_page_tab_bar(title: &str) -> Markup {
        html! {
            div ."o-mainBarTimeline__tabs" {
                a ."o-mainBarTimeline__back"
                    href="/"
                    onclick="history.back(); return false;"
                    aria-label="Back"
                    title="Back"
                {
                    span ."o-mainBarTimeline__tabIcon -back" aria-hidden="true" {}
                }
                span ."-active" { (title) }
            }
        }
    }
}

/// Render the common application body, including AJAX error notifications.
pub(crate) fn render_html_body(
    content: Markup,
    layout_class: &str,
    _resources: PageResources,
) -> Markup {
    html! {
        body ."o-body" x-data="notifications" {
            div ."o-notificationArea" {
                template x-for="notification in notifications" ":key"="notification.id" {
                    div x-cloak
                        ."o-notification"
                        ":class"=r#"{
                            '-error': notification.type === 'error',
                            '-success': notification.type === 'success',
                            '-info': notification.type === 'info'
                        }"#
                        "@click"="removeNotification(notification.id)"
                        x-text="notification.message"
                    {}
                }
            }
            div ."o-pageLayout" .(layout_class) { (content) }
        }
    }
}

/// Render the shared image viewer on pages that display social posts.
pub(crate) fn render_post_image_dialog() -> Markup {
    html! {
        dialog #post-image-dialog ."o-imageDialog" aria-label="Enlarged post image" {
            button type="button" autofocus
                "@click"="closeRostraDialog($el.closest('dialog'))" { "Close" }
            img alt="";
        }
    }
}

/// Render the shared application navbar around page-specific sidebar content.
pub(crate) fn render_app_navbar(aria_label: Option<&str>, content: Markup) -> Markup {
    html! {
        nav ."o-navBar" aria-label=[aria_label] {
            (render_top_nav())
            (content)
        }
    }
}

/// Render the common Home, Support, and Settings controls.
fn render_top_nav() -> Markup {
    html! {
        div ."o-topNav" {
            a ."o-topNav__item" href="/" {
                span ."o-topNav__icon -home" aria-hidden="true" {}
                span ."o-topNav__label" { "Home" }
            }
            a ."o-topNav__item"
                href="https://github.com/dpc/rostra/discussions"
            {
                span ."o-topNav__icon -support" aria-hidden="true" {}
                span ."o-topNav__label" { "Support" }
            }
            a ."o-topNav__item" href="/settings/profile" {
                span ."o-topNav__icon -settings" aria-hidden="true" {}
                span ."o-topNav__label" { "Settings" }
            }
        }
    }
}

/// Truncate a string at a word boundary, appending "..." if truncated.
pub fn truncate_at_word_boundary(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        return s.to_string();
    }
    let truncated: String = s.chars().take(max_len.saturating_sub(3)).collect();
    if let Some(last_space) = truncated.rfind(' ') {
        format!("{}...", &truncated[..last_space])
    } else {
        format!("{truncated}...")
    }
}
