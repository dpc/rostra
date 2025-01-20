use std::time::Duration;

use rostra_core::id::RostraId;
use tracing::{debug, info, instrument};

use crate::client::Client;
const LOG_TARGET: &str = "rostra::publisher";

pub struct FolloweeChecker {
    _client: crate::client::ClientHandle,
    followee_updated: tokio::sync::watch::Receiver<Vec<RostraId>>,
}

impl FolloweeChecker {
    pub fn new(client: &Client) -> Self {
        debug!(target: LOG_TARGET, "Starting followee checking task" );
        Self {
            _client: client.handle(),
            followee_updated: client
                .self_followees_list_subscribe()
                .expect("Can't start folowee checker without storage"),
        }
    }

    /// Run the thread
    #[instrument(skip(self), ret)]
    pub async fn run(self) {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        let mut previous_followees = self.followee_updated.borrow().clone();

        loop {
            interval.tick().await;

            if self.followee_updated.has_changed().unwrap_or(false) {
                let current_followees = self.followee_updated.borrow().clone();
                let new_followees: Vec<_> = current_followees
                    .iter()
                    .filter(|id| !previous_followees.contains(id))
                    .copied()
                    .collect();

                debug!(
                    target: LOG_TARGET,
                    previous_count = previous_followees.len(),
                    current_count = current_followees.len(),
                    new_count = new_followees.len(),
                    "Followee list changed"
                );

                // Query only new followees
                for followee_id in &new_followees {
                    // match self.app.connect(followee_id).await {
                    //     Ok(_) => {
                    //         debug!(
                    //             target: LOG_TARGET,
                    //             followee_id = %followee_id,
                    //             "Successfully connected to followee"
                    //         );
                    //     }
                    //     Err(e) => {
                    //         debug!(
                    //             target: LOG_TARGET,
                    //             followee_id = %followee_id,
                    //             error = %e,
                    //             "Failed to connect to followee"
                    //         );
                    //     }
                    // }
                    //
                    info!(target: LOG_TARGET,
                        followee_id = %followee_id,
                        "Followee is not implemented yet",
                    );
                }

                previous_followees = current_followees;
            }
        }
    }
}
