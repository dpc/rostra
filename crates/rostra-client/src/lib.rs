pub mod error;
mod id_publisher;

use std::marker::PhantomData;
use std::ops;
use std::str::FromStr as _;
use std::sync::{Arc, Weak};

use error::{IrohError, IrohResult};
use id_publisher::IdPublisher;
use iroh_net::ticket::NodeTicket;
use iroh_net::NodeAddr;
use pkarr::dns::rdata::RData;
use pkarr::{dns, Keypair, PkarrClient};
use rostra_core::id::RostraId;
use rostra_p2p_api::ROSTRA_P2P_V0_ALPN;
use snafu::{OptionExt as _, ResultExt as _, Snafu};

#[derive(Debug, Snafu)]
pub enum InitError {
    // Iroh { source: IrohError },
    #[snafu(display("Pkarr Client initialization error"))]
    PkarrClient { source: pkarr::Error },
    #[snafu(display("Iroh Client initialization error"))]
    IrohClient { source: IrohError },
}
pub type InitResult<T> = std::result::Result<T, InitError>;

#[derive(Debug, Snafu)]
pub enum IdResolveError {
    IdNotFound,
    IdInvalid { source: pkarr::Error },
    MissingRecord,
    MissingRecordAttribute,
    InvalidRecordValue,
    ConnectionError { source: IrohError },
    PkarrResolve { source: pkarr::Error },
}

type IdResolveResult<T> = std::result::Result<T, IdResolveError>;

/// Weak handle to [`Client`]
#[derive(Debug, Clone)]
pub struct ClientHandle(Weak<Client>);

impl ClientHandle {
    pub fn app_ref(&self) -> Option<ClientRef<'_>> {
        let app = self.0.upgrade()?;
        Some(ClientRef {
            app,
            r: PhantomData,
        })
    }
}

impl From<Weak<Client>> for ClientHandle {
    fn from(value: Weak<Client>) -> Self {
        Self(value)
    }
}

/// A strong reference to [`Client`]
///
/// It contains a phantom reference, to avoid attempts of
/// storing it anywhere.
#[derive(Debug, Clone)]
pub struct ClientRef<'r> {
    pub(crate) app: Arc<Client>,
    pub(crate) r: PhantomData<&'r ()>,
}

impl<'r> ops::Deref for ClientRef<'r> {
    type Target = Client;

    fn deref(&self) -> &Self::Target {
        &self.app
    }
}

#[derive(Debug)]
pub struct Client {
    /// Weak self-reference that can be given out to components
    pub(crate) handle: ClientHandle,

    pkarr_client: Arc<pkarr::PkarrClientAsync>,

    /// Our main identity (pkarr/ed25519_dalek keypair)
    pub(crate) id_keypair: pkarr::Keypair,

    /// Our iroh-net endpoint
    pub(crate) endpoint: iroh_net::Endpoint,
}

impl Client {
    pub async fn new() -> InitResult<Arc<Self>> {
        let pkarr_client = PkarrClient::builder()
            .build()
            .context(PkarrClientSnafu)?
            .as_async()
            .into();

        let endpoint = Self::make_iroh_endpoint().await?;

        let id_keypair = Keypair::random();

        let client = Arc::new_cyclic(|app| Self {
            handle: app.clone().into(),
            id_keypair,
            endpoint,
            pkarr_client,
        });

        client.start_id_publisher();

        Ok(client)
    }

    pub(crate) async fn make_iroh_endpoint() -> InitResult<iroh_net::Endpoint> {
        use iroh_net::discovery::dns::DnsDiscovery;
        use iroh_net::discovery::pkarr::PkarrPublisher;
        use iroh_net::discovery::ConcurrentDiscovery;
        use iroh_net::key::SecretKey;
        use iroh_net::Endpoint;

        let secret_key = SecretKey::generate();
        let discovery = ConcurrentDiscovery::from_services(vec![
            Box::new(PkarrPublisher::n0_dns(secret_key.clone())),
            Box::new(DnsDiscovery::n0_dns()),
        ]);
        let ep = Endpoint::builder()
            .secret_key(secret_key)
            .discovery(Box::new(discovery))
            .bind()
            .await
            .context(IrohClientSnafu)?;
        Ok(ep)
    }

    fn start_id_publisher(&self) {
        tokio::spawn(IdPublisher::new(self, self.id_keypair.clone()).run());
    }

    pub(crate) async fn iroh_address(&self) -> IrohResult<NodeAddr> {
        self.endpoint.node_addr().await
    }

    pub(crate) fn handle(&self) -> ClientHandle {
        self.handle.clone()
    }

    pub async fn resolve_id(&self, id: RostraId) -> IdResolveResult<NodeTicket> {
        let packet = self
            .pkarr_client
            .resolve(&pkarr::PublicKey::try_from(id).context(IdInvalidSnafu)?)
            .await
            .context(PkarrResolveSnafu)?
            .context(IdNotFoundSnafu)?;

        let RData::TXT(ref value) = packet
            .packet()
            .answers
            .iter()
            .find(|a| a.name == dns::Name::new("iroh").expect("can't fail"))
            .context(MissingRecordSnafu)?
            .rdata
        else {
            IdNotFoundSnafu.fail()?
        };

        let v = value
            .attributes()
            .into_keys()
            .next()
            .context(MissingRecordAttributeSnafu)?;

        let ticket = iroh_net::ticket::NodeTicket::from_str(&v)
            .ok()
            .context(InvalidRecordValueSnafu)?;

        Ok(ticket)
    }

    pub async fn fetch_data(&self, id: RostraId) -> IdResolveResult<String> {
        let node_ticket = self.resolve_id(id).await?;
        let _connection = self
            .endpoint
            .connect(node_ticket, ROSTRA_P2P_V0_ALPN)
            .await
            .context(ConnectionSnafu)?;

        todo!()
    }

    pub(crate) fn pkarr_client(&self) -> Arc<pkarr::PkarrClientAsync> {
        self.pkarr_client.clone()
    }
}
