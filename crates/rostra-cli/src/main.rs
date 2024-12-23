mod pkarr_publish;

use std::future::pending;
use std::marker::PhantomData;
use std::ops;
use std::sync::{Arc, Weak};

use iroh_net::NodeAddr;
use pkarr::Keypair;
use snafu::{ResultExt as _, Snafu};
use tracing::Level;

pub const PROJECT_NAME: &str = "rostra";

#[derive(Debug, Snafu)]
pub enum AppError {
    Iroh { source: IrohError },
    Pkarr { source: pkarr::Error },
}

type IrohError = anyhow::Error;
type IrohResult<T> = anyhow::Result<T>;
type AppResult<T> = std::result::Result<T, AppError>;

/// Weak handle to [`App`]
#[derive(Debug, Clone)]
pub struct AppHandle(Weak<App>);

impl AppHandle {
    pub fn app_ref(&self) -> Option<AppRef<'_>> {
        let app = self.0.upgrade()?;
        Some(AppRef {
            app,
            r: PhantomData,
        })
    }
}

impl From<Weak<App>> for AppHandle {
    fn from(value: Weak<App>) -> Self {
        Self(value)
    }
}

/// A strong reference to [`App`]
///
/// It contains a phantom reference, to avoid attempts of
/// storing it anywhere.
#[derive(Debug, Clone)]
pub struct AppRef<'r> {
    app: Arc<App>,
    r: PhantomData<&'r ()>,
}

impl<'r> ops::Deref for AppRef<'r> {
    type Target = App;

    fn deref(&self) -> &Self::Target {
        &self.app
    }
}

#[derive(Debug)]
pub struct App {
    /// Weak self-reference that can be given out to components
    app: AppHandle,

    /// Our main identity (pkarr/ed25519_dalek keypair)
    id_keypair: pkarr::Keypair,

    /// Our iroh endpoint
    endpoint: iroh_net::Endpoint,
}

impl App {
    async fn new() -> AppResult<Arc<Self>> {
        let endpoint = Self::make_iroh_endpoint().await?;
        let id_keypair = Keypair::random();
        Ok(Arc::new_cyclic(|app| Self {
            app: app.clone().into(),
            id_keypair,
            endpoint,
        }))
    }

    async fn make_iroh_endpoint() -> AppResult<iroh_net::Endpoint> {
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
            .context(IrohSnafu)?;
        Ok(ep)
    }

    fn start_id_publishing_task(&self) -> AppResult<()> {
        let task = pkarr_publish::IDPublishingTask::new(self, self.id_keypair.clone())?;
        tokio::spawn(task.run());
        Ok(())
    }

    async fn iroh_address(&self) -> IrohResult<NodeAddr> {
        self.endpoint.node_addr().await
    }

    fn handle(&self) -> AppHandle {
        self.app.clone()
    }
}

#[snafu::report]
#[tokio::main]
async fn main() -> AppResult<()> {
    tracing_subscriber::fmt().with_max_level(Level::INFO).init();

    let app = App::new().await?;

    app.start_id_publishing_task()?;

    pending::<()>().await;

    Ok(())
}
