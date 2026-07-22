use std::collections::HashSet;

use rand::Rng as _;
use rostra_core::ShortEventId;

/// Select the deterministic representative of a head set.
///
/// The representative is only a stable local default. It does not imply that
/// the selected event is newer, preferred, or the only current head.
pub(crate) fn representative_head(heads: &HashSet<ShortEventId>) -> Option<ShortEventId> {
    heads.iter().copied().min()
}

/// Uniformly sample one member of a head set.
///
/// Each call makes an independent choice. This is appropriate for repeated
/// one-head discovery, but not for callers that require the complete set.
pub(crate) fn sample_head(heads: &HashSet<ShortEventId>) -> Option<ShortEventId> {
    sample_head_with(heads, |len| rand::rng().random_range(0..len))
}

fn sample_head_with(
    heads: &HashSet<ShortEventId>,
    choose_index: impl FnOnce(usize) -> usize,
) -> Option<ShortEventId> {
    if heads.is_empty() {
        return None;
    }

    heads.iter().nth(choose_index(heads.len())).copied()
}

/// Return the complete head set in deterministic order.
pub(crate) fn sorted_heads(heads: &HashSet<ShortEventId>) -> Vec<ShortEventId> {
    let mut heads: Vec<_> = heads.iter().copied().collect();
    heads.sort_unstable();
    heads
}

#[cfg(test)]
mod tests;
