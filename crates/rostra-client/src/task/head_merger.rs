use std::time::Duration;

use rand::Rng as _;
use rostra_core::event::{EventKind, VerifiedEvent};
use rostra_core::id::{RostraId, RostraIdSecretKey};
use rostra_core::{Event, ShortEventId};
use tokio::sync::watch;
use tracing::{debug, instrument};

use crate::client::Client;
const LOG_TARGET: &str = "rostra::head_merger";

pub struct HeadMerger {
    client: crate::client::ClientHandle,
    id: RostraId,
    self_head_rx: watch::Receiver<Option<ShortEventId>>,
    id_secret: RostraIdSecretKey,
}

impl HeadMerger {
    pub fn new(client: &Client, id_secret: RostraIdSecretKey) -> Self {
        debug!(target: LOG_TARGET, "Starting followee head merging task" );
        Self {
            client: client.handle(),
            id: client.rostra_id(),
            self_head_rx: client.self_head_subscribe(),
            id_secret,
        }
    }

    /// Run the thread
    #[instrument(skip(self), ret)]
    pub async fn run(self) {
        let mut head_rx = self.self_head_rx.clone();
        while let Ok(_head) = head_rx.changed().await {
            // To avoid two active nodes merging heads together at the same time, producing
            // more heads, that require more merging, etc., we just sleep a random period of
            // time here, which should be enough to propagate and eventually desynchronize.
            let rand_secs = rand::thread_rng().gen_range(0..60);
            tokio::time::sleep(Duration::from_secs(rand_secs)).await;

            let Ok(client) = self.client.client_ref() else {
                break;
            };

            let db = client.db();
            let mut heads = db.get_heads(self.id).await.into_iter();

            let Some(head1) = heads.next() else {
                continue;
            };
            let Some(head2) = heads.next() else {
                continue;
            };

            let signed_event = Event::builder()
                .author(self.id)
                .kind(EventKind::NULL)
                .parent_prev(head1)
                .parent_aux(head2)
                .build()
                .signed_by(self.id_secret);

            let verified_event = VerifiedEvent::verify_signed(self.id, signed_event)
                .expect("Can't fail to verify self-created event");
            let verified_event_content =
                rostra_core::event::VerifiedEventContent::verify(verified_event, None)
                    .expect("Can't fail to verify self-created content");
            debug!(
                target: LOG_TARGET,
                %head1,
                %head2,
                head = %verified_event.event_id,
                "Merging divergent heads"
            );
            let _ = db.process_event_with_content(&verified_event_content).await;
        }
    }
}
