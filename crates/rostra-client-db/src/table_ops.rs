use redb_bincode::ReadTransaction;

use crate::{Database, DbResult};

impl Database {
    pub(crate) fn dump_table_dbtx<K, V>(
        dbtx: &ReadTransaction,
        def: &redb_bincode::TableDefinition<'_, K, V>,
    ) -> DbResult<()>
    where
        V: bincode::Decode<()> + bincode::Encode + serde::Serialize,
        K: bincode::Decode<()> + bincode::Encode + serde::Serialize,
    {
        let tbl = dbtx.open_table(def)?;
        for record in tbl.range(..)? {
            let (k, v) = record?;
            println!(
                "{} => {}",
                serde_json::to_string(&k.value()).expect("Can't fail"),
                serde_json::to_string(&v.value()).expect("Can't fail")
            )
        }
        Ok(())
    }
}
