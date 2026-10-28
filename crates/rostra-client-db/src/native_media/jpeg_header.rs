//! Borrowed CBOR JPEG header hints, not a JPEG validator.

use super::payload::Input;

/// Read at most 32 segments/65536 logical image bytes, with no payload
/// allocation.
pub(super) fn dimensions(encoded: &[u8], len: usize) -> Option<(u32, u32)> {
    let mut input = Input { bytes: encoded };
    if input.byte().ok()? != 255 || input.byte().ok()? != 216 {
        return None;
    }
    let mut pos = 2usize;
    let mut geometry = None;
    for segment in 0..32 {
        if pos + 4 > len || pos + 4 > 65_536 || input.byte().ok()? != 255 {
            return None;
        }
        let marker = input.byte().ok()?;
        let size = usize::from(u16::from_be_bytes([input.byte().ok()?, input.byte().ok()?]));
        let end = pos.checked_add(2 + size)?;
        if size < 2 || end > len || end > 65_536 {
            return None;
        }
        if segment == 0 && marker != 0xe0 {
            return None;
        }
        // Only small fixed metadata is retained on the stack. Tables are skipped
        // here; complete table/scan/entropy validation belongs to the reader.
        let mut data = [0u8; 15];
        match marker {
            0xe0 if segment == 0 && size == 16 => {
                for byte in &mut data[..14] {
                    *byte = input.byte().ok()?;
                }
                if &data[..5] != b"JFIF\0"
                    || data[5] != 1
                    || ![1, 2].contains(&data[6])
                    || data[7] > 2
                    || data[8..10] == [0, 0]
                    || data[10..12] == [0, 0]
                    || data[12..14] != [0, 0]
                {
                    return None;
                }
            }
            0xc0 if geometry.is_none() && [11, 17].contains(&size) => {
                for byte in &mut data[..size - 2] {
                    *byte = input.byte().ok()?;
                }
                let n = usize::from(data[5]);
                if data[0] != 8 || ![1, 3].contains(&n) || size != 8 + 3 * n {
                    return None;
                }
                for i in 0..n {
                    if data[6 + 3 * i] != (i + 1) as u8 || data[8 + 3 * i] > 3 {
                        return None;
                    }
                    let sampling = data[7 + 3 * i];
                    if (n == 1 || i != 0) && sampling != 0x11
                        || n == 3 && i == 0 && ![0x11, 0x21, 0x22].contains(&sampling)
                    {
                        return None;
                    }
                }
                geometry = Some((
                    u32::from(u16::from_be_bytes([data[3], data[4]])),
                    u32::from(u16::from_be_bytes([data[1], data[2]])),
                ));
            }
            0xdb | 0xc4 => {
                for _ in 2..size {
                    input.byte().ok()?;
                }
            }
            0xda => return geometry,
            _ => return None,
        }
        pos = end;
    }
    None
}
