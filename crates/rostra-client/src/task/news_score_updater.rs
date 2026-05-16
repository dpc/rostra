use rostra_core::ExternalEventId;
use rostra_core::id::ToShort as _;
use rostra_util_error::FmtCompact as _;
use tracing::{debug, instrument};

use crate::LOG_TARGET;
use crate::client::Client;

#[derive(Clone)]
pub struct NewsScoreUpdater {
    client: crate::client::ClientHandle,
    self_id: rostra_core::id::RostraId,
    rx: dedup_chan::Receiver<ExternalEventId>,
}

impl NewsScoreUpdater {
    pub fn new(client: &Client) -> Self {
        debug!(target: LOG_TARGET, "Starting news score updater");
        Self {
            client: client.handle(),
            self_id: client.rostra_id(),
            rx: client.db().news_score_updates_subscribe(1024),
        }
    }

    #[instrument(name = "news-score-updater", skip(self), fields(self_id = %self.self_id.to_short()), ret)]
    pub async fn run(mut self) {
        loop {
            let post_id = match self.rx.recv().await {
                Ok(post_id) => post_id,
                Err(dedup_chan::RecvError::Lagging) => {
                    let Ok(db) = self.client.db() else {
                        break;
                    };
                    for post_id in db.get_random_news_post_ids(4).await {
                        db.recalculate_news_post_score(post_id).await;
                    }
                    continue;
                }
                Err(err) => {
                    debug!(target: LOG_TARGET, err = %err.fmt_compact(), "News score updater receiver closed");
                    break;
                }
            };

            let Ok(db) = self.client.db() else {
                break;
            };

            db.recalculate_news_post_score(post_id).await;
            for random_post_id in db.get_random_news_post_ids(4).await {
                db.recalculate_news_post_score(random_post_id).await;
            }
        }
    }
}
