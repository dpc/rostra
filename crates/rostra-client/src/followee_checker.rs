use std::time::Duration;

use rostra_core::id::RostraId;
use tracing::{debug, instrument};

use crate::client::Client;
const LOG_TARGET: &str = "rostra::client::publisher";

pub struct FolloweeChecker {
    app: crate::client::ClientHandle,
    followee_updated: tokio::sync::watch::Receiver<Vec<RostraId>>,
}

impl FolloweeChecker {
    pub fn new(app: &Client) -> Self {
        debug!(target: LOG_TARGET, "Starting followee checking task" );
        Self {
            app: app.handle(),
            followee_updated: app
                .watch_self_followee_list()
                .expect("Can't start folowee checker without storage"),
        }
    }

    /// Run the thread
    #[instrument(skip(self), ret)]
    pub async fn run(self) {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        loop {
            interval.tick().await;
        }
    }
}

// Add an example fibonacci function, AI!
fn fibonacci
