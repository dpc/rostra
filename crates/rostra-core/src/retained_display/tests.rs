use std::io::Write as _;

use super::*;
use crate::avatar::validate_raster_avatar;

fn png(width: u32, height: u32, rgba16: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, width, height);
        if rgba16 {
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Sixteen);
        }
        let mut writer = encoder.write_header().unwrap();
        {
            let mut stream = writer.stream_writer().unwrap();
            let row = vec![37; width as usize * if rgba16 { 8 } else { 1 }];
            for _ in 0..height {
                stream.write_all(&row).unwrap();
            }
            stream.finish().unwrap();
        }
        writer.finish().unwrap();
    }
    bytes
}

#[test]
fn source_bounds_are_inclusive_and_do_not_expand_avatar_policy() {
    for (width, height) in [(8192, 1), (1, 8192), (8000, 4000), (4000, 8000)] {
        let bytes = png(width, height, false);
        let info = validate_retained_png_for_display(&bytes).unwrap();
        assert_eq!((info.width, info.height), (width, height));
        assert_eq!(
            validate_raster_avatar("image/png", &bytes),
            Err(RasterAvatarError::DimensionsExceeded)
        );
    }
    for (width, height) in [(8193, 1), (1, 8193), (8000, 4001), (4001, 8000)] {
        assert_eq!(
            validate_retained_png_for_display(&png(width, height, false)),
            Err(RetainedPngError::TooLarge)
        );
    }
    for (width, height) in [(2048, 1), (2000, 2000), (8192, 2)] {
        assert!(validate_retained_png_for_display(&png(width, height, true)).is_ok());
    }
}

#[test]
fn complete_png_validation_rejects_truncation_crc_animation_and_trailing_bytes() {
    let bytes = png(4096, 1, true);
    for end in 0..bytes.len() {
        assert!(
            validate_retained_png_for_display(&bytes[..end]).is_err(),
            "accepted prefix {end}"
        );
    }
    let mut corrupt = bytes.clone();
    corrupt[29] ^= 1; // IHDR CRC
    assert_eq!(
        validate_retained_png_for_display(&corrupt),
        Err(RetainedPngError::Invalid)
    );
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert_eq!(
        validate_retained_png_for_display(&trailing),
        Err(RetainedPngError::Invalid)
    );
    let mut animated = bytes.clone();
    animated.splice(33..33, [0, 0, 0, 0, b'a', b'c', b'T', b'L', 0, 0, 0, 0]);
    assert_eq!(
        validate_retained_png_for_display(&animated),
        Err(RetainedPngError::Unsupported)
    );
    let mut interlaced = bytes;
    interlaced[28] = 1;
    // Unsupported does not certify validity; reject before any pixel decoder.
    assert_eq!(
        validate_retained_png_for_display(&interlaced),
        Err(RetainedPngError::Unsupported)
    );
    assert_eq!(
        validate_retained_png_for_display(&vec![0; MAX_AVATAR_BYTES + 1]),
        Err(RetainedPngError::TooLarge)
    );
}

#[test]
fn complete_zlib_and_bounded_expansion_are_required() {
    let mut compressor = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    compressor.write_all(&[0; 4097]).unwrap();
    let compressed = compressor.finish().unwrap();
    for end in 0..compressed.len() {
        let mut bytes = Vec::new();
        {
            let mut writer = png::Encoder::new(&mut bytes, 4096, 1)
                .write_header()
                .unwrap();
            writer
                .write_chunk(png::chunk::IDAT, &compressed[..end])
                .unwrap();
            writer.finish().unwrap();
        }
        assert!(validate_retained_png_for_display(&bytes).is_err());
    }
    let mut compressor = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    compressor.write_all(&[0; 40_000]).unwrap();
    let mut bytes = Vec::new();
    {
        let mut writer = png::Encoder::new(&mut bytes, 4096, 1)
            .write_header()
            .unwrap();
        writer
            .write_chunk(png::chunk::IDAT, &compressor.finish().unwrap())
            .unwrap();
        writer.finish().unwrap();
    }
    assert_eq!(
        validate_retained_png_for_display(&bytes),
        Err(RetainedPngError::TooLarge)
    );
}

#[test]
fn small_interlaced_png_keeps_existing_admission() {
    let mut compressor = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    // A 1x1 Adam7 image has a single nonempty pass and one grayscale sample.
    compressor.write_all(&[0, 37]).unwrap();
    let mut bytes = Vec::new();
    {
        let mut info = png::Info::with_size(1, 1);
        info.interlaced = true;
        let mut writer = png::Encoder::with_info(&mut bytes, info)
            .unwrap()
            .write_header()
            .unwrap();
        writer
            .write_chunk(png::chunk::IDAT, &compressor.finish().unwrap())
            .unwrap();
        writer.finish().unwrap();
    }
    assert_eq!(
        validate_retained_png_for_display(&bytes).unwrap(),
        validate_raster_avatar("image/png", &bytes).unwrap(),
    );
}
