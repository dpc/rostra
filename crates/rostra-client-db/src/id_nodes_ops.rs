use std::collections::{BTreeMap, BTreeSet};

use rostra_core::Timestamp;
use rostra_core::event::IrohNodeId;
use rostra_core::id::RostraId;

use crate::{Database, DbResult, IrohNodeRecord, ids_nodes};

impl Database {
    pub fn trim_iroh_nodes_to_limit_tx(id: RostraId, table: &mut ids_nodes::Table) -> DbResult<()> {
        let existing: BTreeSet<(Timestamp, IrohNodeId)> = table
            .range(&(id, IrohNodeId::ZERO)..=&(id, IrohNodeId::MAX))?
            .map(|res| res.map(|(k, v)| (v.value().announcement_ts, k.value().1)))
            .collect::<Result<BTreeSet<_>, _>>()?;

        if 10 < existing.len() {
            let last = existing.iter().next().expect("Just checked that not empty");
            table.remove(&(id, last.1))?;
        }
        Ok(())
    }

    pub fn get_id_endpoints_tx(
        id: RostraId,
        table: &mut ids_nodes::Table,
    ) -> DbResult<BTreeMap<(Timestamp, IrohNodeId), IrohNodeRecord>> {
        Ok(table
            .range(&(id, IrohNodeId::ZERO)..=&(id, IrohNodeId::MAX))?
            .map(|res| {
                res.map(|(k, v)| {
                    let v = v.value();
                    ((v.announcement_ts, k.value().1), v)
                })
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?)
    }
}
