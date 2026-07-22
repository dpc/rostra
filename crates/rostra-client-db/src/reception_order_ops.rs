//! Durable allocation for database-local reception ordering.

use redb::TableHandle as _;
use rostra_core::Timestamp;
use snafu::OptionExt as _;

use crate::{
    Database, DbResult, OverflowSnafu, ReceptionOrderCollisionSnafu, WriteTransactionCtx,
    reception_order_next,
};

impl Database {
    /// Allocate the next database-local reception sequence in `tx`.
    ///
    /// The counter advances only if the enclosing transaction commits.
    /// `u64::MAX` is reserved as the exhausted sentinel because the table
    /// stores the next unused sequence value.
    fn next_reception_order_tx(tx: &WriteTransactionCtx) -> DbResult<u64> {
        let mut table = tx.open_table(&reception_order_next::TABLE)?;
        let next = table.get(&())?.map(|value| value.value()).unwrap_or(0);
        let following = next.checked_add(1).context(OverflowSnafu)?;
        table.insert(&(), &following)?;
        Ok(next)
    }

    /// Allocate an order and insert one member without replacing an occupied
    /// key.
    pub(crate) fn insert_reception_ordered_tx<V>(
        tx: &WriteTransactionCtx,
        received_at: Timestamp,
        value: &V,
        table: &mut redb_bincode::Table<'_, (Timestamp, u64), V>,
    ) -> DbResult<u64>
    where
        V: bincode::Encode + bincode::Decode<()>,
    {
        let reception_order = Self::next_reception_order_tx(tx)?;
        let key = (received_at, reception_order);
        if table.get(&key)?.is_some() {
            return ReceptionOrderCollisionSnafu {
                table: table.as_raw().name().to_owned(),
                received_at,
                reception_order,
            }
            .fail();
        }
        table.insert(&key, value)?;
        Ok(reception_order)
    }
}
