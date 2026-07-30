//! A deliberately small Chrome DevTools Protocol client for live UI inspection.
//!
//! See the crate's `SECURITY.md` before changing its transport, browser
//! lifecycle, origin policy, or supported actions.

mod browser;
mod cdp;
mod command;
mod endpoint;
#[cfg(test)]
mod main_tests;
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
    /// ID", "fill-label LABEL [literal tab] TEXT", "fill-id ID [literal tab]
    /// TEXT",
    /// "inspect-label LABEL", "inspect-id ID", "hover-label LABEL",
    /// "hover-id ID", "unhover", "scroll up", "scroll down", "ready",
    /// "screenshot PATH", and "unlock-from-dev-secret PATH".
    #[arg(long = "action", value_name = "COMMAND")]
    actions: Vec<Action>,

    /// Chromium executable. ROSTRA_CHROMIUM overrides the default.
    #[arg(long, env = "ROSTRA_CHROMIUM", default_value = "chromium", hide = true)]
    chromium: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let origin = SiteOrigin::parse(&args.origin)?;
    origin.wait_until_ready().with_context(|| {
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

    let action_result = run_actions(&mut browser, &args);
    finalize_authenticated_preview(action_result, args.allow_secret_input, || {
        browser.cleanup_authenticated_preview()
    })
}

/// Combine an action result with mandatory authenticated-session cleanup.
fn finalize_authenticated_preview<T>(
    action_result: Result<T>,
    cleanup_required: bool,
    cleanup: impl FnOnce() -> Result<()>,
) -> Result<T> {
    let cleanup_result = if cleanup_required { cleanup() } else { Ok(()) };
    match (action_result, cleanup_result) {
        (Err(action), Err(cleanup)) => Err(anyhow::anyhow!(
            "preview action failed: {action:#}; authenticated cleanup also failed: {cleanup:#}"
        )),
        (Err(action), Ok(())) => Err(action),
        (Ok(_), Err(cleanup)) => Err(cleanup.context("authenticated preview cleanup failed")),
        (Ok(value), Ok(())) => Ok(value),
    }
}

/// Execute the requested action stream while aggregating exact lookup misses.
fn run_actions(browser: &mut Browser, args: &Args) -> Result<()> {
    let mut deferred_failures = 0;
    if args.actions.is_empty() {
        for line in io::stdin().lock().lines() {
            let line = line?;
            if line.trim().is_empty() || line.trim_start().starts_with('#') {
                continue;
            }
            let action: Action = line.parse()?;
            if should_skip_after_lookup_failure(deferred_failures, &action) {
                eprintln!("skipping non-inspection action after a deferred lookup failure");
                continue;
            }
            if let Err(error) = execute(browser, &action, args.allow_secret_input) {
                if should_defer(&action, &error) {
                    deferred_failures += 1;
                    eprintln!("inspection failed: {error:#}");
                    continue;
                }
                return Err(error);
            }
        }
    } else {
        for action in &args.actions {
            if should_skip_after_lookup_failure(deferred_failures, action) {
                eprintln!("skipping non-inspection action after a deferred lookup failure");
                continue;
            }
            if let Err(error) = execute(browser, action, args.allow_secret_input) {
                if should_defer(action, &error) {
                    deferred_failures += 1;
                    eprintln!("inspection failed: {error:#}");
                    continue;
                }
                return Err(error);
            }
        }
    }

    if deferred_failures != 0 {
        anyhow::bail!(
            "{deferred_failures} inspection action(s) failed; remaining inspections were completed"
        );
    }
    Ok(())
}

/// Return whether an exact inspection lookup miss may be reported at stream
/// end.
fn should_defer(action: &Action, error: &anyhow::Error) -> bool {
    action.is_inspection() && Browser::is_lookup_error(error)
}

/// Return whether safety policy skips this action after a deferred miss.
fn should_skip_after_lookup_failure(deferred_failures: usize, action: &Action) -> bool {
    deferred_failures != 0 && !action.is_inspection()
}

/// Execute one parsed action against the active browser page.
fn execute(browser: &mut Browser, action: &Action, allow_secret_input: bool) -> Result<()> {
    match action {
        Action::Open(target) => browser.open(target),
        Action::ClickLabel(label) => browser.click_label(label),
        Action::ClickId(id) => browser.click_id(id),
        Action::FillLabel { label, text } => browser.fill_label(label, text),
        Action::FillId { id, text } => browser.fill_id(id, text),
        Action::Scroll(direction) => browser.scroll(*direction),
        Action::Ready => browser.verify_ready(),
        Action::Screenshot(path) => {
            browser.screenshot(path)?;
            println!("{}", path.display());
            Ok(())
        }
        Action::HoverLabel(label) => browser.hover_label(label),
        Action::HoverId(id) => browser.hover_id(id),
        Action::Unhover => browser.unhover(),
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
