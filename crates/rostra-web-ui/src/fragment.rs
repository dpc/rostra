use maud::{html, Markup, DOCTYPE};

use crate::error::RequestResult;
use crate::AppState;

impl AppState {
    pub async fn index(&self) -> RequestResult<Markup> {
        self.html_page("You're Rostra!").await
    }

    /// Html page header
    pub(crate) fn html_head(&self, page_title: &str) -> Markup {
        html! {
            (DOCTYPE)
            html lang="en";
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1.0";
                link rel="stylesheet" type="text/css" href="/assets/style.css";
                link rel="icon" type="image/x-icon" href="/assets/favicon.ico";
                // link rel="stylesheet" type="text/css" href="/assets/style-htmx-send-error.css";
                title { (page_title) }
            }
        }
    }

    pub async fn html_page(&self, title: &str) -> RequestResult<Markup> {
        Ok(html! {
            (self.html_head(title))
            body ."o-body" {
                // div #"gray-out-page" ."fixed inset-0 send-error-hidden"  {
                //     div ."relative z-50 bg-white mx-auto max-w-sm p-10 flex flex-center flex-col gap-2" {
                //         p { "Connection error" }
                //         button ."rounded bg-red-700 text-white px-2 py-1" hx-get="/" hx-target="body" hx-swap="outerHTML" { "Reload" }
                //     }
                //     div ."inset-0 absolute z-0 bg-gray-500 opacity-50" {}
                // }
                div ."o-pageLayout" {

                    // (header())
                    nav ."o-navBar" {

                        (self.self_account())

                        (self.new_post_form(None))

                        div ."o-navBar__list" {
                            a ."o-navBar__item" href="/" { "Home" }
                            a ."o-navBar__item" href="/" { "Something" }
                        }
                        (self.add_followee_form(None))
                    }

                    main ."o-mainBar" {

                        (self.main_bar_timeline().await?)
                        div ."o-mainBar__item" {
                            (post_overview("dpc", "Cool stuff"))
                        }

                        div ."o-mainBar__item" {
                            (post_overview("someone", "Some other cool stuff"))
                        }

                    }

                    div ."o-sideBar" {
                        "side bar"
                    }
                    (footer())
                }
            }
        })
    }
}

/// A static footer.
pub(crate) fn footer() -> Markup {
    html! {
        script
            src="https://unpkg.com/htmx.org@2.0.4"
            integrity="sha512-2kIcAizYXhIn8TzUvqzEDZNuDZ+aW7yE/+f1HJHXFjQcGNfv1kqzJSTBRBSlOgp6B/KZsz1K0a3ZTqP9dnxioQ==" crossorigin="anonymous"
            {};
        // script src="https://unpkg.com/htmx.org@1.9.12/dist/ext/response-targets.js" {};
        // script type="module" src="/assets/script.js" {};
        // script type="module" src="/assets/script-htmx-send-error.js" {};
    }
}

pub fn post_overview(username: &str, content: &str) -> Markup {
    html! {
        article ."m-postOverview" {
            div ."m-postOverview__main" {
                img ."m-postOverview__userImage"
                    src="https://avatars.githubusercontent.com/u/9209?v=4"
                    width="32pt"
                    height="32pt"
                    { }

                div ."m-postOverview__contentSide" {
                    header .".m-postOverview__header" {
                        span ."m-postOverview__username" { (username) }
                    }

                    div ."m-postOverview__content" {
                        p {
                            (content)
                        }

                        p {
                            "Molestias commodi voluptate qui nemo veniam quis. Commodi rem quis sapiente omnis dolorem nihil qui. Name totam quaerat qui blanditiis et et enim."
                        }

                        p {
                            "Molestias commodi voluptate qui nemo veniam quis. Commodi rem quis sapiente omnis dolorem nihil qui. Name totam quaerat qui blanditiis et et enim."
                        }

                    }
                }
            }

            div ."m-postOverview__buttonBar"{
                // "Buttons here"
            }
        }
    }
}
