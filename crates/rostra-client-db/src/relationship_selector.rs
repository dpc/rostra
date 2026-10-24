//! Allocation-free shape preflight before the ordinary authoritative decoder.

use bincode::Decode;
use rostra_core::{ShortEventId, Timestamp};

use crate::relationship::{
    RELATIONSHIP_RECORD_BYTES_MAX, RELATIONSHIP_SELECTOR_ELEMENTS_MAX, RelationshipIssue,
    RelationshipSelector,
};
use crate::{DbError, DbResult, IdsFolloweesRecord};

pub(crate) fn take<T: Decode<()>>(bytes: &mut &[u8]) -> DbResult<T> {
    let (value, consumed) = bincode::decode_from_slice(bytes, redb_bincode::BINCODE_CONFIG)?;
    *bytes = &bytes[consumed..];
    Ok(value)
}

pub(crate) fn decode_exact<T: Decode<()>>(mut bytes: &[u8]) -> DbResult<T> {
    let value = take(&mut bytes)?;
    if !bytes.is_empty() {
        return Err(DbError::RelationshipInvariant);
    }
    Ok(value)
}

fn option_present(bytes: &mut &[u8]) -> DbResult<bool> {
    match take::<u8>(bytes)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(DbError::RelationshipInvariant),
    }
}

/// Returns the charged bytes alongside the bounded selector.
pub(crate) fn selector(raw: &[u8]) -> DbResult<(RelationshipSelector, usize)> {
    let oversized = RelationshipSelector::Unavailable(RelationshipIssue::Oversized);
    if raw.len() > RELATIONSHIP_RECORD_BYTES_MAX {
        return Ok((oversized, 0));
    }
    let mut bytes = raw;
    // Mirrors IdsFolloweesRecord's bincode field order, not the event wire
    // format. No String, Vec or BTreeSet is constructed during this preflight.
    let _: (Timestamp, ShortEventId, Timestamp) = take(&mut bytes)?;
    let mut elements = 0;
    for tags in [false, true] {
        if !option_present(&mut bytes)? {
            continue;
        }
        if take::<u32>(&mut bytes)? > 1 {
            return Err(DbError::RelationshipInvariant);
        }
        let count: u64 = take(&mut bytes)?;
        if count > (RELATIONSHIP_SELECTOR_ELEMENTS_MAX - elements) as u64 {
            return Ok((oversized, raw.len()));
        }
        elements += count as usize;
        for _ in 0..count {
            if tags {
                let (_, consumed): (&str, _) =
                    bincode::borrow_decode_from_slice(bytes, redb_bincode::BINCODE_CONFIG)?;
                bytes = &bytes[consumed..];
            } else {
                let _: u8 = take(&mut bytes)?;
            }
        }
    }
    if !bytes.is_empty() {
        return Err(DbError::RelationshipInvariant);
    }
    let record: IdsFolloweesRecord = decode_exact(raw)?;
    Ok((
        RelationshipSelector::Available(record.effective_tags_selector()),
        raw.len(),
    ))
}
