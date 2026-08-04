use rostra_core::id::{RostraId, ToShort as _};

use super::{ProfileSearchIdReference, ProfileSearchResult, order_and_limit_results};

fn id(n: u8) -> RostraId {
    let mut bytes = [0; 32];
    bytes[31] = n;
    RostraId::from_bytes(bytes)
}

fn tied_results(input: impl IntoIterator<Item = u8>) -> Vec<ProfileSearchIdReference> {
    order_and_limit_results(
        input
            .into_iter()
            .map(|n| {
                let id = id(n);
                (
                    7,
                    id,
                    ProfileSearchResult {
                        rostra_id_reference: ProfileSearchIdReference::Short(id.to_short()),
                        display_name: format!("name-{:02}", 12 - n),
                    },
                )
            })
            .collect(),
    )
    .into_iter()
    .map(|result| result.rostra_id_reference)
    .collect()
}

#[test]
fn equal_score_membership_and_order_are_independent_of_input_order() {
    let expected = (0..10)
        .map(|n| ProfileSearchIdReference::Short(id(n).to_short()))
        .collect::<Vec<_>>();
    assert_eq!(tied_results(0..12), expected);

    let permutations = [
        (0..12).rev().collect::<Vec<_>>(),
        vec![7, 1, 11, 3, 9, 0, 5, 10, 2, 8, 4, 6],
        vec![4, 10, 2, 8, 0, 6, 11, 1, 9, 3, 7, 5],
    ];

    for permutation in permutations {
        assert_eq!(tied_results(permutation), expected);
    }
}

#[test]
fn autocomplete_ids_use_short_form_only_for_retained_identities() {
    let retained_id = id(42);
    let unretained_id = id(43);

    assert_eq!(
        ProfileSearchIdReference::for_known_identity(retained_id, Some(retained_id)),
        ProfileSearchIdReference::Short(retained_id.to_short())
    );
    assert_eq!(
        ProfileSearchIdReference::for_known_identity(unretained_id, None),
        ProfileSearchIdReference::Full(unretained_id)
    );
}
