/// Semantic per-identity payload and metadata accounting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Usage {
    pub(super) current_metadata_size: u64,
    pub(super) total_metadata_size: u64,
    pub(super) current_metadata_num: u64,
    pub(super) total_metadata_num: u64,
    pub(super) current_content_size: u64,
    pub(super) total_content_size: u64,
    pub(super) current_payload_num: u64,
    pub(super) total_payload_num: u64,
    pub(super) missing_payload_size: u64,
    pub(super) missing_payload_num: u64,
    pub(super) deleted_payload_size: u64,
    pub(super) deleted_payload_num: u64,
    pub(super) pruned_payload_size: u64,
    pub(super) pruned_payload_num: u64,
    pub(super) invalid_payload_size: u64,
    pub(super) invalid_payload_num: u64,
}

impl From<crate::IdsDataUsageRecord> for Usage {
    fn from(record: crate::IdsDataUsageRecord) -> Self {
        Self {
            current_metadata_size: record.current_metadata_size,
            total_metadata_size: record.total_metadata_size,
            current_metadata_num: record.current_metadata_num,
            total_metadata_num: record.total_metadata_num,
            current_content_size: record.current_content_size,
            total_content_size: record.total_content_size,
            current_payload_num: record.current_payload_num,
            total_payload_num: record.total_payload_num,
            missing_payload_size: record.missing_payload_size,
            missing_payload_num: record.missing_payload_num,
            deleted_payload_size: record.deleted_payload_size,
            deleted_payload_num: record.deleted_payload_num,
            pruned_payload_size: record.pruned_payload_size,
            pruned_payload_num: record.pruned_payload_num,
            invalid_payload_size: record.invalid_payload_size,
            invalid_payload_num: record.invalid_payload_num,
        }
    }
}
