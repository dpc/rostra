use rostra_core::id::RostraId;

use super::{ProfileSearchResult, order_and_limit_results};

fn id(n: u8) -> RostraId {
    let mut bytes = [0; 32];
    bytes[31] = n;
    RostraId::from_bytes(bytes)
}

fn tied_results(input: impl IntoIterator<Item = u8>) -> Vec<String> {
    order_and_limit_results(
        input
            .into_iter()
            .map(|n| {
                let id = id(n);
                (
                    7,
                    id,
                    ProfileSearchResult {
                        rostra_id: id.to_string(),
                        display_name: format!("name-{:02}", 12 - n),
                    },
                )
            })
            .collect(),
    )
    .into_iter()
    .map(|result| result.rostra_id)
    .collect()
}

#[test]
fn equal_score_membership_and_order_are_independent_of_input_order() {
    let expected = (0..10).map(|n| id(n).to_string()).collect::<Vec<_>>();
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
