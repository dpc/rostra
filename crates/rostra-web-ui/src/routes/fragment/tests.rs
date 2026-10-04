use maud::html;

use super::{button, timeline_tab_bar};

#[test]
fn shared_button_preserves_established_sizing_contract() {
    let stylesheet = include_str!("../../../assets/style.css");
    let shared_button_rule = stylesheet
        .split_once(".u-button {")
        .expect("shared button rule should exist")
        .1
        .split_once('}')
        .expect("shared button rule should be closed")
        .0;

    assert!(shared_button_rule.contains("min-width: 5rem;"));
    assert!(shared_button_rule.contains("max-width: 140px;"));
    assert!(shared_button_rule.contains("padding: 0.0rem .5rem;"));
    assert!(!shared_button_rule.contains("white-space"));

    let create_account_rule = stylesheet
        .split_once(".o-unlockScreen__generateButton {")
        .expect("create-account button rule should exist")
        .1
        .split_once('}')
        .expect("create-account button rule should be closed")
        .0;
    assert!(create_account_rule.contains("white-space: nowrap;"));
    assert!(create_account_rule.contains("max-width: unset;"));
}

#[test]
fn shared_button_supports_ordinary_form_actions_and_existing_icons() {
    let markup = button("m-example__submitButton", "Submit")
        .name("action")
        .value("cancel")
        .formaction("/post/link_dialog")
        .formnovalidate(true)
        .disabled_expression("loading")
        .icon_class("o-mediaList__closeButtonIcon")
        .call()
        .into_string();
    assert!(markup.contains("name=\"action\""));
    assert!(markup.contains("value=\"cancel\""));
    assert!(markup.contains("formaction=\"/post/link_dialog\""));
    assert!(markup.contains("formnovalidate"));
    assert!(markup.contains(":disabled=\"loading\""));
    assert!(markup.contains("class=\"o-mediaList__closeButtonIcon u-buttonIcon\""));
}

#[test]
fn javascript_requirement_preserves_button_icon_hook() {
    let markup = button("m-example__copyButton", "Copy")
        .button_type("button")
        .aria_label("Copy secret")
        .requires_js(true)
        .call()
        .into_string();
    let button_tag = markup
        .split_once('>')
        .expect("button should have an opening tag")
        .0;

    assert!(button_tag.contains("m-example__copyButton"));
    assert!(button_tag.contains("u-button"));
    assert!(button_tag.contains("u-requiresJs"));
    assert!(markup.contains("aria-label=\"Copy secret\""));
    assert!(markup.contains("class=\"m-example__copyButtonIcon u-buttonIcon\""));
}

#[test]
fn timeline_tab_bar_shares_framing_without_forcing_history_navigation() {
    let live = timeline_tab_bar(
        None,
        Some("badgeCounts({})"),
        true,
        html! { span { "Live" } },
    )
    .into_string();
    assert!(live.contains("class=\"o-mainBarTimeline__tabs\""));
    assert!(live.contains("x-data=\"badgeCounts({})\""));
    assert!(live.contains("@badges:updated.window=\"onUpdate($event.detail)\""));
    assert!(live.contains("onclick=\"history.back(); return false;\""));

    let private = timeline_tab_bar(
        Some("direct-message-tabs"),
        None,
        false,
        html! { span { "Private" } },
    )
    .into_string();
    assert!(private.contains("id=\"direct-message-tabs\""));
    assert!(private.contains("class=\"o-mainBarTimeline__tabs\""));
    assert!(!private.contains("x-data"));
    assert!(!private.contains("@badges:updated.window"));
    assert!(!private.contains("onclick"));
}
