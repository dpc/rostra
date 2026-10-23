use super::{RasterAvatarError, validate_raster_avatar};

fn png(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let encoder = png::Encoder::new(&mut bytes, width, height);
        let mut writer = encoder.write_header().unwrap();
        writer
            .write_image_data(&vec![37; width as usize * height as usize])
            .unwrap();
    }
    bytes
}

fn lossless_webp() -> Vec<u8> {
    let mut bytes = Vec::new();
    image_webp::WebPEncoder::new(&mut bytes)
        .encode(&[53; 18], 3, 2, image_webp::ColorType::Rgb8)
        .unwrap();
    bytes
}

fn fixtures() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("image/png", png(3, 2)),
        (
            "image/webp",
            include_bytes!("fixtures/red-3x2.webp").to_vec(),
        ),
        ("image/webp", lossless_webp()),
    ]
}

#[test]
fn accepts_static_images_and_rejects_every_truncated_prefix() {
    for (mime, bytes) in fixtures() {
        let info = validate_raster_avatar(mime, &bytes).unwrap();
        assert_eq!((info.width, info.height, info.mime_type), (3, 2, mime));
        for end in 0..bytes.len() {
            assert!(
                validate_raster_avatar(mime, &bytes[..end]).is_err(),
                "{mime} accepted truncated prefix {end}"
            );
        }
        let wrong_mime = if mime == "image/png" {
            "image/webp"
        } else {
            "image/png"
        };
        assert_eq!(
            validate_raster_avatar(wrong_mime, &bytes),
            Err(RasterAvatarError::MimeMismatch)
        );
    }
}

#[test]
fn rejects_active_types_aliases_and_oversized_inputs() {
    for mime in [
        "image/svg+xml",
        "image/gif",
        "image/avif",
        "text/html",
        "image/jpg",
        "image/jpeg",
        "IMAGE/PNG",
        "image/png; charset=binary",
    ] {
        assert_eq!(
            validate_raster_avatar(mime, b"<svg/>"),
            Err(RasterAvatarError::UnsupportedMime)
        );
    }
    assert_eq!(
        validate_raster_avatar("image/png", &vec![0; 1_000_001]),
        Err(RasterAvatarError::TooLarge)
    );
    assert!(validate_raster_avatar("image/png", b"\x89PNG\r\n\x1a\n<html>").is_err());
}

#[test]
fn rejects_both_dimension_and_pixel_bombs_before_decoding() {
    for (width, height) in [(2049, 1), (1, 2049), (2048, 2048)] {
        assert_eq!(
            validate_raster_avatar("image/png", &png(width, height)),
            Err(RasterAvatarError::DimensionsExceeded)
        );
    }
    assert!(validate_raster_avatar("image/png", &png(2000, 2000)).is_ok());
}

#[test]
fn rejects_animations_and_incomplete_container_claims() {
    let mut apng = png(3, 2);
    let iend = apng.len() - 12;
    // Even hidden animation controls after the frame are rejected pre-decode.
    apng.splice(iend..iend, [0, 0, 0, 0, b'a', b'c', b'T', b'L', 0, 0, 0, 0]);
    assert_eq!(
        validate_raster_avatar("image/png", &apng),
        Err(RasterAvatarError::Animated)
    );
    let mut webp = lossless_webp();
    webp.extend_from_slice(b"ANIM\0\0\0\0");
    let riff_len = (webp.len() - 8) as u32;
    webp[4..8].copy_from_slice(&riff_len.to_le_bytes());
    assert_eq!(
        validate_raster_avatar("image/webp", &webp),
        Err(RasterAvatarError::Animated)
    );
    for (mime, mut bytes) in fixtures() {
        bytes.push(0);
        assert!(validate_raster_avatar(mime, &bytes).is_err());
    }
}

#[test]
fn rejects_truncated_pixels_even_when_end_markers_are_repaired() {
    let original = png(3, 2);
    let idat = original.windows(4).position(|v| v == b"IDAT").unwrap();
    let len = u32::from_be_bytes(original[idat - 4..idat].try_into().unwrap()) as usize;
    let compressed = &original[idat + 4..idat + 4 + len];
    for end in 0..compressed.len() {
        let mut truncated = Vec::new();
        {
            let mut writer = png::Encoder::new(&mut truncated, 3, 2)
                .write_header()
                .unwrap();
            writer
                .write_chunk(png::chunk::IDAT, &compressed[..end])
                .unwrap();
            writer.finish().unwrap();
        }
        assert!(
            validate_raster_avatar("image/png", &truncated).is_err(),
            "accepted truncated zlib stream {end}"
        );
    }
    for original in [
        lossless_webp(),
        include_bytes!("fixtures/red-3x2.webp").to_vec(),
    ] {
        // Repair both length fields while removing half the encoded pixels.
        let mut truncated = original[..20 + (original.len() - 20) / 2].to_vec();
        let chunk_len = (truncated.len() - 20) as u32;
        truncated[16..20].copy_from_slice(&chunk_len.to_le_bytes());
        if !chunk_len.is_multiple_of(2) {
            truncated.push(0);
        }
        let riff_len = (truncated.len() - 8) as u32;
        truncated[4..8].copy_from_slice(&riff_len.to_le_bytes());
        assert!(validate_raster_avatar("image/webp", &truncated).is_err());
    }
}

#[test]
fn bounds_lossy_inner_dimensions_independently_of_the_canvas() {
    let mut bytes = include_bytes!("fixtures/red-3x2.webp").to_vec();
    let vp8 = bytes.windows(4).position(|v| v == b"VP8 ").unwrap();
    bytes[vp8 + 14..vp8 + 16].copy_from_slice(&16383u16.to_le_bytes());
    assert_eq!(
        validate_raster_avatar("image/webp", &bytes),
        Err(RasterAvatarError::DimensionsExceeded)
    );
}

#[test]
fn jpeg_is_unsupported_even_with_valid_framing_and_a_decodable_damaged_scan() {
    // Generated with ImageMagick: -size 3x2 xc:red red-3x2.jpg.
    // Both evaluated JPEG decoders accepted this file with its entire scan
    // removed and EOI repaired. Keep that case explicit rather than relying
    // on a strict-mode flag as proof of complete entropy decoding.
    let jpeg = include_bytes!("fixtures/red-3x2.jpg");
    let scan = jpeg.windows(2).position(|v| v == b"\xff\xda").unwrap();
    let header_len = u16::from_be_bytes(jpeg[scan + 2..scan + 4].try_into().unwrap());
    let mut damaged = jpeg[..scan + 2 + usize::from(header_len)].to_vec();
    damaged.extend_from_slice(b"\xff\xd9");
    for data in [jpeg.as_slice(), &damaged] {
        assert_eq!(
            validate_raster_avatar("image/jpeg", data),
            Err(RasterAvatarError::UnsupportedMime)
        );
    }
}
