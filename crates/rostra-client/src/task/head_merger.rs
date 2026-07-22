use std::time::Duration;

use rand::Rng as _;
use rostra_client_db::DbResult;
use rostra_core::event::{EventContentRaw, EventKind, VerifiedEvent};
use rostra_core::id::{RostraId, RostraIdSecretKey};
use rostra_core::{Event, ShortEventId};
use tokio::sync::watch;
use tracing::{debug, error, instrument, trace};

use crate::client::Client;
use crate::task::head_selection::sorted_heads;

const LOG_TARGET: &str = "rostra::head_merger";
const MAX_MERGE_DELAY: Duration = Duration::from_secs(60);

pub struct HeadMerger {
    client: crate::client::ClientHandle,
    id: RostraId,
    self_head_rx: watch::Receiver<Option<ShortEventId>>,
    id_secret: RostraIdSecretKey,
    max_merge_delay: Duration,
}

enum MergeOutcome {
    ClientDropped,
    NoFork,
    Merged,
}

impl HeadMerger {
    pub fn new(client: &Client, id_secret: RostraIdSecretKey) -> Self {
        debug!(target: LOG_TARGET, "Starting followee head merging task" );
        Self {
            client: client.handle(),
            id: client.rostra_id(),
            self_head_rx: client.self_head_subscribe(),
            id_secret,
            max_merge_delay: MAX_MERGE_DELAY,
        }
    }

    /// Run the thread
    #[instrument(name = "head-merger", skip(self), fields(self_id = %self.id.fmt_short()), ret)]
    pub async fn run(self) {
        let mut head_rx = self.self_head_rx.clone();
        loop {
            match self.merge_one_fork().await {
                Err(err) => {
                    error!(
                        target: LOG_TARGET,
                        err = %err,
                        "Failed to store a head-merge event; stopping head merger"
                    );
                    break;
                }
                Ok(MergeOutcome::ClientDropped) => break,
                Ok(MergeOutcome::Merged) => continue,
                Ok(MergeOutcome::NoFork) => {}
            }

            if head_rx.changed().await.is_err() {
                break;
            }
            trace!(target: LOG_TARGET, "Woke up");
        }
    }

    async fn merge_one_fork(&self) -> DbResult<MergeOutcome> {
        let Ok(client) = self.client.client_ref() else {
            return Ok(MergeOutcome::ClientDropped);
        };
        if client.db().get_heads(self.id).await.len() < 2 {
            return Ok(MergeOutcome::NoFork);
        }
        drop(client);

        // Multiple active devices can see the same fork. Delay before rereading
        // the durable set so another device's merge can arrive first.
        let delay = if self.max_merge_delay.is_zero() {
            Duration::ZERO
        } else {
            rand::rng().random_range(Duration::ZERO..self.max_merge_delay)
        };
        tokio::time::sleep(delay).await;

        let Ok(client) = self.client.client_ref() else {
            return Ok(MergeOutcome::ClientDropped);
        };
        let db = client.db();
        let heads = sorted_heads(&db.get_heads(self.id).await);
        let [head1, head2, ..] = heads.as_slice() else {
            return Ok(MergeOutcome::NoFork);
        };

        let empty_content = EventContentRaw::new(vec![]);
        let signed_event = Event::builder_raw_content()
            .author(self.id)
            .kind(EventKind::NULL)
            .parent_prev(*head1)
            .parent_aux(*head2)
            .content(&empty_content)
            .build()
            .signed_by(self.id_secret);

        let verified_event = VerifiedEvent::verify_signed(self.id, signed_event)
            .expect("Can't fail to verify self-created event");
        let verified_event_content =
            rostra_core::event::VerifiedEventContent::verify(verified_event, empty_content)
                .expect("Can't fail to verify self-created content");
        debug!(
            target: LOG_TARGET,
            %head1,
            %head2,
            head = %verified_event.event_id,
            "Merging divergent heads"
        );
        if let Err(err) = db
            .try_process_event_with_content(&verified_event_content)
            .await
        {
            error!(
                target: LOG_TARGET,
                first_head = %head1,
                second_head = %head2,
                merge_event_id = %verified_event.event_id,
                err = %err,
                "Failed to store a head-merge event"
            );
            return Err(err);
        }
        Ok(MergeOutcome::Merged)
    }
}

#[cfg(test)]
mod tests;
