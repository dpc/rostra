use maud::{html, Markup, DOCTYPE};

pub fn index() -> Markup {
    let content = html! {};

    page("PerfIt!", content)
}

pub fn page(title: &str, content: Markup) -> Markup {
    /// A basic header with a dynamic `page_title`.
    pub(crate) fn head(page_title: &str) -> Markup {
        html! {
            (DOCTYPE)
            html lang="en";
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1.0";
                link rel="stylesheet" type="text/css" href="/assets/style.css";
                link rel="stylesheet" type="text/css" href="/assets/style-htmx-send-error.css";
                title { (page_title) }
            }
        }
    }

    pub(crate) fn header() -> Markup {
        html! {
            header ."container py-5 flex flex-row place-content-center gap-6 items-center" {
                    div  { "PerfIt" }
            }
        }
    }

    /// A static footer.
    pub(crate) fn footer() -> Markup {
        html! {
            script src="https://unpkg.com/htmx.org@1.9.12" {};
            script src="https://unpkg.com/htmx.org@1.9.12/dist/ext/response-targets.js" {};
            script type="module" src="/assets/script.js" {};
            script type="module" src="/assets/script-htmx-send-error.js" {};
        }
    }

    html! {
        (head(title))
        body ."container relative mx-auto !block" style="display: none" {
            div #"gray-out-page" ."fixed inset-0 send-error-hidden"  {
                div ."relative z-50 bg-white mx-auto max-w-sm p-10 flex flex-center flex-col gap-2" {
                    p { "Connection error" }
                    button ."rounded bg-red-700 text-white px-2 py-1" hx-get="/" hx-target="body" hx-swap="outerHTML" { "Reload" }
                }
                div ."inset-0 absolute z-0 bg-gray-500 opacity-50" {}
            }
            (header())

            main ."container flex flex-col items-center justify-center" {
                (content)
            }
            (footer())
        }
    }
}
