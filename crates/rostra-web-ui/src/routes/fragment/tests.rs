use super::button;

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
