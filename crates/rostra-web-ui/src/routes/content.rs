use std::borrow::Cow;
use std::str::FromStr as _;

use jotdown::{Attributes, Container, Event};
use maud::{Markup, PreEscaped};
use rostra_client::ClientRef;
use rostra_core::ShortEventId;
use rostra_core::id::RostraId;
use url::Url;

use crate::UiState;

impl UiState {
    pub(crate) async fn render_content(
        &self,
        _client: &ClientRef<'_>,
        author_id: RostraId,
        content: &str,
    ) -> Markup {
        let sanitized = jotdown::Parser::new(content).map(|e| match e {
            Event::Start(Container::RawBlock { format }, _attrs) if format == "html" => {
                Event::Start(Container::CodeBlock { language: format }, Attributes::new())
            }
            Event::End(Container::RawBlock { format }) if format == "html" => {
                Event::End(Container::CodeBlock { language: format })
            }
            Event::Start(Container::RawInline { format }, _attr) if format == "html" => {
                Event::Start(Container::CodeBlock { language: format }, Attributes::new())
            }
            Event::End(Container::RawInline { format }) if format == "html" => {
                Event::End(Container::CodeBlock { language: format })
            }
            Event::Start(container, _attr) => Event::Start(container, Attributes::new()),
            e => e,
        });

        let mut in_profile_link = vec![];
        let mut in_media_link = vec![];
        let mut in_img_to_raw_html = vec![];
        let mut in_img_to_img = vec![];
        let out = jotdown::html::render_to_string(sanitized.flat_map(|event| {
            match event {
                Event::Start(Container::Link(s, jotdown::LinkType::AutoLink), attr) => {
                    if let Some(rostra_id) = Self::extra_rostra_id_link(&s) {
                        // TODO: blocked on https://github.com/hellux/jotdown/issues/86
                        // let x = client
                        //     .db()
                        //     .get_social_profile(rostra_id)
                        //     .await
                        //     .map(|record| record.display_name)
                        //     .unwrap_or_else(|| rostra_id.to_string());
                        in_profile_link.push(rostra_id);
                        vec![Event::Start(
                            Container::Link(
                                format!("/ui/profile/{rostra_id}").into(),
                                jotdown::LinkType::Span(jotdown::SpanLinkType::Inline),
                            ),
                            attr,
                        )]
                    } else {
                        vec![Event::Start(
                            Container::Link(s, jotdown::LinkType::AutoLink),
                            attr,
                        )]
                    }
                }
                Event::End(Container::Link(s, jotdown::LinkType::AutoLink)) => {
                    if let Some(rostra_id) = Self::extra_rostra_id_link(&s) {
                        in_profile_link.pop();
                        vec![Event::End(Container::Link(
                            format!("/ui/profile/{rostra_id}").into(),
                            jotdown::LinkType::Span(jotdown::SpanLinkType::Inline),
                        ))]
                    } else {
                        vec![Event::End(Container::Link(s, jotdown::LinkType::AutoLink))]
                    }
                }
                Event::Start(Container::Image(s, _link_type), attr) => {
                    if let Some(event_id) = Self::extra_rostra_media_link(&s) {
                        // TODO: blocked on https://github.com/hellux/jotdown/issues/86
                        // let x = client
                        //     .db()
                        //     .get_social_profile(rostra_id)
                        //     .await
                        //     .map(|record| record.display_name)
                        //     .unwrap_or_else(|| rostra_id.to_string());
                        in_media_link.push(event_id);
                        vec![Event::Start(
                            Container::Image(
                                format!("/ui/media/{author_id}/{event_id}").into(),
                                jotdown::SpanLinkType::Inline,
                            ),
                            attr,
                        )]
                    } else {
                        if let Some(html) = maybe_embed_media_html(&s) {
                            in_img_to_raw_html.push((html, String::new()));
                        } else {
                            in_img_to_img.push(String::new());
                        }

                        vec![Event::Start(
                            Container::Div {
                                class: "lazyload-wrapper",
                            },
                            jotdown::Attributes::try_from(
                                "{ onclick=\"this.classList.add('-expanded')\" }",
                            )
                            .expect("Can't fail"),
                        )]
                    }
                }
                Event::End(Container::Image(s, link_type)) => {
                    if let Some(event_id) = Self::extra_rostra_media_link(&s) {
                        in_media_link.pop();
                        vec![Event::End(Container::Image(
                            format!("/ui/media/{author_id}/{event_id}").into(),
                            jotdown::SpanLinkType::Inline,
                        ))]
                    } else {
                        [
                            if let Some((html, alt)) = in_img_to_raw_html.pop() {
                                let alt = alt.trim();
                                let load_msg = if alt.is_empty() {
                                    format!("Load: {s}").into()
                                } else {
                                    format!("Load “{alt}”: {s}").into()
                                };
                                vec![
                                    Event::Start(Container::Paragraph, Attributes::new()),
                                    Event::Str(load_msg),
                                    Event::End(Container::Paragraph),
                                    Event::Start(
                                        Container::RawInline { format: "html" },
                                        Attributes::try_from("{ loading=lazy }")
                                            .expect("Can't fail"),
                                    ),
                                    Event::Str(html.into()),
                                    Event::End(Container::RawInline { format: "html" }),
                                ]
                            } else if let Some(alt) = in_img_to_img.pop() {
                                let alt = alt.trim();
                                let load_msg = if alt.is_empty() {
                                    format!("Load: {s}").into()
                                } else {
                                    format!("Load “{alt}”: {s}").into()
                                };
                                vec![
                                    Event::Start(Container::Paragraph, Attributes::new()),
                                    Event::Str(load_msg),
                                    Event::End(Container::Paragraph),
                                    Event::Start(
                                        Container::Image(s.clone(), link_type),
                                        Attributes::try_from("{ loading=lazy }")
                                            .expect("Can't fail"),
                                    ),
                                    Event::Str(alt.to_string().into()),
                                    Event::End(Container::Image(s, link_type)),
                                ]
                            } else {
                                panic!("Can't be here")
                            },
                            vec![Event::End(Container::Div {
                                class: "img-wrapper",
                            })],
                        ]
                        .concat()
                    }
                }
                Event::Str(s) => {
                    if !in_profile_link.is_empty() {
                        let profile = in_profile_link.last().expect("Not empty just checked");
                        vec![Event::Str(format!("@{profile}").into())]
                    } else if let Some(last) = in_img_to_raw_html.last_mut() {
                        last.1 = s.to_string();
                        // skip the img alt tag
                        vec![]
                    } else if let Some(last) = in_img_to_img.last_mut() {
                        *last = s.to_string();
                        // skip the img alt tag
                        vec![]
                    } else {
                        vec![Event::Str(s)]
                    }
                }
                event => vec![event],
            }
        }));

        PreEscaped(out)
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

fn is_rostra_media_url(s: &str) -> bool {
    let Ok(url) = Url::parse(s) else {
        return false;
    };

    if url.scheme() == "rostra-media" {
        return true;
    }

    false
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
