use std::marker::PhantomData;

use bincode::{Decode, Encode};

use crate::{DbResult, ReadTransaction, ReservedExtensionTableSnafu, WriteTransactionCtx};

/// Built-in table-name prefixes unavailable to extension tables.
pub const EXTENSION_RESERVED_TABLE_PREFIXES: &[&str] = &[
    "_total_migration_",
    "content_",
    "db_",
    "events_",
    "ids_",
    "reception_order_",
    "shoutbox_",
    "social_",
];

/// A typed definition for a caller-owned extension table.
///
/// Extension tables are a trusted in-process persistence facility. Their
/// owners are responsible for schema compatibility, data invariants, and stable
/// names. Core event replay and convergence do not rebuild or validate their
/// contents, although total database migrations preserve extension tables
/// byte-for-byte.
///
/// The durable schema consists of the table name and the exact bincode key and
/// value encodings. The API cannot detect two components opening the same name
/// with different Rust key or value types.
///
/// The exact name `events` and names beginning with any
/// [`EXTENSION_RESERVED_TABLE_PREFIXES`] entry are rejected. New tables should
/// use a globally unique, component-qualified name. Unqualified names are
/// supported only to retain existing extension data.
pub struct ExtensionTableDefinition<'a, K, V> {
    /// Caller-owned redb table name.
    name: &'a str,
    /// Typed key and value marker.
    marker: PhantomData<(K, V)>,
}

impl<'a, K, V> ExtensionTableDefinition<'a, K, V> {
    /// Defines a caller-owned extension table.
    pub const fn new(name: &'a str) -> Self {
        Self {
            name,
            marker: PhantomData,
        }
    }

    /// Returns the persistent redb table name.
    pub const fn name(&self) -> &'a str {
        self.name
    }
}

/// Read-only access to trusted, caller-owned extension tables.
pub struct ExtensionReadTransaction<'a> {
    /// Underlying client-database read transaction.
    tx: &'a ReadTransaction,
}

impl<'a> ExtensionReadTransaction<'a> {
    pub(crate) fn new(tx: &'a ReadTransaction) -> Self {
        Self { tx }
    }

    /// Opens a caller-owned extension table after checking its reserved name.
    ///
    /// Corrupt encoded values return an error from
    /// [`redb_bincode::AccessGuard::value_try`]; the infallible `value` method
    /// panics. Range iteration likewise panics before yielding a corrupt key.
    ///
    /// # Errors
    ///
    /// Returns [`crate::DbError::ReservedExtensionTable`] for a reserved name
    /// and propagates table-open errors.
    pub fn open_table<K, V>(
        &self,
        definition: &ExtensionTableDefinition<'_, K, V>,
    ) -> DbResult<redb_bincode::ReadOnlyTable<K, V>>
    where
        K: Encode + Decode<()>,
        V: Encode + Decode<()>,
    {
        ensure_extension_table(definition.name)?;
        Ok(self
            .tx
            .open_table(&redb_bincode::TableDefinition::new(definition.name))?)
    }
}

/// Mutable access to trusted, caller-owned extension tables.
///
/// This transaction deliberately exposes only typed extension-table access. It
/// cannot open built-in graph, lifecycle, or projection tables and cannot
/// register internal post-commit actions.
pub struct ExtensionWriteTransaction<'a> {
    /// Underlying serialized client-database write transaction.
    tx: &'a WriteTransactionCtx,
}

impl<'a> ExtensionWriteTransaction<'a> {
    pub(crate) fn new(tx: &'a WriteTransactionCtx) -> Self {
        Self { tx }
    }

    /// Opens a caller-owned extension table after checking its reserved name.
    ///
    /// # Errors
    ///
    /// Returns [`crate::DbError::ReservedExtensionTable`] for a reserved name
    /// and propagates table-open errors.
    pub fn open_table<K, V>(
        &self,
        definition: &ExtensionTableDefinition<'_, K, V>,
    ) -> DbResult<redb_bincode::Table<'_, K, V>>
    where
        K: Encode + Decode<()>,
        V: Encode + Decode<()>,
    {
        ensure_extension_table(definition.name)?;
        Ok(self
            .tx
            .open_table(&redb_bincode::TableDefinition::new(definition.name))?)
    }
}

fn ensure_extension_table(name: &str) -> DbResult<()> {
    if is_reserved_extension_table(name) {
        return ReservedExtensionTableSnafu {
            name: name.to_owned(),
        }
        .fail();
    }
    Ok(())
}

pub(crate) fn is_reserved_extension_table(name: &str) -> bool {
    name == "events"
        || EXTENSION_RESERVED_TABLE_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
}

/// Defines a typed caller-owned extension table.
///
/// Use a stable, globally unique, component-qualified name because the name is
/// persisted in redb. Existing unqualified names may be retained for data
/// compatibility. See [`ExtensionTableDefinition`] for ownership, replay, and
/// reserved-name obligations.
///
/// ```
/// rostra_client_db::define_extension_table!(
///     cache_entries, "example.org/cache_entries": u64 => String
/// );
///
/// assert_eq!(cache_entries::TABLE.name(), "example.org/cache_entries");
/// ```
#[macro_export]
macro_rules! define_extension_table {
    ($(#[$outer:meta])*
        $name:ident, $persistent_name:literal : $k:ty => $v:ty) => {
        #[allow(unused)]
        $(#[$outer])*
        pub mod $name {
            use super::*;
            pub type Key = $k;
            pub type Value = $v;
            pub type Definition<'a> =
                $crate::ExtensionTableDefinition<'a, Key, Value>;
            pub const TABLE: Definition<'static> =
                $crate::ExtensionTableDefinition::new($persistent_name);
        }
    };
}
