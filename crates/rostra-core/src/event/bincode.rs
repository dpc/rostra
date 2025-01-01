use super::EventContent;
use crate::ContentHash;

impl EventContent {
    pub fn compute_content_hash(&self) -> ContentHash {
        blake3::hash(&self.0).into()
    }
}
