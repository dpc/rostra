use std::time::Duration;

use rostra_core::id::RostraId;
use rostra_util_error::{BoxedErrorResult, FmtCompact};
use snafu::ResultExt as _;
use tracing::{debug, info, instrument};

use crate::client::Client;
use crate::ClientRef;
const LOG_TARGET: &str = "rostra::client::head_checker";

pub struct FolloweeHeadChecker {
    client: crate::client::ClientHandle,
    followee_updated: tokio::sync::watch::Receiver<Vec<RostraId>>,
    check_for_updates_rx: tokio::sync::watch::Receiver<()>,
}

impl FolloweeHeadChecker {
    pub fn new(client: &Client) -> Self {
        debug!(target: LOG_TARGET, "Starting followee head checking task" );
        Self {
            client: client.handle(),
            followee_updated: client
                .self_followees_list_subscribe()
                .expect("Can't start folowee checker without storage"),
            check_for_updates_rx: client.check_for_updates_tx_subscribe(),
        }
    }

    /// Run the thread
    #[instrument(skip(self), ret)]
    pub async fn run(self) {
        // let Self {
        //     client,
        //     mut followee_updated,
        //     mut check_for_updates_rx,
        //     pkarr_client: _,
        //     pkarr_client_relay: _,
        // } = self;
        //
        let mut check_for_updates_rx = self.check_for_updates_rx.clone();
        let mut followee_updated = self.followee_updated.clone();
        let mut interval = tokio::time::interval(Duration::from_secs(10 * 60));

        loop {
            // Trigger on ticks or any change
            tokio::select! {
                _ = interval.tick() => (),
                res = followee_updated.changed() => {
                    if res.is_err() {
                        break;
                    }
                }
                res = check_for_updates_rx.changed() => {
                    if res.is_err() {
                        break;
                    }
                }
            }
            // read / mark everything as read
            let self_followees = followee_updated.borrow_and_update().clone();
            check_for_updates_rx.mark_unchanged();

            for followee in &self_followees {
                let Some(client) = self.client.app_ref() else {
                    debug!(target: LOG_TARGET, "Client gone, quitting");

                    break;
                };

                if let Err(err) = self.check_for_updates(&client, *followee).await {
                    info!(target: LOG_TARGET, err = %err.as_ref().fmt_compact(), "Failed to check for updates");
                }
                todo!()
            }
        }
    }

    async fn check_for_updates(
        &self,
        client: &ClientRef<'_>,
        id: RostraId,
    ) -> BoxedErrorResult<()> {
        let _data = client.resolve_id_data(id).await.boxed()?;

        todo!();
    }
}
