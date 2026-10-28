use std::borrow::Cow;

use rostra_core::event::{EventContentKind, EventKind};
use serde::{Deserialize, Serialize};

use crate::public_attachment_request::{PUBLIC_ATTACHMENT_MIME, PublicAttachmentRequest};

/// Borrowed serializer for the existing SocialMedia integer-array CBOR wire.
///
/// This adapter avoids cloning pending bytes. It does not change SocialMedia's
/// singleton defaults or the normalized image API's validation policy.
#[derive(Serialize, Deserialize)]
pub struct PublicAttachmentContent<'a> {
    /// Constant declaration; deserialization is only required by
    /// EventContentKind.
    #[serde(rename = "m")]
    mime: Cow<'a, str>,
    /// Original bytes serialized as an array, not a CBOR byte string.
    #[serde(rename = "d")]
    data: Cow<'a, [u8]>,
}

impl<'a> PublicAttachmentContent<'a> {
    /// Borrow exact pending bytes; callers must validate before publication.
    pub fn new(request: &'a PublicAttachmentRequest) -> Self {
        Self {
            mime: Cow::Borrowed(PUBLIC_ATTACHMENT_MIME),
            data: Cow::Borrowed(&request.data),
        }
    }
}

impl EventContentKind for PublicAttachmentContent<'_> {
    const KIND: EventKind = EventKind::SOCIAL_MEDIA;
}
