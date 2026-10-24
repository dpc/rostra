//! Opt-in upload normalization, distinct from historical raster presentation.
//!
//! V1 interprets untagged samples as sRGB. Canonical sRGB PNG declarations are
//! accepted; other color transforms and all EXIF/ICC and WebP XMP are rejected
//! rather than silently discarded. PNG text (including embedded XMP) is
//! removed. PNG samples retain their 8/16-bit precision.
//! Palette, transparency and packed grayscale are expanded before encoding.
//! The output contains only IHDR/IDAT/IEND, never inherited ancillary metadata.
//! Codec versions may change encoded bytes: persist successful output verbatim.
//! Run off the UI executor with bounded concurrency; no CPU/RSS promise.

use std::io::{Cursor, Write};

use snafu::Snafu;

use crate::avatar::{
    MAX_AVATAR_BYTES, MAX_AVATAR_PIXELS, RasterAvatarError, validate_raster_avatar,
};

/// Normalization contract version, independent of encoder implementation.
pub const STATIC_MEDIA_POLICY_VERSION: u32 = 1;

/// Fresh metadata-free PNG bytes ready for durable pending-request storage.
#[derive(Debug)]
pub struct NormalizedStaticMedia {
    /// Exact bytes to persist and retry without re-normalization.
    pub data: Vec<u8>,
    /// Encoded width, with no outstanding orientation transform.
    pub width: u32,
    /// Encoded height, with no outstanding orientation transform.
    pub height: u32,
    /// Policy to bind into the publication request.
    pub policy_version: u32,
}

/// Non-sensitive rejection reasons for upload normalization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Snafu)]
pub enum StaticMediaError {
    /// Existing bounded static-raster validation failed.
    #[snafu(display("{error}"))]
    Raster { error: RasterAvatarError },
    /// A declared visual transform cannot be preserved by this policy.
    #[snafu(display("Unsupported image orientation, color, or ancillary metadata"))]
    UnsupportedMetadata,
    /// Encoder output exceeds the independently enforced byte quota.
    #[snafu(display("Normalized PNG exceeds 1,000,000 bytes"))]
    OutputTooLarge,
    /// Output encoding or canonical validation failed.
    #[snafu(display("Invalid normalized image"))]
    InvalidCanonical,
}

impl From<RasterAvatarError> for StaticMediaError {
    fn from(error: RasterAvatarError) -> Self {
        Self::Raster { error }
    }
}

/// Decode bounded static PNG/WebP pixels and encode a fresh metadata-free PNG.
pub fn normalize_static_media(
    mime: &str,
    data: &[u8],
) -> Result<NormalizedStaticMedia, StaticMediaError> {
    let info = validate_raster_avatar(mime, data)?;
    let (pixels, color, depth) = if mime == "image/png" {
        png_metadata(data, false)?;
        validate_palette_indices(data)?;
        let mut options = png::DecodeOptions::default();
        options.set_skip_ancillary_crc_failures(false);
        let mut decoder = png::Decoder::new_with_options(Cursor::new(data), options);
        decoder.set_limits(png::Limits {
            bytes: 64 * 1024 * 1024,
        });
        decoder.set_ignore_text_chunk(true);
        decoder.set_ignore_iccp_chunk(true);
        decoder.set_transformations(png::Transformations::EXPAND);
        let mut reader = decoder
            .read_info()
            .map_err(|_| StaticMediaError::InvalidCanonical)?;
        let size = reader
            .output_buffer_size()
            .filter(|size| *size <= MAX_AVATAR_PIXELS as usize * 8)
            .ok_or(StaticMediaError::InvalidCanonical)?;
        let mut pixels = vec![0; size];
        let frame = reader
            .next_frame(&mut pixels)
            .map_err(|_| StaticMediaError::InvalidCanonical)?;
        reader
            .finish()
            .map_err(|_| StaticMediaError::InvalidCanonical)?;
        pixels.truncate(frame.buffer_size());
        (pixels, frame.color_type, frame.bit_depth)
    } else {
        let mut rest = &data[12..];
        while !rest.is_empty() {
            let len = u32::from_le_bytes(rest[4..8].try_into().expect("validated chunk")) as usize;
            if !matches!(&rest[..4], b"VP8 " | b"VP8L" | b"VP8X" | b"ALPH") {
                return Err(StaticMediaError::UnsupportedMetadata);
            }
            rest = &rest[8 + len + len % 2..];
        }
        let mut decoder = image_webp::WebPDecoder::new(Cursor::new(data))
            .map_err(|_| StaticMediaError::InvalidCanonical)?;
        decoder.set_memory_limit(64 * 1024 * 1024);
        let size = decoder
            .output_buffer_size()
            .filter(|size| *size <= MAX_AVATAR_PIXELS as usize * 4)
            .ok_or(StaticMediaError::InvalidCanonical)?;
        let color = if decoder.has_alpha() {
            png::ColorType::Rgba
        } else {
            png::ColorType::Rgb
        };
        let mut pixels = vec![0; size];
        decoder
            .read_image(&mut pixels)
            .map_err(|_| StaticMediaError::InvalidCanonical)?;
        (pixels, color, png::BitDepth::Eight)
    };
    let mut output = BoundedOutput {
        bytes: Vec::new(),
        exceeded: false,
    };
    let result = (|| {
        let mut encoder = png::Encoder::new(&mut output, info.width, info.height);
        encoder.set_color(color);
        encoder.set_depth(depth);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(&pixels)?;
        writer.finish()
    })();
    if output.exceeded {
        return Err(StaticMediaError::OutputTooLarge);
    }
    result.map_err(|_| StaticMediaError::InvalidCanonical)?;
    Ok(NormalizedStaticMedia {
        data: output.bytes,
        width: info.width,
        height: info.height,
        policy_version: STATIC_MEDIA_POLICY_VERSION,
    })
}

/// Validate persisted normalized bytes without re-encoding or codec-version
/// equality checks. Only the V1 metadata-free, expanded PNG subset is accepted.
pub fn validate_normalized_static_media(
    mime: &str,
    data: &[u8],
    policy_version: u32,
) -> Result<(), StaticMediaError> {
    if mime != "image/png" || policy_version != STATIC_MEDIA_POLICY_VERSION {
        return Err(StaticMediaError::InvalidCanonical);
    }
    validate_raster_avatar(mime, data)?;
    png_metadata(data, true)?;
    if !matches!(data[24], 8 | 16) || !matches!(data[25], 0 | 2 | 4 | 6) || data[28] != 0 {
        return Err(StaticMediaError::InvalidCanonical);
    }
    Ok(())
}

fn png_metadata(data: &[u8], canonical: bool) -> Result<(), StaticMediaError> {
    let mut rest = &data[8..];
    let depth = data[24];
    let color = data[25];
    let mut palette_entries = None;
    let mut have_pixels = false;
    let mut have_transparency = false;
    while !rest.is_empty() {
        let len = u32::from_be_bytes(rest[..4].try_into().expect("validated chunk")) as usize;
        let body = &rest[8..8 + len];
        match &rest[4..8] {
            b"IHDR" | b"IEND" => {}
            b"IDAT" => have_pixels = true,
            _ if canonical => return Err(StaticMediaError::InvalidCanonical),
            b"PLTE" => {
                if palette_entries.is_some()
                    || have_pixels
                    || len == 0
                    || !len.is_multiple_of(3)
                    || len > 768
                    || matches!(color, 0 | 4)
                    || (color == 3 && len / 3 > 1usize << depth)
                {
                    return Err(StaticMediaError::InvalidCanonical);
                }
                palette_entries = Some(len / 3);
            }
            b"tRNS" => {
                if have_pixels || have_transparency {
                    return Err(StaticMediaError::InvalidCanonical);
                }
                have_transparency = true;
                let valid = match color {
                    3 => palette_entries.is_some_and(|entries| len > 0 && len <= entries),
                    0 | 2 => {
                        let expected = if color == 0 { 2 } else { 6 };
                        len == expected
                            && body.chunks_exact(2).all(|sample| {
                                u32::from(u16::from_be_bytes([sample[0], sample[1]]))
                                    < (1u32 << depth)
                            })
                    }
                    _ => false,
                };
                if !valid {
                    return Err(StaticMediaError::InvalidCanonical);
                }
            }
            b"tEXt" | b"zTXt" | b"iTXt" | b"tIME" | b"pHYs" => {}
            b"sRGB" if !have_pixels && palette_entries.is_none() && body == [0] => {}
            b"gAMA"
                if !have_pixels && palette_entries.is_none() && body == 45455u32.to_be_bytes() => {}
            b"cHRM"
                if !have_pixels
                    && palette_entries.is_none()
                    && body
                        == [31270u32, 32900, 64000, 33000, 30000, 60000, 15000, 6000]
                            .into_iter()
                            .flat_map(u32::to_be_bytes)
                            .collect::<Vec<_>>() => {}
            _ => return Err(StaticMediaError::UnsupportedMetadata),
        }
        rest = &rest[len + 12..];
    }
    Ok(())
}

/// The upstream palette expansion substitutes black for out-of-range indices.
/// Inspect native packed indices first so malformed input cannot change pixels.
fn validate_palette_indices(data: &[u8]) -> Result<(), StaticMediaError> {
    if data[25] != 3 {
        return Ok(());
    }
    let mut decoder = png::Decoder::new_with_limits(
        Cursor::new(data),
        png::Limits {
            bytes: 64 * 1024 * 1024,
        },
    );
    decoder.set_ignore_text_chunk(true);
    decoder.set_ignore_iccp_chunk(true);
    let mut reader = decoder
        .read_info()
        .map_err(|_| StaticMediaError::InvalidCanonical)?;
    let entries = reader
        .info()
        .palette
        .as_ref()
        .ok_or(StaticMediaError::InvalidCanonical)?
        .len()
        / 3;
    let size = reader
        .output_buffer_size()
        .filter(|size| *size <= MAX_AVATAR_PIXELS as usize)
        .ok_or(StaticMediaError::InvalidCanonical)?;
    let mut pixels = vec![0; size];
    let frame = reader
        .next_frame(&mut pixels)
        .map_err(|_| StaticMediaError::InvalidCanonical)?;
    let depth = frame.bit_depth as usize;
    let mask = (1usize << depth) - 1;
    for row in pixels[..frame.buffer_size()].chunks_exact(frame.line_size) {
        for x in 0..frame.width as usize {
            let bit = x * depth;
            let index = (usize::from(row[bit / 8]) >> (8 - depth - bit % 8)) & mask;
            if index >= entries {
                return Err(StaticMediaError::InvalidCanonical);
            }
        }
    }
    Ok(())
}

/// Writer enforcing output quota before growing the encoded byte vector.
struct BoundedOutput {
    /// Admitted output only.
    bytes: Vec<u8>,
    /// Distinguishes quota refusal from other encoder errors.
    exceeded: bool,
}

impl Write for BoundedOutput {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_AVATAR_BYTES - self.bytes.len() {
            self.exceeded = true;
            return Err(std::io::Error::other("normalized image quota"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
