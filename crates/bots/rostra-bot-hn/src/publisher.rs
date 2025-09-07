use std::sync::Arc;

use rostra_client::Client;
use rostra_client::error::PostError;
use rostra_core::event::PersonaId;
use rostra_core::id::RostraIdSecretKey;
use snafu::{ResultExt, Snafu};
use tracing::{info, warn};

use crate::tables::HnArticle;

#[derive(Debug, Snafu)]
pub enum PublisherError {
    #[snafu(display("Failed to post to Rostra: {source}"))]
    Post { source: PostError },
}

pub type PublisherResult<T> = std::result::Result<T, PublisherError>;

pub struct HnPublisher {
    client: Arc<Client>,
    secret: RostraIdSecretKey,
    persona_id: PersonaId,
}

impl HnPublisher {
    pub fn new(client: Arc<Client>, secret: RostraIdSecretKey) -> Self {
        Self {
            client,
            secret,
            persona_id: PersonaId(0), // Default persona
        }
    }

    /// Format an HN article into a Rostra post body
    fn format_article_post(&self, article: &HnArticle) -> String {
        let mut post = String::new();

        if let Some(ref url) = article.url {
            post.push_str(&format!("##### [{}]({})\n\n", article.title, url));
        } else {
            post.push_str(&format!("##### {}\n\n", article.title));
        }

        post.push_str(&format!("* [💬 HN Comments]({})\n", article.hn_url));

        post.push('\n');
        post
    }

    /// Publish an article to Rostra
    pub async fn publish_article(&self, article: &HnArticle) -> PublisherResult<()> {
        let body = self.format_article_post(article);

        info!(
            hn_id = article.hn_id,
            title = %article.title,
            "Publishing article to Rostra"
        );

        self.client
            .social_post(
                self.secret.clone(),
                body,
                None, // No reply to
                self.persona_id,
            )
            .await
            .context(PostSnafu)?;

        info!(
            hn_id = article.hn_id,
            "Successfully published article to Rostra"
        );
        Ok(())
    }

    /// Publish multiple articles with a delay between each
    pub async fn publish_articles(
        &self,
        articles: &[HnArticle],
    ) -> Vec<(u32, PublisherResult<()>)> {
        let mut results = Vec::new();

        for article in articles {
            let result = self.publish_article(article).await;

            if let Err(ref err) = result {
                warn!(
                    hn_id = article.hn_id,
                    error = %err,
                    "Failed to publish article"
                );
            }

            results.push((article.hn_id, result));

            // Add a small delay between posts to be respectful
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        }

        results
    }
}
