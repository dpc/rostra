//! Preserve the experiment's decisive accounting witnesses, not just
//! acceptance.

use crate::decoder::Decoder;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

#[test]
fn real_consumption_is_distinct_from_synthetic_lookahead() {
    let bytes = fixture("gray-1x1-lookahead.jpg");
    let mut decoder = Decoder::new(bytes.as_slice());
    decoder.decode().unwrap();
    let c = decoder.completion();
    assert_eq!(
        (c.consumed_bits, c.real_bytes, c.scans, c.mcus, c.blocks),
        (2, 1, 1, 1, [1, 0, 0, 0])
    );
    assert!(c.synthetic_peeks > 0);
    assert!(crate::validate(&bytes).is_ok());
    assert!(crate::validate(&fixture("empty-scan.jpg")).is_err());
    // These encoder-known counts cover every canonical padding length.
    for length in 1..=8 {
        let padding = (8 - (1 + length) % 8) % 8;
        let bytes = fixture(&format!("padding-{padding}.jpg"));
        let mut decoder = Decoder::new(bytes.as_slice());
        decoder.decode().unwrap();
        assert_eq!(decoder.completion().consumed_bits, 1 + length);
    }
}

#[test]
fn strict_baseline_run_errors_are_decoder_errors_too() {
    for name in [
        "illegal-eob-run.jpg",
        "zrl-overrun.jpg",
        "fast-run-overrun.jpg",
        "slow-run-overrun.jpg",
    ] {
        let bytes = fixture(name);
        assert!(Decoder::new(bytes.as_slice()).decode().is_err(), "{name}");
    }
}
