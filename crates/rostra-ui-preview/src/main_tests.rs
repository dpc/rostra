use anyhow::anyhow;

use super::{Action, Browser, should_defer, should_skip_after_lookup_failure};

#[test]
fn stream_policy_defers_only_lookup_misses_and_skips_mutations() {
    let inspection = Action::InspectLabel("Missing".into());
    let click = Action::ClickLabel("Mutating action".into());
    let lookup = Browser::lookup_error_for_test();

    assert!(should_defer(&inspection, &lookup));
    assert!(!should_defer(&inspection, &anyhow!("CDP failed")));
    assert!(!should_defer(&click, &lookup));
    assert!(!should_skip_after_lookup_failure(1, &inspection));
    assert!(should_skip_after_lookup_failure(1, &click));
    assert!(!should_skip_after_lookup_failure(0, &click));
}
