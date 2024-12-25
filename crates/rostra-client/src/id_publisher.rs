use std::sync::Arc;
use std::time::{Duration, Instant};

use iroh_net::ticket::NodeTicket;
use iroh_net::NodeAddr;
use pkarr::{dns, Keypair, SignedPacket};
use tracing::{debug, info, instrument, warn};

use crate::{Client, ClientHandle};

pub struct IdPublisher {
    app: ClientHandle,
    client: Arc<pkarr::PkarrClientAsync>,
    keypair: pkarr::Keypair,
}

impl IdPublisher {
    pub fn new(app: &Client, keypair: Keypair) -> Self {
        info!(pkarr_id = %keypair.public_key(), "Starting ID publishing task" );
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

            let addr = {
                let Some(app) = self.app.app_ref() else {
                    debug!("App gone, quitting");
                    break;
                };
                match app.iroh_address().await {
                    Ok(addr) => addr,
                    Err(err) => {
                        warn!(%err, "No iroh addresses to publish yet");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        continue;
                    }
                }
            };
            if let Err(err) = self
                .publish(
                    &addr,
                    u32::try_from(interval.period().as_secs() * 3 + 1).unwrap_or(u32::MAX),
                )
                .await
            {
                warn!(%err, "Failed to publish to pkarr");
            }
        }
    }

    pub(crate) fn make_pkarr_packet<'a>(
        keypair: &Keypair,
        records: impl IntoIterator<Item = (&'a str, &'a str)>,
        ttl_secs: u32,
    ) -> pkarr::Result<SignedPacket> {
        let mut packet = dns::Packet::new_reply(0);
        for (k, v) in records.into_iter() {
            packet.answers.push(dns::ResourceRecord::new(
                dns::Name::new(k).unwrap(),
                dns::CLASS::IN,
                ttl_secs,
                dns::rdata::RData::TXT(v.try_into()?),
            ));
        }
        SignedPacket::from_packet(keypair, &packet)
    }

    async fn publish(&self, iroh_node_addr: &NodeAddr, ttl_secs: u32) -> pkarr::Result<()> {
        let instant = Instant::now();

        let node_ticket = NodeTicket::from(iroh_node_addr.clone());

        let packet1 = Self::make_pkarr_packet(
            &self.keypair,
            [("iroh", node_ticket.to_string().as_str())],
            ttl_secs,
        )?;

        self.client.publish(&packet1).await?;

        info!(id = %self.keypair.public_key(), time_ms = instant.elapsed().as_millis(), "Published");

        Ok(())
    }
}
