use std::ops::Bound;

use bincode::Decode;
use rostra_core::Timestamp;
use rostra_core::event::PersonasTagsSelector;
use rostra_core::id::RostraId;

use crate::{Database, DbResult, IdsFolloweesRecord, ids_followees};

/// One active direct follow in a coherent current self-follow snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelfFollowee {
    /// Identity currently followed by the database owner.
    pub followee: RostraId,
    /// Effective tag-based persona selector of the winning follow.
    pub persona_selector: PersonasTagsSelector,
    /// Start of the current uninterrupted follow epoch.
    pub first_ts: Timestamp,
}

impl Database {
    /// Gets all active direct self-follows from one current-state read
    /// snapshot.
    ///
    /// An unfollow removes the relationship from this snapshot. A later
    /// refollow creates a new uninterrupted epoch whose `first_ts` excludes
    /// follows at or before the winning unfollow boundary. Additional
    /// follows in the current epoch can move `first_ts` earlier, while the
    /// effective persona selector continues to come from the winning follow
    /// event.
    ///
    /// Concurrent commits fall wholly before or after the read snapshot; the
    /// returned records never combine relationships from different database
    /// states. The order of returned records is unspecified.
    ///
    /// # Errors
    ///
    /// Returns an error on storage or record decode failure.
    pub async fn get_self_followees_snapshot(&self) -> DbResult<Vec<SelfFollowee>> {
        self.read_with(|tx| {
            let followees = tx.open_table(&ids_followees::TABLE)?;
            let prefix = bincode::encode_to_vec(self.self_id, redb_bincode::BINCODE_CONFIG)
                .expect("encoding a followee prefix cannot fail");
            let prefix_end = lexicographic_successor(&prefix);
            followees
                .as_raw()
                .range::<&[u8]>((
                    Bound::Included(prefix.as_slice()),
                    prefix_end
                        .as_deref()
                        .map_or(Bound::Unbounded, Bound::Excluded),
                ))?
                .map(|entry| {
                    let (key, value) = entry?;
                    let (_, followee): (RostraId, RostraId) = decode_exact(key.value())?;
                    let record: IdsFolloweesRecord = decode_exact(value.value())?;
                    Ok(SelfFollowee {
                        followee,
                        persona_selector: record.effective_tags_selector(),
                        first_ts: record.first_ts,
                    })
                })
                .collect()
        })
        .await
    }
}

fn decode_exact<T: Decode<()>>(bytes: &[u8]) -> DbResult<T> {
    let (value, consumed) =
        bincode::decode_from_slice::<T, _>(bytes, redb_bincode::BINCODE_CONFIG)?;
    if consumed != bytes.len() {
        return Err(
            bincode::error::DecodeError::Other("Trailing bytes after encoded value").into(),
        );
    }
    Ok(value)
}

fn lexicographic_successor(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut successor = bytes.to_vec();
    let index = successor.iter().rposition(|byte| *byte != u8::MAX)?;
    successor[index] += 1;
    successor.truncate(index + 1);
    Some(successor)
}
