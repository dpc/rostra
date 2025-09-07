use std::sync::Arc;
use std::time::{Duration, Instant};

use pkarr::dns::rdata::TXT;
use pkarr::{Keypair, SignedPacket};
use rand::Rng as _;
use rostra_core::ShortEventId;
use rostra_core::id::{RostraId, RostraIdSecretKey, ToShort as _};
use rostra_util_error::BoxedErrorResult;
use rostra_util_fmt::AsFmtOption as _;
use snafu::ResultExt as _;
use tokio::sync::watch;
use tracing::{debug, info, instrument, trace, warn};

use crate::client::Client;
use crate::error::{DnsSnafu, IdPublishResult, PkarrPublishSnafu, PkarrSignedPacketSnafu};
use crate::id::{CompactTicket, IdPublishedData};
use crate::{RRECORD_HEAD_KEY, RRECORD_P2P_KEY};
const LOG_TARGET: &str = "rostra::id-publish";

pub fn publishing_interval() -> Duration {
    Duration::from_secs(360)
}

pub struct PkarrIdPublisher {
    client: crate::client::ClientHandle,
    pkarr_client: Arc<pkarr::Client>,
    keypair: pkarr::Keypair,
    self_head_rx: watch::Receiver<Option<ShortEventId>>,
}

impl PkarrIdPublisher {
    fn self_id(&self) -> RostraId {
        RostraId::from(self.keypair.public_key())
    }
}

impl IdPublishedData {
    fn to_signed_packet<'s, 'n, 'txt>(
        &'s self,
        keypair: &Keypair,
        ttl_secs: u32,
    ) -> IdPublishResult<SignedPacket>
    where
        'n: 'txt,
    {
        let mut packet = pkarr::SignedPacket::builder();

        let ticket = self.ticket.as_ref().map(|ticket| ticket.to_string());
        let head = self.head.as_ref().map(|head| head.to_string());
        if let Some(ticket) = ticket.as_deref() {
            trace!(target: LOG_TARGET, key=%RRECORD_P2P_KEY, val=%ticket, val_len=ticket.len(), "Publishing rrecord");
            packet = packet.txt(
                pkarr::dns::Name::new_unchecked(RRECORD_P2P_KEY),
                ticket.try_into().context(DnsSnafu)?,
                ttl_secs,
            );
        }
        if let Some(head) = head.as_deref() {
            trace!(target: LOG_TARGET, key=%RRECORD_HEAD_KEY, val=%head, val_len=head.len(), "Publishing rrecord");
            let mut txt = TXT::new();
            txt.add_string(head).context(DnsSnafu)?;
            packet = packet.txt(
                pkarr::dns::Name::new_unchecked(RRECORD_HEAD_KEY),
                head.try_into().context(DnsSnafu)?,
                ttl_secs,
            );
        }
        packet.build(keypair).context(PkarrSignedPacketSnafu)
    }
}

impl PkarrIdPublisher {
    pub fn new(client: &Client, id_secret: RostraIdSecretKey) -> Self {
        debug!(target: LOG_TARGET, pkarr_id = %id_secret.id().to_unprefixed_z32_string(), "Starting ID publishing task" );
        Self {
            client: client.handle(),
            keypair: id_secret.into(),
            pkarr_client: client.pkarr_client(),
            self_head_rx: client.self_head_subscribe(),
        }
    }

    /// Run the thread
    #[instrument(skip(self), ret)]
    pub async fn run(mut self) {
        info!(target: LOG_TARGET, "Starting pkarr id publisher");
        let mut interval = tokio::time::interval(publishing_interval());
        loop {
            tokio::select! {
                // either periodically
                _ = interval.tick() => (),
                // or when our head changes
                res = self.self_head_rx.changed() => {
                    if res.is_err() {
                        break;
                    }
                }
            }
            trace!(target: LOG_TARGET, "Woke up");

            self.wait_for_your_turn().await;
            debug!(target: LOG_TARGET, "Detect no other peer alive, assuming the role of Pkarr ID publisher");

            let (addr, head) = {
                let Some(app) = self.client.app_ref_opt() else {
                    debug!(target: LOG_TARGET, "Client gone, quitting");
                    break;
                };
                (
                    app.iroh_address()
                        .await
                        .inspect_err(|err| {
                            warn!(%err, "No iroh addresses to publish yet");
                        })
                        .ok(),
                    app.events_head().await,
                )
            };

            let ticket = addr.map(CompactTicket::from);

            let id_data = IdPublishedData { ticket, head };

            if let Err(err) = self
                .publish(
                    id_data,
                    u32::try_from(interval.period().as_secs() * 3 + 1).unwrap_or(u32::MAX),
                )
                .await
            {
                warn!(%err, "Failed to publish to pkarr");
            }
        }
    }

    /// Publish current state
    async fn publish(&self, data: IdPublishedData, ttl_secs: u32) -> IdPublishResult<()> {
        trace!(
            target: LOG_TARGET,
            id = %self.self_id().to_short(),
            ticket = ?data.ticket,
            head = %data.head.fmt_option(),
            "Publishing RostraId"
        );
        let instant = Instant::now();

        let packet = data.to_signed_packet(&self.keypair, ttl_secs)?;

        self.pkarr_client
            .publish(&packet, None)
            .await
            .context(PkarrPublishSnafu)?;

        debug!(
            target: LOG_TARGET,
            time_ms = instant.elapsed().as_millis(),
            id = %self.self_id().to_short(),
            head = %data.head.fmt_option(),
            "Published RostraId"
        );

        Ok(())
    }

    async fn wait_for_your_turn(&self) {
        let mut failures_count = 0;

        debug!(
            target: LOG_TARGET,
            "Checking for other alive peers"
        );
        loop {
            if 3 < failures_count {
                break;
            }
            if self.connect_self().await.is_err() {
                failures_count += 1;
            } else {
                trace!(target: LOG_TARGET, "Detect other peer alive, waiting");
                failures_count = 0;
            }
            let secs = rand::thread_rng().gen_range(1..10);
            tokio::time::sleep(Duration::from_secs(secs)).await;
        }
    }

    async fn connect_self(&self) -> BoxedErrorResult<()> {
        let conn = self
            .client
            .client_ref()?
            .connect_by_pkarr_resolution(self.self_id())
            .await?;

        conn.ping(3).await?;

        Ok(())
    }
}
