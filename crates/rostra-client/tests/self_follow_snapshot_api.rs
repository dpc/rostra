use rostra_client::{Database, DbError, SelfFollowee};
use rostra_core::Timestamp;
use rostra_core::event::PersonasTagsSelector;
use rostra_core::id::RostraId;

#[test]
fn self_follow_snapshot_is_available_through_the_client_facade() {
    async fn snapshot(database: &Database) -> Result<Vec<SelfFollowee>, DbError> {
        database.get_self_followees_snapshot().await
    }

    fn inspect(follow: SelfFollowee) -> (RostraId, PersonasTagsSelector, Timestamp) {
        (follow.followee, follow.persona_selector, follow.first_ts)
    }

    let _ = snapshot;
    let _ = inspect;
}
