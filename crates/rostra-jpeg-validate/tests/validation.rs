use rostra_jpeg_validate::{Rejection, validate};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

#[test]
fn preserved_phase2_controls() {
    for line in include_str!("fixtures/manifest.txt").lines() {
        let (expected, name) = line.split_once(' ').unwrap();
        let result = validate(&fixture(name));
        let actual = match result {
            Ok(_) => "Valid",
            Err(Rejection::Invalid(_)) => "Invalid",
            Err(Rejection::Unsupported(_)) => "Unsupported",
            Err(Rejection::TooLarge) => "TooLarge",
        };
        assert_eq!(actual, expected, "{name}: {result:?}");
    }
}

#[test]
fn incomplete_scans_padding_and_extra_entropy_never_pass() {
    for name in [
        "gray-1x1-lookahead.jpg",
        "420-17x15.jpg",
        "repository-red.jpg",
    ] {
        let bytes = fixture(name);
        for end in 0..bytes.len() {
            assert!(validate(&bytes[..end]).is_err(), "{name} prefix {end}");
        }
        let sos = bytes.windows(2).position(|b| b == [255, 218]).unwrap();
        let start = sos + 2 + usize::from(u16::from_be_bytes([bytes[sos + 2], bytes[sos + 3]]));
        for end in start..bytes.len() - 2 {
            let mut cut = bytes[..end].to_vec();
            cut.extend([255, 217]);
            assert!(validate(&cut).is_err(), "{name} repaired cut {end}");
        }
        let mut extra = bytes[..bytes.len() - 2].to_vec();
        extra.extend([0, 255, 217]);
        assert!(validate(&extra).is_err(), "{name} extra entropy");
    }
    for padding in 1..=7 {
        let bytes = fixture(&format!("padding-{padding}.jpg"));
        let last = bytes.len() - 3;
        for bit in 0..padding {
            let mut flipped = bytes.clone();
            flipped[last] ^= 1 << bit;
            assert!(validate(&flipped).is_err(), "padding {padding} bit {bit}");
        }
    }
}

#[test]
fn size_and_header_budget_taxonomy() {
    assert_eq!(validate(&vec![0; 1_000_001]), Err(Rejection::TooLarge));
    let mut bytes = fixture("gray-1x1-lookahead.jpg");
    // One segment claims an end outside the permitted header span.
    bytes[4..6].copy_from_slice(&u16::MAX.to_be_bytes());
    assert!(matches!(validate(&bytes), Err(Rejection::Unsupported(_))));
    let mut bytes = fixture("gray-1x1-lookahead.jpg");
    let sof = bytes.windows(2).position(|b| b == [255, 192]).unwrap();
    bytes[sof + 5..sof + 7].fill(0);
    assert!(matches!(validate(&bytes), Err(Rejection::Invalid(_))));
}
