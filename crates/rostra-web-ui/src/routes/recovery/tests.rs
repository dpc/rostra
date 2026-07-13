use std::collections::BTreeMap;

use cssparser::{
    AtRuleParser, CowRcStr, DeclarationParser, ParseError, Parser, ParserInput,
    QualifiedRuleParser, RuleBodyItemParser, RuleBodyParser, StyleSheetParser,
};
use rostra_core::id::RostraIdSecretKey;
use scraper::{Html, Selector};

use super::settings_phrase;

type Declarations = BTreeMap<String, String>;

struct DeclarationCollector;

impl<'i> DeclarationParser<'i> for DeclarationCollector {
    type Declaration = (String, String);
    type Error = ();

    fn parse_value<'t>(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i, 't>,
    ) -> Result<Self::Declaration, ParseError<'i, Self::Error>> {
        let start = input.position();
        while input.next_including_whitespace_and_comments().is_ok() {}
        Ok((name.to_string(), input.slice_from(start).trim().to_owned()))
    }
}

impl<'i> AtRuleParser<'i> for DeclarationCollector {
    type Prelude = ();
    type AtRule = (String, String);
    type Error = ();
}

impl<'i> QualifiedRuleParser<'i> for DeclarationCollector {
    type Prelude = ();
    type QualifiedRule = (String, String);
    type Error = ();
}

impl<'i> RuleBodyItemParser<'i, (String, String), ()> for DeclarationCollector {
    fn parse_declarations(&self) -> bool {
        true
    }

    fn parse_qualified(&self) -> bool {
        false
    }
}

struct StylesheetCollector;

impl<'i> QualifiedRuleParser<'i> for StylesheetCollector {
    type Prelude = String;
    type QualifiedRule = (String, Declarations);
    type Error = ();

    fn parse_prelude<'t>(
        &mut self,
        input: &mut Parser<'i, 't>,
    ) -> Result<Self::Prelude, ParseError<'i, Self::Error>> {
        let start = input.position();
        while input.next_including_whitespace_and_comments().is_ok() {}
        Ok(input.slice_from(start).trim().to_owned())
    }

    fn parse_block<'t>(
        &mut self,
        selector: Self::Prelude,
        _: &cssparser::ParserState,
        input: &mut Parser<'i, 't>,
    ) -> Result<Self::QualifiedRule, ParseError<'i, Self::Error>> {
        let declarations = RuleBodyParser::new(input, &mut DeclarationCollector)
            .filter_map(Result::ok)
            .collect();
        Ok((selector, declarations))
    }
}

impl<'i> AtRuleParser<'i> for StylesheetCollector {
    type Prelude = String;
    type AtRule = (String, Declarations);
    type Error = ();

    fn parse_prelude<'t>(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i, 't>,
    ) -> Result<Self::Prelude, ParseError<'i, Self::Error>> {
        while input.next_including_whitespace_and_comments().is_ok() {}
        Ok(format!("@{name}"))
    }

    fn rule_without_block(
        &mut self,
        prelude: Self::Prelude,
        _: &cssparser::ParserState,
    ) -> Result<Self::AtRule, ()> {
        Ok((prelude, Declarations::new()))
    }

    fn parse_block<'t>(
        &mut self,
        prelude: Self::Prelude,
        _: &cssparser::ParserState,
        input: &mut Parser<'i, 't>,
    ) -> Result<Self::AtRule, ParseError<'i, Self::Error>> {
        while input.next_including_whitespace_and_comments().is_ok() {}
        Ok((prelude, Declarations::new()))
    }
}

fn css_declarations(stylesheet: &str, selector: &str) -> Declarations {
    let mut input = ParserInput::new(stylesheet);
    let mut parser = Parser::new(&mut input);
    let mut merged = Declarations::new();
    let mut found = false;
    for rule in StyleSheetParser::new(&mut parser, &mut StylesheetCollector) {
        let (candidate, declarations) =
            rule.unwrap_or_else(|(error, source)| panic!("invalid CSS near {source:?}: {error:?}"));
        if candidate == selector {
            found = true;
            merged.extend(declarations);
        }
    }
    assert!(found, "missing CSS rule for {selector}");
    merged
}

#[test]
fn settings_credential_remains_a_selectable_html_fallback() {
    let secret = RostraIdSecretKey::generate();
    let phrase = secret.to_string();
    let document = Html::parse_fragment(&settings_phrase(secret).into_string());

    let field_selector = Selector::parse("input[type='password'][readonly]").unwrap();
    let fields = document.select(&field_selector).collect::<Vec<_>>();
    assert_eq!(fields.len(), 1, "recovery field must be unique");
    let field = fields[0].value();
    assert_eq!(field.attr("value"), Some(phrase.as_str()));
    assert!(
        field.attr("disabled").is_none(),
        "manual-copy fallback must remain keyboard-selectable"
    );

    let field_id = field
        .id()
        .expect("recovery field must be label-addressable");
    let label_selector = Selector::parse(&format!("label[for='{field_id}']")).unwrap();
    assert_eq!(
        document.select(&label_selector).count(),
        1,
        "the fallback field must retain its accessible label"
    );

    let copy_selector = Selector::parse("button[type='button'][onclick][aria-label]").unwrap();
    let copy_buttons = document.select(&copy_selector).collect::<Vec<_>>();
    assert_eq!(copy_buttons.len(), 1, "copy enhancement must be unique");
    let accessible_name = copy_buttons[0].value().attr("aria-label").unwrap();
    assert!(accessible_name.to_ascii_lowercase().contains("copy"));
    let visible_label = copy_buttons[0].text().collect::<String>();
    assert!(visible_label.to_ascii_lowercase().contains("copy"));
    assert!(
        visible_label.split_whitespace().count() <= 2,
        "copy enhancement should retain a concise visible label"
    );
    assert!(copy_buttons[0].value().attr("disabled").is_none());
}

#[test]
fn settings_credential_uses_scoped_compact_responsive_control_rules() {
    let stylesheet = include_str!("../../../assets/style.css");

    let row = css_declarations(stylesheet, ".m-recoveryPhrase__settingsControlRow");
    assert_eq!(row["display"], "flex");
    assert_eq!(row["align-items"], "stretch");
    assert_eq!(row["flex-wrap"], "wrap");
    assert_eq!(row["gap"], "0.5rem");

    let field = css_declarations(
        stylesheet,
        ".m-recoveryPhrase__settingsControl .m-recoveryPhrase__phrase",
    );
    assert_eq!(field["flex"], "1 1 20rem");
    assert_eq!(field["min-width"], "0");
    assert_eq!(field["height"], "1.75rem");
    assert_eq!(field["padding"], "0 0.5rem");

    let button = css_declarations(
        stylesheet,
        ".m-recoveryPhrase__settingsControl .m-recoveryPhrase__copyButton",
    );
    assert_eq!(button["min-height"], "1.75rem");

    let status = css_declarations(
        stylesheet,
        ".m-recoveryPhrase__settingsControl .m-recoveryPhrase__status",
    );
    assert_eq!(status["margin"], "0");
}
