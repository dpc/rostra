use maud::{Markup, PreEscaped, html};

/// Re-render math equations and code highlighting after dynamic content load.
///
/// Necessary after new content is added to the page via AJAX
/// (e.g. timeline pagination, post preview, shoutbox messages).
pub(crate) fn re_typeset() -> Markup {
    html! {
        script ."mathjax" {
            (PreEscaped(r#"
                if (typeof MathJax !== 'undefined') {
                    MathJax.typesetPromise();
                }
                if (typeof Prism !== 'undefined') {
                    Prism.highlightAll();
                }
            "#))
        }
    }
}
