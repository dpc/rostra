//! Reusable HTML fragments for the web UI.

use maud::{Markup, html};

/// Renders a button with an icon.
///
/// The icon class is automatically derived from the button class by appending
/// "Icon". For example, if `class` is "m-postView__fetchButton", the icon class
/// will be "m-postView__fetchButtonIcon".
#[bon::builder]
pub fn button(
    /// Base CSS class for the button (e.g., "m-postView__fetchButton")
    #[builder(start_fn)]
    class: &str,
    /// Button label text
    #[builder(start_fn)]
    label: &str,
    /// Whether the button is disabled (uses disabled attribute)
    disabled: Option<bool>,
    /// Whether to use -disabled class instead of disabled attribute
    disabled_class: Option<bool>,
    /// Button type attribute (defaults to "submit")
    button_type: Option<&str>,
    /// Optional variant modifier (e.g., "--danger")
    variant: Option<&str>,
    /// Optional onclick handler for non-ajax buttons
    onclick: Option<&str>,
    /// Optional form ID for buttons that submit external forms
    form: Option<&str>,
    /// Optional data-value attribute
    data_value: Option<&str>,
    /// Optional title/tooltip
    title: Option<&str>,
) -> Markup {
    let disabled = disabled.unwrap_or(false);
    let disabled_class = disabled_class.unwrap_or(false);
    let button_type = button_type.unwrap_or("submit");
    let icon_class = format!("{class}Icon");

    let variant_class = variant.map(|v| format!("u-button{v}")).unwrap_or_default();

    html! {
        button
            .(class)
            ."u-button"
            .(variant_class)
            ."-disabled"[disabled_class]
            type=(button_type)
            disabled[disabled]
            onclick=[onclick]
            form=[form]
            data-value=[data_value]
            title=[title]
        {
            span .(icon_class) ."u-buttonIcon" {}
            (label)
        }
    }
}

/// JavaScript for ajax loading state with delay.
/// Returns the @ajax:before and @ajax:after attribute values.
fn ajax_loading_js(button_selector: &str) -> (String, String) {
    let before = format!(
        "clearTimeout($el._lt); $el._lt = setTimeout(() => {button_selector}?.classList.add('-loading'), 150)"
    );
    let after = format!("clearTimeout($el._lt); {button_selector}?.classList.remove('-loading')");
    (before, after)
}

/// Helper struct for ajax loading attributes on forms.
///
/// Use this when you have a complex form that can't use `ajax_form` directly
/// but still wants the consistent loading pattern.
pub struct AjaxLoadingAttrs {
    pub before: String,
    pub after: String,
}

impl AjaxLoadingAttrs {
    /// Create loading attributes for a button with the given CSS selector.
    ///
    /// Example: `AjaxLoadingAttrs::new("$el.querySelector('.my-button')")`
    pub fn new(button_selector: &str) -> Self {
        let (before, after) = ajax_loading_js(button_selector);
        Self { before, after }
    }

    /// Create loading attributes for a `.u-button` inside the form.
    pub fn for_button() -> Self {
        Self::new("$el.querySelector('.u-button')")
    }

    /// Create loading attributes for a button with a specific class inside the
    /// form.
    pub fn for_class(class: &str) -> Self {
        Self::new(&format!("$el.querySelector('.{class}')"))
    }

    /// Create loading attributes for a button found via document.querySelector.
    ///
    /// Use this when the button is outside the form element.
    pub fn for_document_class(class: &str) -> Self {
        Self::new(&format!("document.querySelector('.{class}')"))
    }
}

/// Renders a form with a button that shows a loading state during ajax
/// requests.
///
/// This is the primary abstraction for ajax-enabled action buttons.
#[bon::builder]
pub fn ajax_form(
    /// Form action URL
    #[builder(start_fn)]
    action: &str,
    /// HTTP method ("get" or "post")
    #[builder(start_fn)]
    method: &str,
    /// Alpine ajax x-target attribute
    #[builder(start_fn)]
    x_target: &str,
    /// The button to render inside the form
    #[builder(start_fn)]
    button: Markup,
    /// Custom CSS selector for the button (defaults to ".u-button")
    button_selector: Option<&str>,
    /// Extra JavaScript to run before the request (e.g., confirm dialog).
    /// If this returns early (via preventDefault), loading won't be triggered.
    before_js: Option<&str>,
    /// Extra JavaScript to run after the request completes (e.g., opening
    /// dialogs)
    after_js: Option<&str>,
    /// Hidden form inputs
    hidden_inputs: Option<Markup>,
    /// Additional form CSS class
    form_class: Option<&str>,
    /// Additional form styles
    form_style: Option<&str>,
    /// Whether to autofocus after ajax completes
    autofocus: Option<bool>,
) -> Markup {
    let selector = button_selector.unwrap_or("$el.querySelector('.u-button')");
    let (loading_before, loading_after) = ajax_loading_js(selector);

    // Combine before_js with loading logic
    let ajax_before = match before_js {
        Some(js) => format!(
            "{js} clearTimeout($el._lt); $el._lt = setTimeout(() => {selector}?.classList.add('-loading'), 150)"
        ),
        None => loading_before,
    };

    // Combine loading cleanup with after_js
    let ajax_after = match after_js {
        Some(js) => format!("{loading_after}; {js}"),
        None => loading_after,
    };

    html! {
        form
            action=(action)
            method=(method)
            x-target=(x_target)
            "@ajax:before"=(ajax_before)
            "@ajax:after"=(ajax_after)
            class=[form_class]
            style=[form_style]
            x-autofocus[autofocus.unwrap_or(false)]
        {
            @if let Some(inputs) = hidden_inputs {
                (inputs)
            }
            (button)
        }
    }
}

/// Renders an ajax form with an integrated button.
///
/// This is a convenience function that combines `ajax_form` and `button`.
#[bon::builder]
pub fn ajax_button(
    // Form parameters
    /// Form action URL
    #[builder(start_fn)]
    action: &str,
    /// HTTP method ("get" or "post")
    #[builder(start_fn)]
    method: &str,
    /// Alpine ajax x-target attribute
    #[builder(start_fn)]
    x_target: &str,
    // Button parameters
    /// Base CSS class for the button
    #[builder(start_fn)]
    button_class: &str,
    /// Button label text
    #[builder(start_fn)]
    label: &str,
    /// Whether the button is disabled
    disabled: Option<bool>,
    /// Optional variant modifier (e.g., "--danger")
    variant: Option<&str>,
    // Form parameters
    /// Extra JavaScript to run before the request
    before_js: Option<&str>,
    /// Extra JavaScript to run after the request
    after_js: Option<&str>,
    /// Hidden form inputs
    hidden_inputs: Option<Markup>,
    /// Additional form CSS class
    form_class: Option<&str>,
    /// Additional form styles
    form_style: Option<&str>,
    /// Whether to autofocus after ajax completes
    autofocus: Option<bool>,
) -> Markup {
    let btn = button(button_class, label)
        .maybe_disabled(disabled)
        .maybe_variant(variant)
        .call();

    ajax_form(action, method, x_target, btn)
        .maybe_before_js(before_js)
        .maybe_after_js(after_js)
        .maybe_hidden_inputs(hidden_inputs)
        .maybe_form_class(form_class)
        .maybe_form_style(form_style)
        .maybe_autofocus(autofocus)
        .call()
}

/// Generates a script that closes a dialog when Escape is pressed.
///
/// The handler is registered only once per dialog (using a window property).
/// The dialog element should use `-active` class to indicate it's open.
pub fn dialog_escape_handler(dialog_id: &str) -> Markup {
    let handler_name = format!("_escHandler_{}", dialog_id.replace('-', "_"));
    html! {
        script {
            (maud::PreEscaped(format!(r#"
                if (!window.{handler_name}) {{
                    window.{handler_name} = function(e) {{
                        if (e.key === 'Escape') {{
                            document.querySelector('#{dialog_id}')?.classList.remove('-active');
                        }}
                    }};
                    document.addEventListener('keydown', window.{handler_name});
                }}
            "#)))
        }
    }
}
