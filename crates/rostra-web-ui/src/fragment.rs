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
                link rel="icon" type="image/png" href="/assets/favicon.png";
                // link rel="stylesheet" type="text/css" href="/assets/style-htmx-send-error.css";
                title { (page_title) }
                // Load htmx right away so it's immediately available, use defer to make it
                // non-blocking
                script defer src="/assets/libs/htmx.org@2.0.4.js" {}
                script defer src="/assets/libs/htmx-ext-ws@2.0.1.ws.js" {}
            }
        }
    }

    pub async fn render_html_page(&self, title: &str, content: Markup) -> RequestResult<Markup> {
        Ok(html! {
            (self.render_html_head(title))
            body ."o-body" {
                // div #"gray-out-page" ."fixed inset-0 send-error-hidden"  {
                //     div ."relative z-50 bg-white mx-auto max-w-sm p-10 flex flex-center flex-col gap-2" {
                //         p { "Connection error" }
                //         button ."rounded bg-red-700 text-white px-2 py-1" hx-get="/" hx-target="body" hx-swap="outerHTML" { "Reload" }
                //     }
                //     div ."inset-0 absolute z-0 bg-gray-500 opacity-50" {}
                // }
                div ."o-pageLayout" { (content) }
                (render_html_footer())
            }
        })
    }
}

/// A static footer.
pub(crate) fn render_html_footer() -> Markup {
    html! {

        // script id="MathJax-script" async src="https://cdn.jsdelivr.net/npm/mathjax@3/es5/tex-mml-chtml.js" {}
        script defer src="/assets/libs/mathjax-3.2.2/tex-mml-chtml.js" {}

        // script type="module" src="/assets/script.js" {};
        // script type="module" src="/assets/script-htmx-send-error.js" {};

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
            "#))
        }
    }
}
