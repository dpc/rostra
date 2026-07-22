use std::collections::HashSet;

use rostra_core::ShortEventId;

use super::{representative_head, sample_head_with, sorted_heads};

#[test]
fn head_selectors_have_explicit_empty_and_complete_semantics() {
    let empty = HashSet::new();
    assert_eq!(representative_head(&empty), None);
    assert_eq!(
        sample_head_with(&empty, |_| panic!("empty sets must not choose an index")),
        None
    );
    assert!(sorted_heads(&empty).is_empty());

    let heads: HashSet<_> = [
        ShortEventId::from_bytes([3; 16]),
        ShortEventId::from_bytes([1; 16]),
        ShortEventId::from_bytes([2; 16]),
    ]
    .into_iter()
    .collect();

    assert_eq!(
        representative_head(&heads),
        Some(ShortEventId::from_bytes([1; 16]))
    );
    assert_eq!(
        sorted_heads(&heads),
        vec![
            ShortEventId::from_bytes([1; 16]),
            ShortEventId::from_bytes([2; 16]),
            ShortEventId::from_bytes([3; 16]),
        ]
    );

    let samples: HashSet<_> = (0..heads.len())
        .map(|index| {
            sample_head_with(&heads, |len| {
                assert_eq!(len, heads.len());
                index
            })
            .expect("nonempty head set")
        })
        .collect();
    assert_eq!(samples, heads);
}

#[test]
fn singleton_sample_is_the_only_head() {
    let head = ShortEventId::from_bytes([7; 16]);
    let heads = HashSet::from([head]);

    assert_eq!(sample_head_with(&heads, |_| 0), Some(head));
}
