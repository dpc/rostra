use maud::{html, Markup, PreEscaped};

/// Rendering this will re-render math equations using MathJax
///
/// Necessary after new content with (possibly) math was added after
/// page load.
pub(crate) fn re_typeset_mathjax() -> Markup {
    html! {
        script ."mathjax" {
            (PreEscaped(r#"
                if (typeof MathJax !== 'undefined') {
                    MathJax.typesetPromise();
                }
            "#))
        }
    }
}

/// Add ctrl+enter submit behavior to an input in a form
pub(crate) fn submit_on_ctrl_enter(form_selector: &str, input_selector: &str) -> Markup {
    html! {
        script {
            (PreEscaped(format!(r#"
                (function() {{
                    const form = document.querySelector('{}');
                    const input = document.querySelector('{}');

                    input.addEventListener('keydown', (e) => {{
                        if (e.ctrlKey && e.key === 'Enter') {{
                            htmx.trigger(form, 'submit');
                        }}
                    }});
                }}())
            "#, form_selector, input_selector)))
        }
    }
}
