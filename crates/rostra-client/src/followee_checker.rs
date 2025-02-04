use std::collections::HashMap;
use std::time::Duration;

use rostra_client_db::IdsFolloweesRecord;
use rostra_core::id::RostraId;
use tracing::{debug, info, instrument};

use crate::client::Client;
const LOG_TARGET: &str = "rostra::publisher";

pub struct FolloweeChecker {
    client: crate::client::ClientHandle,
    self_followees_updated: tokio::sync::watch::Receiver<HashMap<RostraId, IdsFolloweesRecord>>,
}

impl FolloweeChecker {
    pub fn new(client: &Client) -> Self {
        debug!(target: LOG_TARGET, "Starting followee checking task" );
        Self {
            client: client.handle(),
            self_followees_updated: client.self_followees_subscribe(),
        }
    }

    /// Run the thread
    #[instrument(skip(self), ret)]
    pub async fn run(mut self) {
        let mut interval = tokio::time::interval(Duration::from_secs(30));

        let mut self_followees = {
            let Ok(storage) = self.client.db() else {
                return;
            };
            storage.get_self_followees().await
        };

        loop {
            tokio::select! {
                // either periodically
                _ = interval.tick() => (),
                // or when our followees change
                res = self.self_followees_updated.changed() => {
                    if res.is_err() {
                        break;
                    }

                    let Ok(storage) = self.client.db() else {
                        break;
                    };


                    let self_followees_new = storage
                        .get_self_followees()
                        .await;

                    debug!(
                        target: LOG_TARGET,
                        // previous_count = previous_followees.len(),
                        // current_count = current_followees.len(),
                        new_count = self_followees_new.len(),
                        "Followee list changed"
                    );

                    self_followees = self_followees_new;

                }
            }

            // Query only new followees
            for (followee_id, _persona_id) in &self_followees {
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

                // previous_followees = current_followees;
            }
        }
    }
}
