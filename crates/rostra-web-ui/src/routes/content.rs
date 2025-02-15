use std::str::FromStr as _;

use jotdown::{Attributes, Container, Event};
use maud::{Markup, PreEscaped};
use rostra_client::ClientRef;
use rostra_core::id::RostraId;

use crate::UiState;

impl UiState {
    pub(crate) async fn render_content(&self, _client: &ClientRef<'_>, content: &str) -> Markup {
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
        let out = jotdown::html::render_to_string(sanitized.map(|event| match event {
            Event::Start(Container::Link(s, jotdown::LinkType::AutoLink), attr) => {
                let ret = if let Some(rostra_id) = Self::extra_rostra_id_link(&s) {
                    // TODO: blocked on https://github.com/hellux/jotdown/issues/86
                    // let x = client
                    //     .db()
                    //     .get_social_profile(rostra_id)
                    //     .await
                    //     .map(|record| record.display_name)
                    //     .unwrap_or_else(|| rostra_id.to_string());
                    in_profile_link.push(rostra_id);
                    Event::Start(
                        Container::Link(
                            format!("/ui/profile/{rostra_id}").into(),
                            jotdown::LinkType::Span(jotdown::SpanLinkType::Inline),
                        ),
                        attr,
                    )
                } else {
                    Event::Start(Container::Link(s, jotdown::LinkType::AutoLink), attr)
                };
                ret
            }
            Event::End(Container::Link(s, jotdown::LinkType::AutoLink)) => {
                if let Some(rostra_id) = Self::extra_rostra_id_link(&s) {
                    in_profile_link.pop();
                    Event::End(Container::Link(
                        format!("/ui/profile/{rostra_id}").into(),
                        jotdown::LinkType::Span(jotdown::SpanLinkType::Inline),
                    ))
                } else {
                    Event::End(Container::Link(s, jotdown::LinkType::AutoLink))
                }
            }
            Event::Str(_s) if !in_profile_link.is_empty() => {
                let profile = in_profile_link.last().expect("Not empty just checked");
                Event::Str(format!("@{profile}").into())
            }
            event => event,
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
}

#[cfg(test)]
mod tests;
