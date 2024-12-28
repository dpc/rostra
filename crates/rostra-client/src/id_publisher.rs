use std::sync::Arc;
use std::time::{Duration, Instant};

use pkarr::dns::rdata::TXT;
use pkarr::dns::SimpleDnsError;
use pkarr::{dns, Keypair, SignedPacket};
use snafu::ResultExt as _;
use tracing::{debug, instrument, trace, warn};

use crate::id::{CompactTicket, IdPublishedData};
use crate::{
    Client, ClientHandle, DnsSnafu, IdPublishResult, PkarrPacketSnafu, PkarrPublishSnafu,
    RRECORD_P2P_KEY, RRECORD_TIP_KEY,
};

const LOG_TARGET: &str = "rostra::client::publisher";

pub struct IdPublisher {
    app: ClientHandle,
    client: Arc<pkarr::PkarrClientAsync>,
    keypair: pkarr::Keypair,
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
        let tip = self.tip.as_ref().map(|tip| tip.to_string());
        if let Some(ticket) = ticket.as_deref() {
            trace!(target: LOG_TARGET, key=%RRECORD_P2P_KEY, val=%ticket, val_len=ticket.len(), "Publishing rrecord");
            packet
                .answers
                .push(make_txt_rrecord(RRECORD_P2P_KEY, ticket, ttl_secs).context(DnsSnafu)?);
        }
        if let Some(tip) = tip.as_deref() {
            trace!(target: LOG_TARGET, key=%RRECORD_TIP_KEY, val=%tip, val_len=tip.len(), "Publishing rrecord");
            packet
                .answers
                .push(make_txt_rrecord(RRECORD_TIP_KEY, tip, ttl_secs).context(DnsSnafu)?);
        }
        SignedPacket::from_packet(keypair, &packet).context(PkarrPacketSnafu)
    }
}

impl IdPublisher {
    pub fn new(app: &Client, keypair: Keypair) -> Self {
        debug!(target: LOG_TARGET, pkarr_id = %keypair.public_key(), "Starting ID publishing task" );
        Self {
            app: app.handle(),
            keypair,
            client: app.pkarr_client(),
        }
    }

    /// Run the thread
    #[instrument(skip(self), ret)]
    pub async fn run(self) {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;

            let (addr, tip) = {
                let Some(app) = self.app.app_ref() else {
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
                    app.event_tip().await,
                )
            };

            let ticket = addr.map(CompactTicket::from);

            let id_data = IdPublishedData { ticket, tip };

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
        let instant = Instant::now();

        let packet = data.to_signed_packet(&self.keypair, ttl_secs)?;

        self.client
            .publish(&packet)
            .await
            .context(PkarrPublishSnafu)?;

        debug!(target: LOG_TARGET, id = %self.keypair.public_key(), time_ms = instant.elapsed().as_millis(), "Published");

        Ok(())
    }
}
