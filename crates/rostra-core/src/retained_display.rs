//! Larger retained PNG sources for host downsampling, not upload or reuse.

use crate::avatar::{
    MAX_AVATAR_BYTES, MAX_AVATAR_DIMENSION, MAX_AVATAR_PIXELS, RasterAvatarError, RasterAvatarInfo,
};

/// Maximum retained PNG source width or height, before host downsampling.
pub const MAX_SOURCE_DIMENSION: u32 = 8192;
/// Maximum retained PNG source pixel count, before host downsampling.
pub const MAX_SOURCE_PIXELS: u64 = 32_000_000;

/// Failure to validate a retained PNG source for display.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetainedPngError {
    /// Encoded bytes, source geometry or decompressed data exceed policy.
    TooLarge,
    /// The source is malformed or incomplete.
    Invalid,
    /// Animation or large interlaced sources are not supported.
    Unsupported,
}

impl From<RasterAvatarError> for RetainedPngError {
    fn from(error: RasterAvatarError) -> Self {
        match error {
            RasterAvatarError::TooLarge | RasterAvatarError::DimensionsExceeded => Self::TooLarge,
            RasterAvatarError::Animated | RasterAvatarError::UnsupportedMime => Self::Unsupported,
            RasterAvatarError::InvalidImage | RasterAvatarError::MimeMismatch => Self::Invalid,
        }
    }
}

pub(crate) fn dimensions(width: u32, height: u32) -> Result<(u32, u32), RasterAvatarError> {
    if width == 0
        || height == 0
        || width > MAX_SOURCE_DIMENSION
        || height > MAX_SOURCE_DIMENSION
        || u64::from(width) * u64::from(height) > MAX_SOURCE_PIXELS
    {
        return Err(RasterAvatarError::DimensionsExceeded);
    }
    Ok((width, height))
}

/// Validate a complete <=1 MB PNG source without allocating a full raster.
///
/// Sources beyond 2048 per axis or 4,000,000 pixels must be noninterlaced:
/// Android's interlaced decoder can allocate a full source raster even when
/// sampling. All sources are bounded at 8192 per axis and 32,000,000 pixels.
/// Complete chunk, CRC, zlib and scanline decoding precede success. Returned
/// dimensions describe the original, not a display-sized bitmap.
///
/// This is not signed-media authentication, normalization or reuse permission.
/// Hosts must bound concurrency, run off the UI executor, independently verify
/// decoder agreement, sample before allocating display pixels and cap the
/// resulting bitmap at 2048 per axis and 4,000,000 pixels. No RSS limit, CPU
/// deadline or cancellation guarantee is provided.
pub fn validate_retained_png_for_display(
    data: &[u8],
) -> Result<RasterAvatarInfo, RetainedPngError> {
    if data.len() > MAX_AVATAR_BYTES {
        return Err(RetainedPngError::TooLarge);
    }
    if data.len() < 33 || !data.starts_with(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR") {
        return Err(RetainedPngError::Invalid);
    }
    let width = u32::from_be_bytes(data[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(data[20..24].try_into().unwrap());
    dimensions(width, height)?;
    if (width > MAX_AVATAR_DIMENSION
        || height > MAX_AVATAR_DIMENSION
        || u64::from(width) * u64::from(height) > MAX_AVATAR_PIXELS)
        && data[28] != 0
    {
        return Err(RetainedPngError::Unsupported);
    }
    let (width, height) = crate::avatar::png(data, true)?;
    Ok(RasterAvatarInfo {
        mime_type: "image/png",
        width,
        height,
    })
}

#[cfg(test)]
mod tests;
