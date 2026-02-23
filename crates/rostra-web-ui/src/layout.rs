use maud::{DOCTYPE, Markup, html};

use crate::UiState;
use crate::error::RequestResult;
use crate::routes::unlock::session::UserSession;

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
        &self,
        page_title: &str,
        feed_links: Option<&FeedLinks>,
        og: Option<&OpenGraphMeta>,
    ) -> Markup {
        html! {
            (DOCTYPE)
            html lang="en";
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1.0";
                meta name="color-scheme" content="light dark";
                link rel="stylesheet" type="text/css" href="/assets/style.css";
                // Prism.js themes - conditionally loaded based on color scheme
                link rel="stylesheet" type="text/css" href="/assets/libs/prismjs/prism.min.css" media="(prefers-color-scheme: light)";
                link rel="stylesheet" type="text/css" href="/assets/libs/prismjs/prism-tomorrow.min.css" media="(prefers-color-scheme: dark)";
                link rel="stylesheet" type="text/css" href="/assets/libs/prismjs/prism-toolbar.min.css";
                link rel="icon" type="image/png" href="/assets/favicon.svg";
                title { (page_title) }
                // Feed discovery links
                @if let Some(links) = feed_links {
                    link rel="alternate" type="application/atom+xml"
                         title=(links.title) href=(links.atom_url);
                }
                // Open Graph meta tags
                @if let Some(og) = og {
                    meta property="og:type" content="article";
                    meta property="og:title" content=(og.title);
                    meta property="og:description" content=(og.description);
                    meta property="og:url" content=(og.url);
                    @if let Some(ref image) = og.image {
                        meta property="og:image" content=(image);
                    }
                }
                // Hide elements with x-cloak until Alpine initializes
                style { "[x-cloak] { display: none !important; }" }
                // Load Alpine.js right away so it's immediately available, use defer to make it
                // non-blocking. ALL plugins must load BEFORE Alpine core.
                script defer src="/assets/libs/alpinejs-persist@3.14.3.js" {}
                script defer src="/assets/libs/alpinejs-intersect@3.14.3.js" {}
                script defer src="/assets/libs/alpinejs-morph@3.14.3.js" {}
                script defer src="/assets/libs/alpine-ajax@0.12.6.js" {}
                script defer src="/assets/app.js" {}
                script defer src="/assets/libs/alpinejs@3.14.3.js" {}
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
            }
        }
    }

    pub async fn render_html_page(
        &self,
        title: &str,
        content: Markup,
        feed_links: Option<&FeedLinks>,
        og: Option<&OpenGraphMeta>,
    ) -> RequestResult<Markup> {
        Ok(html! {
            (self.render_html_head(title, feed_links, og))
            body ."o-body"
                x-data="notifications"
            {
                // Global notification area
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

                div ."o-pageLayout" { (content) }
                (render_html_footer())
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
        self.render_html_page(title, page_layout, None, None).await
    }

    /// Renders a tab bar with a back button and a title tab.
    pub fn render_page_tab_bar(title: &str) -> Markup {
        html! {
            div ."o-mainBarTimeline__tabs" {
                a ."o-mainBarTimeline__back" href="/" onclick="history.back(); return false;" { "<" }
                span ."-active" { (title) }
            }
        }
    }

    /// Renders the top navigation bar with Home, Support, and Settings links
    pub fn render_top_nav(&self) -> Markup {
        html! {
            div ."o-topNav" {
                a ."o-topNav__item" href="/" {
                    span ."o-topNav__icon -home" {}
                    "Home"
                }
                a ."o-topNav__item" href="https://github.com/dpc/rostra/discussions" {
                    span ."o-topNav__icon -support" {}
                    "Support"
                }
                a ."o-topNav__item" href="/settings" {
                    span ."o-topNav__icon -settings" {}
                    "Settings"
                }
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

/// A static footer.
pub(crate) fn render_html_footer() -> Markup {
    html! {
        script defer src="/assets/libs/mathjax-3.2.2/tex-mml-chtml.js" {}
    }
}
