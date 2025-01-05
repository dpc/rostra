use rostra_core::id::RostraId;
use tokio::sync::watch;

use crate::db::{Database, DbResult};

pub struct Storage {
    db: Database,
    self_followee_list_updated: watch::Sender<Vec<RostraId>>,
}

impl Storage {
    pub async fn new(db: Database, self_id: RostraId) -> DbResult<Self> {
        let self_followees = db
            .read_followees(self_id.into())
            .await?
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        let (self_followee_list_updated, _) = watch::channel(self_followees);
        Ok(Self {
            db,
            self_followee_list_updated,
        })
    }

    pub fn watch_self_followee_list(&self) -> watch::Receiver<Vec<RostraId>> {
        self.self_followee_list_updated.subscribe()
    }
}
