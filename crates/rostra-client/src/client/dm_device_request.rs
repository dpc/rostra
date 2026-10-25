use rostra_client_db::DbError;
use rostra_client_db::dm_device_request::{DmDeviceOutcome, DmDeviceRequest, DmDeviceRequestId};
use rostra_core::id::RostraIdSecretKey;
use snafu::ResultExt as _;

use super::Client;
use crate::error::{PostResult, StorageSnafu};

impl Client {
    /// Accept an exact own-device retirement or conditional local enrollment
    /// once.
    ///
    /// Persist a random request ID and immutable request before calling.
    /// Authority is checked before receipts, then accepted retries return
    /// the original outcome before current lifecycle or capacity checks.
    /// They never enroll again after a later retirement or restore pruned
    /// announcement content. New enrollment compares the exact
    /// absent/retired local incarnation; a background auto-enrollment
    /// racing an absent expectation is a conflict.
    ///
    /// Success is atomic local acceptance, not delivery, current eligibility,
    /// or distributed compare-and-swap. Receipts are permanent public
    /// metadata, outside payload quotas, and do not deduplicate
    /// restored/independent DBs. Closing rejects new calls, but
    /// cancellation/shutdown after entry does not imply rollback. Retry the
    /// same request after reopening to reconcile. Hosts must independently
    /// enforce their session boundary, bound concurrent commands, and keep
    /// this blocking transaction off their UI executor.
    pub async fn mutate_direct_message_device_idempotent(
        &self,
        secret: RostraIdSecretKey,
        id: DmDeviceRequestId,
        request: &DmDeviceRequest,
    ) -> PostResult<DmDeviceOutcome> {
        self.require_dm_authority(secret)?;
        if let Some(outcome) = self
            .db
            .dm_device_receipt(secret, id, request)
            .await
            .context(StorageSnafu)?
        {
            return Ok(outcome);
        }
        let result = async {
            // Fixed announcement Vec plus Arc conversion; no secret payload.
            let mut allocation = self
                .db
                .reserve_payload_allocation(148)
                .map_err(|reason| DbError::PayloadAdmissionPaused { reason })
                .context(StorageSnafu)?;
            let mut buffer = None;
            self.require_dm_authority(secret)?;
            self.db
                .commit_dm_device_request(secret, id, request, &mut allocation, &mut buffer)
                .await
                .context(StorageSnafu)
        }
        .await;
        if result.is_err()
            && let Some(outcome) = self
                .db
                .dm_device_receipt(secret, id, request)
                .await
                .context(StorageSnafu)?
        {
            return Ok(outcome);
        }
        result
    }
}
