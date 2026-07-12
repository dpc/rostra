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
            _ => bail!("unknown action `{command}`"),
        }
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
}
