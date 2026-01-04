use maud::{DOCTYPE, Markup, PreEscaped, html};

use crate::UiState;
use crate::error::RequestResult;

impl UiState {
    /// Html page header
    pub(crate) fn render_html_head(&self, page_title: &str) -> Markup {
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
                link rel="icon" type="image/png" href="/assets/favicon.png";
                title { (page_title) }
                // Hide elements with x-cloak until Alpine initializes
                style { "[x-cloak] { display: none !important; }" }
                // Load Alpine.js right away so it's immediately available, use defer to make it
                // non-blocking. ALL plugins must load BEFORE Alpine core.
                script defer src="/assets/libs/alpinejs-intersect@3.14.3.js" {}
                script defer src="/assets/libs/alpine-ajax@0.12.6.js" {}
                script defer src="/assets/libs/alpinejs@3.14.3.js" {}
                // Disable Prism.js automatic highlighting - we'll do it manually
                script {
                    "window.Prism = window.Prism || {}; Prism.manual = true;"
                }
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
                script defer src="/assets/libs/prismjs/prism-autoloader.min.js" {}
                // Prism.js plugins - toolbar must load before copy-to-clipboard
                script defer src="/assets/libs/prismjs/prism-toolbar.min.js" {}
                script defer src="/assets/libs/prismjs/prism-copy-to-clipboard.min.js" {}
            }
        }
    }

    pub async fn render_html_page(&self, title: &str, content: Markup) -> RequestResult<Markup> {
        Ok(html! {
            (self.render_html_head(title))
            body ."o-body"
                x-data=r#"{
                    notifications: [],
                    nextId: 1,
                    addNotification(type, message, duration = null) {
                        // Check if this exact message already exists
                        const exists = this.notifications.some(n => n.message === message && n.type === type);
                        if (exists) {
                            return; // Don't add duplicate
                        }

                        const id = this.nextId++;
                        this.notifications.push({ id, type, message });

                        // Auto-dismiss all notification types
                        const dismissTime = duration !== null ? duration :
                                          type === 'error' ? 8000 :
                                          4000;
                        setTimeout(() => {
                            this.removeNotification(id);
                        }, dismissTime);
                    },
                    removeNotification(id) {
                        this.notifications = this.notifications.filter(n => n.id !== id);
                    },
                    clearErrorNotifications() {
                        this.notifications = this.notifications.filter(n => n.type !== 'error');
                    }
                }"#
                "@ajax:error.window"=r#"
                    console.log('AJAX error event:', $event.detail);
                    const xhr = $event.detail.xhr || $event.detail;
                    const status = xhr?.status;
                    let message;
                    if (status === 0 || status === undefined) {
                        message = '⚠ Network Error - Unable to complete request';
                    } else if (status >= 500) {
                        message = '⚠ Server Error (' + status + ')';
                    } else if (status >= 400) {
                        message = '⚠ Request Failed (' + status + ')';
                    }
                    if (message) {
                        $data.addNotification('error', message);
                    }
                "#
                "@ajax:success.window"="$data.clearErrorNotifications()"
                "@notify.window"="$data.addNotification($event.detail.type || 'info', $event.detail.message, $event.detail.duration)"
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
}

/// A static footer.
pub(crate) fn render_html_footer() -> Markup {
    html! {
        script defer src="/assets/libs/mathjax-3.2.2/tex-mml-chtml.js" {}

        // Prevent flickering of images when they are already in the cache
        script {
            (PreEscaped(r#"
                document.addEventListener("DOMContentLoaded", () => {
                  const images = document.querySelectorAll('img[loading="lazy"]');
                  images.forEach(img => {
                    const testImg = new Image();
                    testImg.src = img.src;
                    if (testImg.complete) {
                      img.removeAttribute("loading");
                    }
                  });
                });

                // Catch unhandled promise rejections (network errors)
                window.addEventListener('unhandledrejection', (event) => {
                  console.log('Unhandled rejection:', event);
                  if (event.reason instanceof TypeError &&
                      (event.reason.message.includes('NetworkError') ||
                       event.reason.message.includes('fetch'))) {
                    window.dispatchEvent(new CustomEvent('notify', {
                      detail: { type: 'error', message: '⚠ Network Error - Unable to complete request' }
                    }));
                    event.preventDefault();
                  }
                });
            "#))
        }

        // Configure Prism.js autoloader and trigger highlighting
        script {
            (PreEscaped(r#"
                // Configure autoloader BEFORE it loads
                window.Prism = window.Prism || {};
                window.Prism.plugins = window.Prism.plugins || {};
                window.Prism.plugins.autoloader = window.Prism.plugins.autoloader || {};
                window.Prism.plugins.autoloader.languages_path = 'https://cdnjs.cloudflare.com/ajax/libs/prism/1.29.0/components/';

                document.addEventListener('DOMContentLoaded', () => {
                    // Manually trigger highlighting after everything is loaded
                    if (window.Prism) {
                        Prism.highlightAll();
                    }
                });
            "#))
        }

        // Alpine.js initialization
        script {
            (PreEscaped(r#"
                document.addEventListener("alpine:init", () => {
                  // WebSocket handler for real-time updates
                  Alpine.data("websocket", (url) => ({
                    ws: null,
                    init() {
                      this.connect(url);
                    },
                    connect(url) {
                      const protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
                      const wsUrl = `${protocol}//${window.location.host}${url}`;

                      this.ws = new WebSocket(wsUrl);

                      this.ws.onopen = () => {
                        console.log("WebSocket connected");
                      };

                      this.ws.onmessage = (event) => {
                        const parser = new DOMParser();
                        const doc = parser.parseFromString(event.data, "text/html");

                        // Process out-of-band swaps (x-swap-oob)
                        const oobElements = doc.querySelectorAll("[x-swap-oob]");
                        oobElements.forEach((element) => {
                          const oobValue = element.getAttribute("x-swap-oob");
                          const [swapType, selector] = oobValue.split(":");

                          if (selector) {
                            const target = document.querySelector(selector.trim());
                            if (target) {
                              if (swapType.includes("outerHTML")) {
                                target.outerHTML = element.outerHTML;
                              } else if (swapType.includes("innerHTML")) {
                                target.innerHTML = element.innerHTML;
                              } else if (swapType.includes("afterend")) {
                                target.insertAdjacentHTML("afterend", element.outerHTML);
                              } else if (swapType.includes("beforebegin")) {
                                target.insertAdjacentHTML("beforebegin", element.outerHTML);
                              }
                            }
                          }
                        });
                      };

                      this.ws.onerror = (error) => {
                        // Suppress error logging - it's expected if WebSocket isn't available
                      };

                      this.ws.onclose = () => {
                        // Don't reconnect - WebSocket is optional
                        // If we want reconnection, it should be with exponential backoff
                      };
                    },
                    destroy() {
                      if (this.ws) {
                        this.ws.close();
                      }
                    }
                  }));
                });
            "#))
        }
    }
}
