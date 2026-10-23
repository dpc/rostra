//! Opt-in static raster policy for native avatar selection and presentation.
//!
//! This is not signed-event validation: historical profile formats remain
//! valid. Callers preserving an untouched legacy avatar need not revalidate it,
//! but must validate bytes again before presenting them through this raster
//! policy. Validation decodes pixels without re-encoding or removing metadata.
//!
//! Encoded length, dimensions, pixel count and output buffers have hard bounds.
//! Codec scratch allocation is not a whole-process memory limit: PNG receives
//! a 64 MiB budget, while WebP scratch is controlled by bounded inputs and
//! dimensions. WebP's allocation limit is only partially enforced upstream.
//! There is no CPU deadline; run off the UI/async executor and bound concurrent
//! validations at the host boundary.

use std::io::Cursor;

use snafu::Snafu;

/// Maximum encoded avatar size, matching the historical profile limit.
pub const MAX_AVATAR_BYTES: usize = 1_000_000;
/// Maximum width or height before pixel decoding.
pub const MAX_AVATAR_DIMENSION: u32 = 2048;
/// Maximum number of pixels before pixel decoding.
pub const MAX_AVATAR_PIXELS: u64 = 4_000_000;
/// Output budget accommodates four-channel sixteen-bit PNG.
const MAX_OUTPUT_BYTES: usize = MAX_AVATAR_PIXELS as usize * 8;
/// Decoder budget where supported, excluding caller-owned output.
const DECODER_BUDGET: usize = 64 * 1024 * 1024;

/// Metadata of an avatar accepted by the opt-in raster policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RasterAvatarInfo {
    /// Canonical MIME type matching the decoded format.
    pub mime_type: &'static str,
    /// Encoded pixel width, before orientation metadata.
    pub width: u32,
    /// Encoded pixel height, before orientation metadata.
    pub height: u32,
}

/// A non-sensitive reason an avatar was rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Snafu)]
pub enum RasterAvatarError {
    /// Only exact canonical PNG and WebP MIME strings are supported.
    #[snafu(display("Avatar must be image/png or image/webp"))]
    UnsupportedMime,
    /// The encoded avatar exceeds the profile byte limit.
    #[snafu(display("Avatar exceeds 1,000,000 encoded bytes"))]
    TooLarge,
    /// The file signature does not match its declared MIME type.
    #[snafu(display("Avatar bytes do not match the declared MIME type"))]
    MimeMismatch,
    /// Container parsing or complete pixel decoding failed.
    #[snafu(display("Avatar image is invalid or incomplete"))]
    InvalidImage,
    /// Animated PNG and WebP are deliberately unsupported.
    #[snafu(display("Animated avatars are not supported"))]
    Animated,
    /// Dimensions or decoded output exceed the native presentation budget.
    #[snafu(display("Avatar exceeds 2048 pixels per side or 4,000,000 pixels"))]
    DimensionsExceeded,
}

/// Validate a complete static raster avatar, including pixel decoding.
///
/// Accepts exact canonical MIME strings only, without parameters or aliases.
/// Rejects animation and incomplete containers; decoder acceptance is not a
/// claim that every other platform decoder interprets malformed data
/// identically. JPEG is deliberately unsupported: evaluated decoders synthesize
/// missing entropy data even when configured for strict decoding.
pub fn validate_raster_avatar(
    declared_mime: &str,
    data: &[u8],
) -> Result<RasterAvatarInfo, RasterAvatarError> {
    let mime_type = match declared_mime {
        "image/png" => "image/png",
        "image/webp" => "image/webp",
        _ => return Err(RasterAvatarError::UnsupportedMime),
    };
    if MAX_AVATAR_BYTES < data.len() {
        return Err(RasterAvatarError::TooLarge);
    }
    let signature_matches = match mime_type {
        "image/png" => data.starts_with(b"\x89PNG\r\n\x1a\n"),
        _ => data.starts_with(b"RIFF") && data.get(8..12) == Some(b"WEBP"),
    };
    if !signature_matches {
        return Err(RasterAvatarError::MimeMismatch);
    }
    let (width, height) = match mime_type {
        "image/png" => png(data)?,
        _ => webp(data)?,
    };
    Ok(RasterAvatarInfo {
        mime_type,
        width,
        height,
    })
}

fn dimensions(width: u32, height: u32) -> Result<(u32, u32), RasterAvatarError> {
    if width == 0
        || height == 0
        || MAX_AVATAR_DIMENSION < width
        || MAX_AVATAR_DIMENSION < height
        || MAX_AVATAR_PIXELS < u64::from(width) * u64::from(height)
    {
        return Err(RasterAvatarError::DimensionsExceeded);
    }
    Ok((width, height))
}

fn output(size: Option<usize>) -> Result<Vec<u8>, RasterAvatarError> {
    let size = size
        .filter(|size| *size <= MAX_OUTPUT_BYTES)
        .ok_or(RasterAvatarError::DimensionsExceeded)?;
    Ok(vec![0; size])
}

fn png(data: &[u8]) -> Result<(u32, u32), RasterAvatarError> {
    // Walk complete chunks before decoding: no hidden animation or missing IEND.
    let mut rest = &data[8..];
    let mut compressed = Vec::new();
    loop {
        if rest.len() < 12 {
            return Err(RasterAvatarError::InvalidImage);
        }
        let len = u32::from_be_bytes(rest[..4].try_into().unwrap()) as usize;
        let chunk = rest
            .get(..len.checked_add(12).ok_or(RasterAvatarError::InvalidImage)?)
            .ok_or(RasterAvatarError::InvalidImage)?;
        if matches!(&chunk[4..8], b"acTL" | b"fcTL" | b"fdAT") {
            return Err(RasterAvatarError::Animated);
        }
        if &chunk[4..8] == b"IDAT" {
            compressed.extend_from_slice(&chunk[8..8 + len]);
        }
        rest = &rest[chunk.len()..];
        if &chunk[4..8] == b"IEND" {
            if len != 0 || !rest.is_empty() {
                return Err(RasterAvatarError::InvalidImage);
            }
            break;
        }
    }
    let mut decoder = png::Decoder::new_with_limits(
        Cursor::new(data),
        png::Limits {
            bytes: DECODER_BUDGET,
        },
    );
    decoder.set_ignore_text_chunk(true);
    decoder.set_ignore_iccp_chunk(true);
    decoder.ignore_checksums(false);
    let header = decoder
        .read_header_info()
        .map_err(|_| RasterAvatarError::InvalidImage)?;
    let size = dimensions(header.width, header.height)?;
    validate_png_zlib(&compressed)?;
    let mut reader = decoder
        .read_info()
        .map_err(|_| RasterAvatarError::InvalidImage)?;
    let mut buffer = output(reader.output_buffer_size())?;
    reader
        .next_frame(&mut buffer)
        .map_err(|_| RasterAvatarError::InvalidImage)?;
    reader
        .finish()
        .map_err(|_| RasterAvatarError::InvalidImage)?;
    Ok(size)
}

/// PNG's pixel decoder tolerates missing zlib checksums after sufficient
/// pixels. Require complete decompression separately, with bounded scratch and
/// a cap covering sixteen-bit RGBA plus one filter byte per nonempty row.
fn validate_png_zlib(compressed: &[u8]) -> Result<(), RasterAvatarError> {
    let mut decoder = flate2::Decompress::new(true);
    let mut scratch = [0; 8192];
    loop {
        let before = (decoder.total_in(), decoder.total_out());
        let status = decoder
            .decompress(
                &compressed[before.0 as usize..],
                &mut scratch,
                flate2::FlushDecompress::None,
            )
            .map_err(|_| RasterAvatarError::InvalidImage)?;
        if MAX_OUTPUT_BYTES as u64 + MAX_AVATAR_PIXELS < decoder.total_out() {
            return Err(RasterAvatarError::DimensionsExceeded);
        }
        if status == flate2::Status::StreamEnd {
            return if decoder.total_in() == compressed.len() as u64 {
                Ok(())
            } else {
                Err(RasterAvatarError::InvalidImage)
            };
        }
        if before == (decoder.total_in(), decoder.total_out()) {
            return Err(RasterAvatarError::InvalidImage);
        }
    }
}

fn webp(data: &[u8]) -> Result<(u32, u32), RasterAvatarError> {
    let riff_size = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;
    if riff_size.checked_add(8) != Some(data.len()) {
        return Err(RasterAvatarError::InvalidImage);
    }
    let mut rest = &data[12..];
    while !rest.is_empty() {
        if rest.len() < 8 {
            return Err(RasterAvatarError::InvalidImage);
        }
        let len = u32::from_le_bytes(rest[4..8].try_into().unwrap()) as usize;
        let chunk_len = len
            .checked_add(len % 2)
            .and_then(|len| len.checked_add(8))
            .ok_or(RasterAvatarError::InvalidImage)?;
        let chunk = rest
            .get(..chunk_len)
            .ok_or(RasterAvatarError::InvalidImage)?;
        match &chunk[..4] {
            b"ANIM" | b"ANMF" => return Err(RasterAvatarError::Animated),
            b"VP8 " => {
                // The lossy decoder allocates using inner dimensions before
                // checking them against VP8X. Bound both, not just the canvas.
                if len < 10 || chunk.get(11..14) != Some(b"\x9d\x01\x2a") {
                    return Err(RasterAvatarError::InvalidImage);
                }
                let width = u16::from_le_bytes(chunk[14..16].try_into().unwrap()) & 0x3fff;
                let height = u16::from_le_bytes(chunk[16..18].try_into().unwrap()) & 0x3fff;
                dimensions(u32::from(width), u32::from(height))?;
            }
            _ => {}
        }
        rest = &rest[chunk_len..];
    }
    let mut decoder = image_webp::WebPDecoder::new(Cursor::new(data))
        .map_err(|_| RasterAvatarError::InvalidImage)?;
    decoder.set_memory_limit(DECODER_BUDGET);
    if decoder.is_animated() {
        return Err(RasterAvatarError::Animated);
    }
    let (width, height) = decoder.dimensions();
    let size = dimensions(width, height)?;
    let mut buffer = output(decoder.output_buffer_size())?;
    decoder
        .read_image(&mut buffer)
        .map_err(|_| RasterAvatarError::InvalidImage)?;
    Ok(size)
}

#[cfg(test)]
mod tests;
