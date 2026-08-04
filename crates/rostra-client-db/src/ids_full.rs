//! Checked access to the retained event-author index.
//!
//! `ids_full` maps each author's 128-bit identity prefix to the remaining 128
//! bits. Readers reconstruct the full identities when enumerating known event
//! authors. Production mutation stays behind [`Table::register`]; test-only
//! corruption fixtures use an explicit bypass.

use rostra_core::id::{RestRostraId, RostraId, ShortRostraId};

use crate::{DbError, DbResult, ReadTransaction, WriteTransactionCtx};

type Definition<'a> = redb_bincode::TableDefinition<'a, ShortRostraId, RestRostraId>;

const TABLE: Definition<'_> = redb_bincode::TableDefinition::new("ids_full");

/// Checked write access to the shortened/full identity mapping.
pub(crate) struct Table<'txn>(redb_bincode::Table<'txn, ShortRostraId, RestRostraId>);

impl<'txn> Table<'txn> {
    pub(crate) fn open(tx: &'txn WriteTransactionCtx) -> DbResult<Self> {
        Ok(Self(tx.open_table(&TABLE)?))
    }

    /// Register the full identity behind a shortened storage key.
    ///
    /// An existing identical mapping is idempotent. A different full identity
    /// with the same shortened prefix fails before changing the mapping.
    pub(crate) fn register(&mut self, id: RostraId) -> DbResult<()> {
        let (prefix, rest) = id.split();
        let existing_rest = self.0.get(&prefix)?.map(|entry| entry.value());

        match existing_rest {
            Some(existing_rest) if existing_rest != rest => Err(DbError::IdentityPrefixCollision {
                prefix,
                existing_id: RostraId::assemble(prefix, existing_rest),
                incoming_id: id,
                location: snafu::Location::new(file!(), line!(), column!()),
            }),
            Some(_) => Ok(()),
            None => {
                self.0.insert(&prefix, &rest)?;
                Ok(())
            }
        }
    }
}

pub(crate) fn init(tx: &WriteTransactionCtx) -> DbResult<()> {
    drop(Table::open(tx)?);
    Ok(())
}

pub(crate) fn read_all(tx: &ReadTransaction) -> DbResult<Vec<RostraId>> {
    Ok(tx
        .open_table(&TABLE)?
        .range(..)?
        .map(|entry| entry.map(|(prefix, rest)| RostraId::assemble(prefix.value(), rest.value())))
        .collect::<Result<_, _>>()?)
}

pub(crate) fn get(tx: &ReadTransaction, prefix: ShortRostraId) -> DbResult<Option<RestRostraId>> {
    Ok(tx
        .open_table(&TABLE)?
        .get(&prefix)?
        .map(|entry| entry.value()))
}

/// Deliberately bypass the collision guard to build corruption fixtures.
#[cfg(test)]
pub(crate) fn set_for_test(
    tx: &WriteTransactionCtx,
    prefix: ShortRostraId,
    rest: Option<RestRostraId>,
) -> DbResult<()> {
    let mut table = tx.open_table(&TABLE)?;
    if let Some(rest) = rest {
        table.insert(&prefix, &rest)?;
    } else {
        table.remove(&prefix)?;
    }
    Ok(())
}
