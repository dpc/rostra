use std::num::NonZeroUsize;

use rostra_client::{
    Database, DbError, SocialPostMaterialization, SocialPostMaterializationCursor,
    SocialPostMaterializationPage,
};

#[test]
fn materialization_feed_types_and_scan_are_reexported() {
    async fn scan(
        database: &Database,
        cursor: Option<SocialPostMaterializationCursor>,
    ) -> Result<SocialPostMaterializationPage, DbError> {
        let _tip = database.get_social_post_materialization_tip().await?;
        database
            .scan_social_post_materializations(cursor, NonZeroUsize::MIN)
            .await
    }

    fn inspect(item: SocialPostMaterialization) {
        match item {
            SocialPostMaterialization::Present {
                post_id,
                authored_at,
                content,
            } => {
                let _ = (post_id, authored_at, content);
            }
            SocialPostMaterialization::Removed { post_id } => {
                let _ = post_id;
            }
        }
    }

    let _ = scan;
    let _ = inspect;
}
