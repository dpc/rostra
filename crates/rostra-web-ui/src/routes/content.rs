use jotdown::Attributes;
use maud::{Markup, PreEscaped};

use crate::UiState;

impl UiState {
    pub(crate) fn render_content(&self, content: &str) -> Markup {
        PreEscaped(jotdown::html::render_to_string(
            jotdown::Parser::new(content).map(|e| match e {
                jotdown::Event::Start(jotdown::Container::RawBlock { format }, _attrs)
                    if format == "html" =>
                {
                    jotdown::Event::Start(
                        jotdown::Container::CodeBlock { language: format },
                        Attributes::new(),
                    )
                }
                jotdown::Event::End(jotdown::Container::RawBlock { format })
                    if format == "html" =>
                {
                    jotdown::Event::End(jotdown::Container::CodeBlock { language: format })
                }
                jotdown::Event::Start(jotdown::Container::RawInline { format }, _attr)
                    if format == "html" =>
                {
                    jotdown::Event::Start(
                        jotdown::Container::CodeBlock { language: format },
                        Attributes::new(),
                    )
                }
                jotdown::Event::End(jotdown::Container::RawInline { format })
                    if format == "html" =>
                {
                    jotdown::Event::End(jotdown::Container::CodeBlock { language: format })
                }
                jotdown::Event::Start(container, _attr) => {
                    jotdown::Event::Start(container, Attributes::new())
                }
                e => e,
            }),
        ))
    }
}
