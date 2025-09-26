use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

use rostra_core::id::RostraId;
use rostra_p2p::Connection;

use crate::ClientRef;

#[derive(Debug)]
pub enum ConnectionState {
    Connected(Connection),
    Failed,
}

#[derive(Default)]
pub struct ConnectionCache {
    connections: BTreeMap<RostraId, ConnectionState>,
}

impl ConnectionCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn get_or_connect(
        &mut self,
        client: &ClientRef<'_>,
        id: RostraId,
    ) -> Option<&mut Connection> {
        match self.connections.entry(id) {
            Entry::Occupied(entry) => match entry.get() {
                ConnectionState::Connected(_) => {}
                ConnectionState::Failed => return None,
            },
            Entry::Vacant(entry) => match client.connect(id).await {
                Ok(conn) => {
                    entry.insert(ConnectionState::Connected(conn));
                }
                Err(_) => {
                    entry.insert(ConnectionState::Failed);
                    return None;
                }
            },
        }

        let ConnectionState::Connected(conn) =
            self.connections.get_mut(&id).expect("Just inserted")
        else {
            unreachable!()
        };
        Some(conn)
    }
}
