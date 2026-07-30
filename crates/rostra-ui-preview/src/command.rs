use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{Context, Result, bail};

/// Vertical direction used by the viewport-sized scroll action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScrollDirection {
    /// Scroll toward the top of the document.
    Up,
    /// Scroll toward the bottom of the document.
    Down,
}

/// One intentionally narrow browser action.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Action {
    /// Navigate to a path on the configured origin.
    Open(String),
    /// Activate the unique interactive element with this accessible label.
    ClickLabel(String),
    /// Activate the element with this HTML ID.
    ClickId(String),
    /// Replace an accessible editable element's text.
    FillLabel { label: String, text: String },
    /// Replace an editable element's text by its HTML ID.
    FillId { id: String, text: String },
    /// Scroll by three quarters of the viewport.
    Scroll(ScrollDirection),
    /// Wait for document and font readiness plus two animation frames.
    Ready,
    /// Capture the current viewport as a PNG.
    Screenshot(PathBuf),
    /// Move the real browser pointer onto an accessible label.
    HoverLabel(String),
    /// Move the real browser pointer onto an element ID.
    HoverId(String),
    /// Move the browser pointer away from the inspected page.
    Unhover,
    /// Print structured rendered evidence for an accessible label.
    InspectLabel(String),
    /// Print structured rendered evidence for an element ID.
    InspectId(String),
    /// Unlock Rostra using the protected secret for this development port.
    UnlockFromDevSecret {
        /// Protected file containing the value.
        path: PathBuf,
    },
}

impl FromStr for Action {
    type Err = anyhow::Error;

    fn from_str(input: &str) -> Result<Self> {
        let (command, raw_argument) = input.split_once(char::is_whitespace).unwrap_or((input, ""));
        let argument = if matches!(command, "fill-label" | "fill-id") {
            raw_argument
        } else {
            raw_argument.trim()
        };

        match (command, argument) {
            ("open", "") => bail!("open requires a path"),
            ("open", target) => Ok(Self::Open(target.to_owned())),
            ("click-label", "") => bail!("click-label requires an accessible label"),
            ("click-label", label) => Ok(Self::ClickLabel(label.to_owned())),
            ("click-id", "") => bail!("click-id requires an element ID"),
            ("click-id", id) => Ok(Self::ClickId(id.to_owned())),
            ("fill-label", argument) => {
                let (label, text) = parse_fill_arguments("fill-label", argument)?;
                Ok(Self::FillLabel { label, text })
            }
            ("fill-id", argument) => {
                let (id, text) = parse_fill_arguments("fill-id", argument)?;
                Ok(Self::FillId { id, text })
            }
            ("scroll", "up") => Ok(Self::Scroll(ScrollDirection::Up)),
            ("scroll", "down") => Ok(Self::Scroll(ScrollDirection::Down)),
            ("scroll", _) => bail!("scroll direction must be `up` or `down`"),
            ("ready", "") => Ok(Self::Ready),
            ("ready", _) => bail!("ready does not accept an argument"),
            ("screenshot", "") => bail!("screenshot requires an output path"),
            ("screenshot", path) => Ok(Self::Screenshot(path.into())),
            ("hover-label", "") => bail!("hover-label requires an accessible label"),
            ("hover-label", label) => Ok(Self::HoverLabel(label.to_owned())),
            ("hover-id", "") => bail!("hover-id requires an element ID"),
            ("hover-id", id) => Ok(Self::HoverId(id.to_owned())),
            ("unhover", "") => Ok(Self::Unhover),
            ("unhover", _) => bail!("unhover does not accept an argument"),
            ("inspect-label", "") => bail!("inspect-label requires an accessible label"),
            ("inspect-label", label) => Ok(Self::InspectLabel(label.to_owned())),
            ("inspect-id", "") => bail!("inspect-id requires an element ID"),
            ("inspect-id", id) => Ok(Self::InspectId(id.to_owned())),
            ("unlock-from-dev-secret", "") => {
                bail!("unlock-from-dev-secret requires the dev secret path")
            }
            ("unlock-from-dev-secret", path) => Ok(Self::UnlockFromDevSecret { path: path.into() }),
            _ => bail!("unknown action `{command}`"),
        }
    }
}

fn parse_fill_arguments(command: &str, argument: &str) -> Result<(String, String)> {
    let (target, text) = argument
        .split_once('\t')
        .with_context(|| format!("{command} requires TARGET, a tab, and TEXT"))?;
    if target.is_empty() {
        bail!("{command} requires a non-empty target");
    }
    Ok((target.to_owned(), text.to_owned()))
}

impl Action {
    /// Return whether a runtime lookup failure may defer until later
    /// inspections run.
    pub fn is_inspection(&self) -> bool {
        matches!(self, Self::InspectLabel(_) | Self::InspectId(_))
    }
}

#[cfg(test)]
mod tests {
    use super::{Action, ScrollDirection};

    #[test]
    fn parses_labels_with_spaces() {
        assert_eq!(
            "click-label Copy recovery phrase"
                .parse::<Action>()
                .unwrap(),
            Action::ClickLabel("Copy recovery phrase".into())
        );
    }

    #[test]
    fn rejects_ambiguous_scroll() {
        assert!("scroll sideways".parse::<Action>().is_err());
        assert_eq!(
            "scroll down".parse::<Action>().unwrap(),
            Action::Scroll(ScrollDirection::Down)
        );
    }

    #[test]
    fn parses_secret_file_without_reading_it() {
        assert_eq!(
            "unlock-from-dev-secret dev/2345/secret"
                .parse::<Action>()
                .unwrap(),
            Action::UnlockFromDevSecret {
                path: "dev/2345/secret".into(),
            }
        );
    }

    #[test]
    fn parses_fill_text_without_losing_spaces() {
        assert_eq!(
            "fill-label Post content\tHello, Rostra!"
                .parse::<Action>()
                .unwrap(),
            Action::FillLabel {
                label: "Post content".into(),
                text: "Hello, Rostra!".into(),
            }
        );
        assert_eq!(
            "fill-id field\ttrailing  ".parse::<Action>().unwrap(),
            Action::FillId {
                id: "field".into(),
                text: "trailing  ".into(),
            }
        );
        assert_eq!(
            "fill-id field\t".parse::<Action>().unwrap(),
            Action::FillId {
                id: "field".into(),
                text: String::new(),
            }
        );
        assert_eq!(
            "fill-id field\t  padded  ".parse::<Action>().unwrap(),
            Action::FillId {
                id: "field".into(),
                text: "  padded  ".into(),
            }
        );
        assert!("fill-id \ttext".parse::<Action>().is_err());
        assert!("fill-label Post content Hello".parse::<Action>().is_err());
    }

    #[test]
    fn only_inspections_have_nonfatal_lookup_policy() {
        assert!(Action::InspectLabel("Missing".into()).is_inspection());
        assert!(Action::InspectId("missing".into()).is_inspection());
        assert!(!Action::HoverLabel("Missing".into()).is_inspection());
        assert!(!Action::ClickLabel("Missing".into()).is_inspection());
    }
}
