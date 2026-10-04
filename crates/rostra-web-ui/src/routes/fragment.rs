//! Reusable HTML fragments for the web UI.

#[cfg(test)]
mod tests;

use std::collections::BTreeSet;

use maud::{Markup, html};
use rostra_core::event::PersonaTag;

/// Render the shared timeline navigation frame around page-specific tabs.
pub(crate) fn timeline_tab_bar(
    id: Option<&str>,
    badge_counts: Option<&str>,
    history_back: bool,
    tabs: Markup,
) -> Markup {
    html! {
        div id=[id] ."o-mainBarTimeline__tabs"
            x-data=[badge_counts]
            "@badges:updated.window"=[badge_counts.map(|_| "onUpdate($event.detail)")]
        {
            a ."o-mainBarTimeline__back"
                href="/"
                onclick=[history_back.then_some("history.back(); return false;")]
                aria-label="Back"
                title="Back"
            {
                span ."o-mainBarTimeline__tabIcon -back" aria-hidden="true" {}
            }
            (tabs)
        }
    }
}

/// Render the shared timeline destinations with static or live unread badges.
///
/// Live callers supply the surrounding `badgeCounts` Alpine component. Private
/// pages can use ordinary server counts without opening an updates connection.
pub(crate) fn timeline_tab_links(
    active: &str,
    counts: super::timeline::PendingCounts,
    live: bool,
) -> Markup {
    html! {
        @for (key, path, label, count) in [
            ("followees", "/following", "Following", counts.followees),
            ("network", "/network", "Network", counts.network),
            ("news", "/news", "News", 0),
            ("notifications", "/notifications", "Notifications", counts.notifications),
            ("shoutbox", "/shoutbox", "Shoutbox", counts.shoutbox),
            ("messages", "/messages", "Messages", counts.messages),
        ] {
            a .(format!("o-mainBarTimeline__{key}"))
                ."-active"[active == key]
                ."-pending"[count > 0 && matches!(key, "notifications" | "shoutbox" | "messages")]
                href=(path)
                aria-current=[(active == key).then_some("page")]
                ":class"=[(live && matches!(key, "notifications" | "shoutbox" | "messages"))
                    .then(|| format!("{{ '-pending': {key} > 0 }}"))]
            {
                span ."o-mainBarTimeline__tabIcon" .(format!("-{key}")) aria-hidden="true" {}
                span ."o-mainBarTimeline__tabLabel" { (label) }
                @if key != "news" {
                    span .(if key == "notifications" {
                        "o-mainBarTimeline__pendingNotifications"
                    } else {
                        "o-mainBarTimeline__newCount"
                    })
                        x-text=[live.then(|| format!("formatCount({key})"))]
                    {
                        @if count > 0 {
                            (count.min(99))
                            @if count >= 99 { "+" }
                        }
                    }
                }
            }
        }
    }
}

/// Render the shared mention and emoji autocomplete result list.
pub(crate) fn text_autocomplete(id: &str, upward: bool) -> Markup {
    let option_id = format!("'{id}-option-' + index");
    html! {
        div id=(id) ."m-textAutocomplete" ."-upward"[upward]
            role="listbox"
            x-show="showDropdown"
            x-cloak
            "@click.outside"="showDropdown = false"
        {
            template x-if="autocompleteType === 'mention'" {
                div {
                    template x-for="(result, index) in results" ":key"="result.rostra_id_reference" {
                        div ."m-textAutocomplete__item"
                            role="option"
                            ":id"=(option_id)
                            ":aria-selected"="index === selectedIndex"
                            ":class"="{ '-selected': index === selectedIndex }"
                            "@click"="selectResult(result)"
                        {
                            span ."m-textAutocomplete__displayName" x-text="result.display_name" {}
                            span ."m-textAutocomplete__id" x-text="'@' + result.rostra_id_reference.substring(0, 8)" {}
                        }
                    }
                }
            }
            template x-if="autocompleteType === 'emoji'" {
                div {
                    template x-for="(result, index) in results" ":key"="index" {
                        div ."m-textAutocomplete__item"
                            role="option"
                            ":id"=(option_id)
                            ":aria-selected"="index === selectedIndex"
                            ":class"="{ '-selected': index === selectedIndex }"
                            "@click"="selectResult(result)"
                        {
                            span ."m-textAutocomplete__emoji" x-text="result.emoji" {}
                            span ."m-textAutocomplete__shortcode" x-text="':' + result.shortcode + ':'" {}
                        }
                    }
                }
            }
            div x-show="results.length === 0 && query.length > 0" ."m-textAutocomplete__empty" {
                "No matches found"
            }
        }
    }
}

/// Renders a user avatar image.
pub fn avatar(class: &str, src: impl maud::Render, alt: &str) -> Markup {
    html! {
        // width/height attributes prevent layout shift while avatars load
        img .(class) ."u-userImage"
            src=(src)
            alt=(alt)
            width="43"
            height="43"
        { }
    }
}

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
    /// Optional accessible name when the visible label does not name the action
    aria_label: Option<&str>,
    /// Hide the button when JavaScript is disabled.
    requires_js: Option<bool>,
    /// Form field name contributed by the submit button.
    name: Option<&str>,
    /// Form field value contributed by the submit button.
    value: Option<&str>,
    /// Override the owning form's submission destination.
    formaction: Option<&str>,
    /// Skip field validation for a cancellation or navigation action.
    formnovalidate: Option<bool>,
    /// Alpine expression that disables the button while a request is running.
    disabled_expression: Option<&str>,
    /// Use an existing icon class instead of deriving one from the button
    /// class.
    icon_class: Option<&str>,
) -> Markup {
    let disabled = disabled.unwrap_or(false);
    let disabled_class = disabled_class.unwrap_or(false);
    let requires_js = requires_js.unwrap_or(false);
    let button_type = button_type.unwrap_or("submit");
    let default_icon_class = format!("{class}Icon");
    let icon_class = icon_class.unwrap_or(&default_icon_class);

    let variant_class = variant.map(|v| format!("u-button{v}")).unwrap_or_default();

    html! {
        button
            .(class)
            ."u-button"
            ."u-requiresJs"[requires_js]
            .(variant_class)
            ."-disabled"[disabled_class]
            type=(button_type)
            disabled[disabled]
            onclick=[onclick]
            form=[form]
            data-value=[data_value]
            title=[title]
            aria-label=[aria_label]
            name=[name]
            value=[value]
            formaction=[formaction]
            formnovalidate[formnovalidate.unwrap_or(false)]
            ":disabled"=[disabled_expression]
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

/// Renders a persona tag multi-select combobox widget.
///
/// Without JS, the checkboxes are displayed as a plain visible list.
/// With JS, they are wrapped in a dropdown toggled by a button.
#[bon::builder]
pub fn persona_tag_select(
    /// HTML name attribute for the checkboxes (e.g., "persona_tags" or
    /// "personas")
    #[builder(start_fn)]
    name: &str,
    /// Available tags to show as options
    available_tags: &BTreeSet<PersonaTag>,
    /// Tags that should be pre-checked
    selected_tags: &BTreeSet<PersonaTag>,
    /// Unique HTML id prefix for this instance
    id: &str,
    /// Label to show when no tags are selected (defaults to "Select tags")
    empty_label: Option<&str>,
) -> Markup {
    let empty_label = empty_label.unwrap_or("Select tags");
    let checked_labels: Vec<&str> = available_tags
        .iter()
        .filter(|t| selected_tags.contains(t))
        .map(PersonaTag::as_str)
        .collect();
    let toggle_label = if checked_labels.is_empty() {
        empty_label.to_string()
    } else {
        checked_labels.join(", ")
    };

    html! {
        div ."m-personaTagSelect" data-id=(id) data-empty-label=(empty_label) {
            // Toggle button — hidden by default, shown when JS adds .-initialized
            div ."m-personaTagSelect__toggle"
                tabindex="0"
                onclick="personaTagSelectToggle(this)"
            {
                span ."m-personaTagSelect__toggleLabel" { (toggle_label) }
                span ."m-personaTagSelect__toggleArrow" { "\u{25be}" }
            }

            div ."m-personaTagSelect__dropdown" {
                div ."m-personaTagSelect__options" {
                    @for tag in available_tags {
                        label ."m-personaTagSelect__option" {
                            input
                                type="checkbox"
                                name=(name)
                                value=(tag.as_str())
                                checked[selected_tags.contains(tag)]
                                onchange="personaTagSelectChanged(this)"
                            {}
                            span { (tag.as_str()) }
                        }
                    }
                }

                input
                    ."m-personaTagSelect__addInput"
                    type="text"
                    placeholder="custom"
                    maxlength="32"
                {}
            }
        }
    }
}
