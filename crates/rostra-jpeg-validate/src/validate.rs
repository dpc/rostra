use crate::decoder::Decoder;
use crate::preflight;

/// Validated encoded geometry; no decoded pixels escape this package.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dimensions {
    /// Encoded width, at most 2048.
    pub width: u32,
    /// Encoded height, at most 2048.
    pub height: u32,
}

/// Rejection under the fixed retained-display subset, not general JPEG
/// validity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    /// Encoded file, axis or pixel budget exceeded.
    TooLarge,
    /// Malformed admitted-subset content or incomplete entropy.
    Invalid(&'static str),
    /// Excluded coding, metadata or header complexity; validity is not
    /// certified.
    Unsupported(&'static str),
}

impl std::fmt::Display for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for Rejection {}

/// Fully validate the fixed JFIF baseline subset and drop all decoded pixels.
///
/// Input is capped at 1,000,000 bytes, axes at 2048 and pixels at 4,000,000.
/// This is neither a hard RSS limit, CPU deadline nor panic-isolation boundary.
/// Callers must bound concurrency and stay off the UI executor. This validates
/// bytes, not signed provenance or host renderer agreement.
pub fn validate(bytes: &[u8]) -> Result<Dimensions, Rejection> {
    let h = preflight::parse(bytes)?;
    let mut decoder = Decoder::new(bytes);
    decoder.set_max_decoding_buffer_size(12_000_000);
    let pixels = decoder.decode().map_err(|_| Rejection::Invalid("decode"))?;
    let c = decoder.completion();
    let info = decoder.info().ok_or(Rejection::Invalid("missing info"))?;
    if usize::from(info.width) != h.width
        || usize::from(info.height) != h.height
        || pixels.len() != h.width * h.height * h.components
        || c.scans != 1
        || !c.eoi
        || c.mcus != h.mcus
        || c.blocks != h.blocks
        || c.real_bytes != h.entropy_bytes
    {
        return Err(Rejection::Invalid("completion geometry/span"));
    }
    // In this single restart-free scan, synthetic refill is only a suffix.
    // Peeking into it is permitted; consuming it cannot pass this subtraction.
    let remaining = (h.entropy_bytes * 8)
        .checked_sub(c.consumed_bits)
        .ok_or(Rejection::Invalid("synthetic bits consumed"))?;
    if remaining > 7 || (u64::from(h.final_byte) & ((1 << remaining) - 1)) != (1 << remaining) - 1 {
        return Err(Rejection::Invalid("extra entropy/padding"));
    }
    drop(pixels);
    Ok(Dimensions {
        width: h.width as u32,
        height: h.height as u32,
    })
}
