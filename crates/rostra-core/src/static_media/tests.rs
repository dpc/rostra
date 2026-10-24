use super::*;

fn image(color: png::ColorType, depth: png::BitDepth, pixels: &[u8]) -> Vec<u8> {
    let mut data = Vec::new();
    let mut encoder = png::Encoder::new(&mut data, 2, 1);
    encoder.set_color(color);
    encoder.set_depth(depth);
    encoder
        .add_text_chunk("private".into(), "secret location".into())
        .unwrap();
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(pixels).unwrap();
    writer.finish().unwrap();
    data
}

fn chunk(data: &mut Vec<u8>, name: &[u8; 4], body: &[u8]) {
    // The encoder supplies a correct CRC without adding any dependency.
    let mut encoded = Vec::new();
    {
        let mut writer = png::Encoder::new(&mut encoded, 1, 1)
            .write_header()
            .unwrap();
        writer
            .write_chunk(png::chunk::ChunkType(*name), body)
            .unwrap();
    }
    let begin = 33;
    data.splice(
        33..33,
        encoded[begin..begin + body.len() + 12].iter().copied(),
    );
}

#[test]
fn fresh_encoding_strips_metadata_and_preserves_native_precision_alpha_and_samples() {
    for (color, depth, pixels) in [
        (
            png::ColorType::Grayscale,
            png::BitDepth::Eight,
            vec![0, 255],
        ),
        (
            png::ColorType::Rgb,
            png::BitDepth::Eight,
            vec![0, 1, 2, 253, 254, 255],
        ),
        (
            png::ColorType::GrayscaleAlpha,
            png::BitDepth::Sixteen,
            vec![1, 2, 3, 4, 250, 251, 252, 253],
        ),
        (
            png::ColorType::Rgba,
            png::BitDepth::Sixteen,
            (0..16).collect(),
        ),
    ] {
        let source = image(color, depth, &pixels);
        assert!(validate_normalized_static_media("image/png", &source, 1).is_err());
        let normalized = normalize_static_media("image/png", &source).unwrap();
        validate_normalized_static_media("image/png", &normalized.data, 1).unwrap();
        assert!(!normalized.data.windows(7).any(|bytes| bytes == b"private"));
        assert_eq!((normalized.width, normalized.height), (2, 1));
        let mut reader = png::Decoder::new(Cursor::new(&normalized.data))
            .read_info()
            .unwrap();
        let mut decoded = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut decoded).unwrap();
        assert_eq!((info.color_type, info.bit_depth), (color, depth));
        assert_eq!(decoded, pixels);
    }
}

#[test]
fn explicit_color_policy_and_unsupported_orientation_are_not_silently_removed() {
    for (name, body) in [
        (*b"sRGB", vec![0]),
        (*b"gAMA", 45455u32.to_be_bytes().to_vec()),
        (
            *b"cHRM",
            [31270u32, 32900, 64000, 33000, 30000, 60000, 15000, 6000]
                .into_iter()
                .flat_map(u32::to_be_bytes)
                .collect(),
        ),
        (*b"pHYs", vec![0; 9]),
    ] {
        let mut source = image(png::ColorType::Grayscale, png::BitDepth::Eight, &[5, 7]);
        chunk(&mut source, &name, &body);
        let output = normalize_static_media("image/png", &source).unwrap();
        validate_normalized_static_media("image/png", &output.data, 1).unwrap();
    }
    for (name, body) in [
        (*b"gAMA", 100000u32.to_be_bytes().to_vec()),
        (*b"eXIf", b"II*\0\x08\0\0\0\0\0\0\0\0\0".to_vec()),
        (*b"zzZz", vec![0]),
    ] {
        let mut source = image(png::ColorType::Grayscale, png::BitDepth::Eight, &[5, 7]);
        chunk(&mut source, &name, &body);
        assert!(matches!(
            normalize_static_media("image/png", &source),
            Err(StaticMediaError::UnsupportedMetadata)
        ));
    }
}

#[test]
fn lossless_webp_is_reencoded_not_copied_and_all_truncations_fail() {
    let pixels = [3, 5, 7, 9, 11, 13, 15, 17];
    let mut source = Vec::new();
    image_webp::WebPEncoder::new(&mut source)
        .encode(&pixels, 2, 1, image_webp::ColorType::Rgba8)
        .unwrap();
    let normalized = normalize_static_media("image/webp", &source).unwrap();
    validate_normalized_static_media("image/png", &normalized.data, 1).unwrap();
    let mut reader = png::Decoder::new(Cursor::new(&normalized.data))
        .read_info()
        .unwrap();
    let mut decoded = vec![0; reader.output_buffer_size().unwrap()];
    reader.next_frame(&mut decoded).unwrap();
    assert_eq!(decoded, pixels);
    for end in 0..source.len() {
        assert!(normalize_static_media("image/webp", &source[..end]).is_err());
    }
    for end in 0..normalized.data.len() {
        assert!(normalize_static_media("image/png", &normalized.data[..end]).is_err());
    }
}

#[test]
fn expanded_pixels_cannot_bypass_output_quota() {
    // A small palette compresses differently from expanded RGB. Generate a
    // deterministic incompressible index field under the input quota.
    let mut pixels = Vec::new();
    let mut state = 1u32;
    for _ in 0..900_000 {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        pixels.push(state as u8);
    }
    let mut source = Vec::new();
    let mut encoder = png::Encoder::new(&mut source, 1000, 900);
    encoder.set_color(png::ColorType::Indexed);
    encoder.set_palette(
        (0..256)
            .flat_map(|x| {
                [
                    x as u8,
                    (x as u8).wrapping_mul(73),
                    (x as u8).wrapping_mul(151),
                ]
            })
            .collect::<Vec<_>>(),
    );
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(&pixels).unwrap();
    writer.finish().unwrap();
    assert!(source.len() <= MAX_AVATAR_BYTES);
    assert!(matches!(
        normalize_static_media("image/png", &source),
        Err(StaticMediaError::OutputTooLarge)
    ));
}

#[test]
fn malformed_transparency_palette_indices_and_ancillary_crc_do_not_change_pixels_silently() {
    let mut source = image(png::ColorType::Grayscale, png::BitDepth::Eight, &[0, 1]);
    chunk(&mut source, b"tRNS", &[1, 0]);
    assert!(normalize_static_media("image/png", &source).is_err());

    let mut indexed = Vec::new();
    let mut encoder = png::Encoder::new(&mut indexed, 2, 1);
    encoder.set_color(png::ColorType::Indexed);
    encoder.set_palette(vec![1, 2, 3]);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(&[0, 1]).unwrap();
    writer.finish().unwrap();
    assert!(normalize_static_media("image/png", &indexed).is_err());

    let mut source = image(png::ColorType::Grayscale, png::BitDepth::Eight, &[0, 1]);
    chunk(&mut source, b"tRNS", &[0, 0]);
    source[33 + 12 + 2 - 1] ^= 1;
    assert!(normalize_static_media("image/png", &source).is_err());
}

#[test]
fn packed_palette_and_grayscale_expand_without_saturation_or_alpha_loss() {
    let mut indexed = Vec::new();
    let mut encoder = png::Encoder::new(&mut indexed, 2, 1);
    encoder.set_color(png::ColorType::Indexed);
    encoder.set_depth(png::BitDepth::One);
    encoder.set_palette(vec![1, 2, 3, 5, 7, 9]);
    encoder.set_trns(vec![17, 19]);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(&[0b01000000]).unwrap();
    writer.finish().unwrap();
    let output = normalize_static_media("image/png", &indexed).unwrap();
    let mut reader = png::Decoder::new(Cursor::new(output.data))
        .read_info()
        .unwrap();
    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
    reader.next_frame(&mut pixels).unwrap();
    assert_eq!(pixels, [1, 2, 3, 17, 5, 7, 9, 19]);

    let source = image(png::ColorType::Grayscale, png::BitDepth::One, &[0b01000000]);
    let output = normalize_static_media("image/png", &source).unwrap();
    let mut reader = png::Decoder::new(Cursor::new(output.data))
        .read_info()
        .unwrap();
    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
    reader.next_frame(&mut pixels).unwrap();
    assert_eq!(pixels, [0, 255]);
}

#[test]
fn transparency_after_pixels_cannot_be_silently_lost() {
    for (color, pixels, transparency) in [
        (png::ColorType::Grayscale, vec![0, 1], vec![0, 0]),
        (png::ColorType::Rgb, vec![0, 0, 0, 1, 1, 1], vec![0; 6]),
    ] {
        let mut source = image(color, png::BitDepth::Eight, &pixels);
        chunk(&mut source, b"tRNS", &transparency);
        let late: Vec<_> = source.drain(33..33 + transparency.len() + 12).collect();
        let iend = source.len() - 12;
        source.splice(iend..iend, late);
        assert!(normalize_static_media("image/png", &source).is_err());
    }
}
