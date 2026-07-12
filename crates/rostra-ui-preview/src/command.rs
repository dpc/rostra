use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{Result, bail};

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
        let (command, argument) = input
            .split_once(char::is_whitespace)
            .map_or((input, ""), |(command, argument)| {
                (command, argument.trim())
            });

        match (command, argument) {
            ("open", "") => bail!("open requires a path"),
            ("open", target) => Ok(Self::Open(target.to_owned())),
            ("click-label", "") => bail!("click-label requires an accessible label"),
            ("click-label", label) => Ok(Self::ClickLabel(label.to_owned())),
            ("click-id", "") => bail!("click-id requires an element ID"),
            ("click-id", id) => Ok(Self::ClickId(id.to_owned())),
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
            "click-label Reveal recovery phrase"
                .parse::<Action>()
                .unwrap(),
            Action::ClickLabel("Reveal recovery phrase".into())
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
    fn only_inspections_have_nonfatal_lookup_policy() {
        assert!(Action::InspectLabel("Missing".into()).is_inspection());
        assert!(Action::InspectId("missing".into()).is_inspection());
        assert!(!Action::HoverLabel("Missing".into()).is_inspection());
        assert!(!Action::ClickLabel("Missing".into()).is_inspection());
    }
}
