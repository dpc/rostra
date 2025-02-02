use std::future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use pkarr::dns::rdata::TXT;
use pkarr::dns::SimpleDnsError;
use pkarr::{dns, Keypair, SignedPacket};
use rostra_core::id::{RostraId, RostraIdSecretKey, ToShort as _};
use rostra_core::ShortEventId;
use rostra_util_fmt::AsFmtOption as _;
use snafu::ResultExt as _;
use tokio::sync::watch;
use tracing::{debug, instrument, trace, warn};

use crate::client::Client;
use crate::error::{DnsSnafu, IdPublishResult, PkarrPacketSnafu, PkarrPublishSnafu};
use crate::id::{CompactTicket, IdPublishedData};
use crate::{RRECORD_HEAD_KEY, RRECORD_P2P_KEY};
const LOG_TARGET: &str = "rostra::publisher";

pub fn publishing_interval() -> Duration {
    Duration::from_secs(60)
}

pub struct PkarrIdPublisher {
    app: crate::client::ClientHandle,
    pkarr_client: Arc<pkarr::PkarrClientAsync>,
    pkarr_client_relay: Arc<pkarr::PkarrRelayClientAsync>,
    keypair: pkarr::Keypair,
    self_head_rx: Option<watch::Receiver<Option<ShortEventId>>>,
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
        fn make_txt_rrecord<'a>(
            name: &'a str,
            val: &'a str,
            ttl_secs: u32,
        ) -> Result<dns::ResourceRecord<'a>, SimpleDnsError> {
            let mut txt = TXT::new();
            txt.add_string(val)?;
            Ok(dns::ResourceRecord::new(
                dns::Name::new(name)?,
                dns::CLASS::IN,
                ttl_secs,
                dns::rdata::RData::TXT(txt),
            ))
        }

        let mut packet = dns::Packet::new_reply(0);

        let ticket = self.ticket.as_ref().map(|ticket| ticket.to_string());
        let head = self.head.as_ref().map(|head| head.to_string());
        if let Some(ticket) = ticket.as_deref() {
            trace!(target: LOG_TARGET, key=%RRECORD_P2P_KEY, val=%ticket, val_len=ticket.len(), "Publishing rrecord");
            packet
                .answers
                .push(make_txt_rrecord(RRECORD_P2P_KEY, ticket, ttl_secs).context(DnsSnafu)?);
        }
        if let Some(head) = head.as_deref() {
            trace!(target: LOG_TARGET, key=%RRECORD_HEAD_KEY, val=%head, val_len=head.len(), "Publishing rrecord");
            packet
                .answers
                .push(make_txt_rrecord(RRECORD_HEAD_KEY, head, ttl_secs).context(DnsSnafu)?);
        }
        SignedPacket::from_packet(keypair, &packet).context(PkarrPacketSnafu)
    }
}

impl PkarrIdPublisher {
    pub fn new(client: &Client, id_secret: RostraIdSecretKey) -> Self {
        debug!(target: LOG_TARGET, pkarr_id = %id_secret.id().to_z32_string(), "Starting ID publishing task" );
        Self {
            app: client.handle(),
            keypair: id_secret.into(),
            pkarr_client: client.pkarr_client(),
            pkarr_client_relay: client.pkarr_client_relay(),
            self_head_rx: client.self_head_subscribe(),
        }
    }

    /// Run the thread
    #[instrument(skip(self), ret)]
    pub async fn run(mut self) {
        let mut interval = tokio::time::interval(publishing_interval());
        loop {
            tokio::select! {
                // either periodically
                _ = interval.tick() => (),
                // or when our head changes
                res = async {
                    if let Some(rx) = self.self_head_rx.as_mut() {
                        rx.changed().await
                    } else {
                        future::pending().await
                    }
                }  => {
                    if res.is_err() {
                        break;
                    }
                }
            }

            let (addr, head) = {
                let Some(app) = self.app.app_ref_opt() else {
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
                warn!(%err, "Failed to publish to pkarr relay");
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

        let (res, res_relay) = tokio::join!(
            self.pkarr_client.publish(&packet),
            self.pkarr_client_relay.publish(&packet)
        );

        // TODO: report both?
        if res_relay.is_err() && res.is_err() {
            res_relay.context(PkarrPublishSnafu)?;
        }

        debug!(
            target: LOG_TARGET,
            time_ms = instant.elapsed().as_millis(),
            id = %self.self_id().to_short(),
            head = %data.head.fmt_option(),
            "Published RostraId"
        );

        Ok(())
    }
}
