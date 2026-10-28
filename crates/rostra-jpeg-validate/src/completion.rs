/// Private scan accounting, meaningful only in the bounded validator
/// composition.
#[derive(Clone, Debug, Default)]
pub struct Completion {
    /// Actual entropy consumption, including any synthetic suffix consumed.
    pub consumed_bits: u64,
    /// Unstuffed bytes fetched before the first marker.
    pub real_bytes: u64,
    /// Synthetic zero bytes inserted after a marker.
    pub synthetic_bytes: u64,
    /// Peeks that extend into the synthetic suffix.
    pub synthetic_peeks: u64,
    /// Completed blocks in scan component order.
    pub blocks: [u64; 4],
    /// Completed MCUs.
    pub mcus: u64,
    /// Completed scans.
    pub scans: u64,
    /// Successful AC fast-path decodes.
    pub fast_ac: u64,
    /// Successful Huffman decodes with codes longer than eight bits.
    pub slow_codes: u64,
    /// Whether the scan's terminal marker was EOI.
    pub eoi: bool,
}
