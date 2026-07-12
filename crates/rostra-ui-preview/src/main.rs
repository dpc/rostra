//! A deliberately small Chrome DevTools Protocol client for live UI inspection.
//!
//! See the crate's `SECURITY.md` before changing its transport, browser
//! lifecycle, origin policy, or supported actions.

mod browser;
mod cdp;
mod command;
mod endpoint;
mod secret_file;

use std::io::{self, BufRead};
use std::path::PathBuf;

use anyhow::{Context, Result};
use browser::Browser;
use clap::Parser;
use command::Action;
use endpoint::SiteOrigin;

/// Inspect a live Rostra development site using an isolated Chromium profile.
#[derive(Debug, Parser)]
#[command(version)]
struct Args {
    /// Rostra origin to inspect; only an IPv4 or IPv6 loopback origin is
    /// accepted.
    #[arg(long, default_value = "http://[::1]:2345")]
    origin: String,

    /// Initial path at the Rostra origin.
    #[arg(long, default_value = "/")]
    path: String,

    /// Viewport width in CSS pixels.
    #[arg(long, default_value_t = 1280)]
    width: u32,

    /// Viewport height in CSS pixels.
    #[arg(long, default_value_t = 900)]
    height: u32,

    /// Show Chromium instead of running it headlessly.
    #[arg(long)]
    headed: bool,

    /// Permit secret-file input actions for an explicitly approved dev login.
    #[arg(long)]
    allow_secret_input: bool,

    /// Browser action, repeated in execution order. If absent, read actions
    /// from stdin.
    ///
    /// Supported values include: "open PATH", "click-label LABEL", "click-id
    /// ID", "inspect-label LABEL", "inspect-id ID", "scroll up", "scroll
    /// down", "ready", "screenshot PATH", and "unlock-from-dev-secret PATH".
    #[arg(long = "action", value_name = "COMMAND")]
    actions: Vec<Action>,

    /// Chromium executable. ROSTRA_CHROMIUM overrides the default.
    #[arg(long, env = "ROSTRA_CHROMIUM", default_value = "chromium", hide = true)]
    chromium: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let origin = SiteOrigin::parse(&args.origin)?;
    origin.probe().with_context(|| {
        format!("Rostra is not ready at {origin}; run `just dev-no-open` first")
    })?;

    let mut browser = Browser::launch(
        &args.chromium,
        &origin,
        args.width,
        args.height,
        args.headed,
    )?;
    browser.open(&args.path)?;

    if args.actions.is_empty() {
        for line in io::stdin().lock().lines() {
            let line = line?;
            if line.trim().is_empty() || line.trim_start().starts_with('#') {
                continue;
            }
            execute(&mut browser, &line.parse()?, args.allow_secret_input)?;
        }
    } else {
        for action in &args.actions {
            execute(&mut browser, action, args.allow_secret_input)?;
        }
    }

    Ok(())
}

/// Execute one parsed action against the active browser page.
fn execute(browser: &mut Browser, action: &Action, allow_secret_input: bool) -> Result<()> {
    match action {
        Action::Open(target) => browser.open(target),
        Action::ClickLabel(label) => browser.click_label(label),
        Action::ClickId(id) => browser.click_id(id),
        Action::Scroll(direction) => browser.scroll(*direction),
        Action::Ready => browser.verify_ready(),
        Action::Screenshot(path) => {
            browser.screenshot(path)?;
            println!("{}", path.display());
            Ok(())
        }
        Action::InspectLabel(label) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&browser.inspect_label(label)?)?
            );
            Ok(())
        }
        Action::InspectId(id) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&browser.inspect_id(id)?)?
            );
            Ok(())
        }
        Action::UnlockFromDevSecret { path } => {
            if !allow_secret_input {
                anyhow::bail!("secret-file input requires --allow-secret-input");
            }
            let secret = secret_file::read_dev_secret(path, browser.origin_port())?;
            browser.fill_rostra_unlock_password(&secret)
        }
    }
}
