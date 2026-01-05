use maud::{Markup, PreEscaped, html};

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
