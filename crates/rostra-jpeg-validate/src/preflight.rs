//! Bounded, borrowed header/container parser. Never decodes entropy or pixels.

use Rejection::{Invalid, TooLarge, Unsupported};

use crate::Rejection;

pub struct Header {
    pub width: usize,
    pub height: usize,
    pub components: usize,
    pub mcus: u64,
    pub blocks: [u64; 4],
    pub entropy_bytes: u64,
    pub final_byte: u8,
}

fn word(bytes: &[u8]) -> usize {
    usize::from(u16::from_be_bytes([bytes[0], bytes[1]]))
}

pub fn parse(bytes: &[u8]) -> Result<Header, Rejection> {
    if bytes.len() > 1_000_000 {
        return Err(TooLarge);
    }
    if bytes.len() < 4 || bytes[..2] != [255, 216] {
        return Err(Invalid("size/SOI"));
    }
    let mut pos = 2;
    let mut segments = 0;
    let mut jfif = false;
    let mut frame = false;
    let mut qtables = [false; 4];
    let mut htables = [[false; 2]; 2];
    let mut ids = [0; 3];
    let mut sampling = [(0usize, 0usize); 3];
    let mut qt = [0usize; 3];
    let mut header = Header {
        width: 0,
        height: 0,
        components: 0,
        mcus: 0,
        blocks: [0; 4],
        entropy_bytes: 0,
        final_byte: 0,
    };
    loop {
        segments += 1;
        if segments > 32 || pos > 65_536 {
            return Err(Unsupported("header complexity"));
        }
        if pos + 4 > bytes.len() || bytes[pos] != 255 {
            return Err(Invalid("header bound/marker"));
        }
        let marker = bytes[pos + 1];
        if marker == 255 {
            return Err(Unsupported("marker fill"));
        }
        let length = word(&bytes[pos + 2..]);
        if pos + 2 + length > 65_536 {
            return Err(Unsupported("header complexity"));
        }
        if length < 2 || pos + 2 + length > bytes.len() {
            return Err(Invalid("segment length"));
        }
        let data = &bytes[pos + 4..pos + 2 + length];
        pos += 2 + length;
        match marker {
            0xe0 => {
                if jfif || segments != 1 {
                    return Err(Unsupported("JFIF placement/redefinition"));
                }
                if data.len() != 14
                    || &data[..5] != b"JFIF\0"
                    || data[5] != 1
                    || ![1, 2].contains(&data[6])
                    || data[7] > 2
                    || word(&data[8..]) == 0
                    || word(&data[10..]) == 0
                    || data[12..] != [0, 0]
                {
                    return Err(Unsupported("noncanonical JFIF"));
                }
                jfif = true;
            }
            0xdb => {
                let mut p = 0;
                if data.is_empty() {
                    return Err(Invalid("empty DQT"));
                }
                while p < data.len() {
                    let spec = data[p];
                    if spec >> 4 != 0 {
                        return Err(Unsupported("DQT precision"));
                    }
                    let id = usize::from(spec & 15);
                    if id > 3 || qtables[id] || p + 65 > data.len() {
                        return Err(Invalid("DQT id/redefinition/length"));
                    }
                    if data[p + 1..p + 65].contains(&0) {
                        return Err(Invalid("zero DQT"));
                    }
                    qtables[id] = true;
                    p += 65;
                }
            }
            0xc4 => {
                let mut p = 0;
                if data.is_empty() {
                    return Err(Invalid("empty DHT"));
                }
                while p < data.len() {
                    if p + 17 > data.len() {
                        return Err(Invalid("DHT length"));
                    }
                    let class = usize::from(data[p] >> 4);
                    let id = usize::from(data[p] & 15);
                    if class > 1 || id > 1 || htables[class][id] {
                        return Err(Invalid("DHT class/id/redefinition"));
                    }
                    let counts = &data[p + 1..p + 17];
                    let count: usize = counts.iter().map(|&n| usize::from(n)).sum();
                    if count == 0 || count > 256 || p + 17 + count > data.len() {
                        return Err(Invalid("DHT symbol count"));
                    }
                    let mut code = 0u32;
                    for (i, &n) in counts.iter().enumerate() {
                        code += u32::from(n);
                        if code >= (1 << (i + 1)) {
                            return Err(Invalid("DHT tree/all-ones"));
                        }
                        code <<= 1;
                    }
                    for &symbol in &data[p + 17..p + 17 + count] {
                        if (class == 0 && symbol > 11)
                            || (class == 1
                                && !(symbol == 0
                                    || symbol == 0xf0
                                    || (1..=10).contains(&(symbol & 15))))
                        {
                            return Err(Invalid("baseline DHT symbol"));
                        }
                    }
                    htables[class][id] = true;
                    p += 17 + count;
                }
            }
            0xc0 => {
                if frame || data.len() < 6 {
                    return Err(Invalid("SOF count/length"));
                }
                if data[0] != 8 {
                    return Err(Unsupported("precision"));
                }
                let n = usize::from(data[5]);
                if n != 1 && n != 3 {
                    return Err(Unsupported("component layout"));
                }
                if data.len() != 6 + 3 * n {
                    return Err(Invalid("SOF length"));
                }
                header.width = word(&data[3..]);
                header.height = word(&data[1..]);
                header.components = n;
                if header.width > 2048
                    || header.height > 2048
                    || header.width * header.height > 4_000_000
                {
                    return Err(TooLarge);
                }
                if header.width == 0 || header.height == 0 {
                    return Err(Invalid("zero dimensions"));
                }
                for i in 0..n {
                    ids[i] = data[6 + 3 * i];
                    sampling[i] = (
                        usize::from(data[7 + 3 * i] >> 4),
                        usize::from(data[7 + 3 * i] & 15),
                    );
                    qt[i] = usize::from(data[8 + 3 * i]);
                    if ids[i] != (i + 1) as u8 || qt[i] > 3 {
                        return Err(Unsupported("component/table layout"));
                    }
                }
                if (n == 1 && sampling[0] != (1, 1))
                    || (n == 3
                        && (![(1, 1), (2, 1), (2, 2)].contains(&sampling[0])
                            || sampling[1] != (1, 1)
                            || sampling[2] != (1, 1)))
                {
                    return Err(Unsupported("sampling"));
                }
                frame = true;
            }
            0xda => {
                let n = header.components;
                if !frame || data.len() < 4 || data.len() != 1 + 2 * usize::from(data[0]) + 3 {
                    return Err(Invalid("SOS prerequisites/length"));
                }
                if !jfif {
                    return Err(Unsupported("missing JFIF"));
                }
                if usize::from(data[0]) != n {
                    return Err(Unsupported("noninterleaved/multiscan"));
                }
                if data[1 + 2 * n..] != [0, 63, 0] {
                    return Err(Unsupported("scan coding"));
                }
                for i in 0..n {
                    let dc = usize::from(data[2 + 2 * i] >> 4);
                    let ac = usize::from(data[2 + 2 * i] & 15);
                    if data[1 + 2 * i] != ids[i]
                        || dc > 1
                        || ac > 1
                        || !htables[0][dc]
                        || !htables[1][ac]
                        || !qtables[qt[i]]
                    {
                        return Err(Invalid("SOS components/missing tables"));
                    }
                }
                let (h, v) = sampling[0];
                header.mcus = (header.width.div_ceil(8 * h) * header.height.div_ceil(8 * v)) as u64;
                for (i, &(ch, cv)) in sampling[..n].iter().enumerate() {
                    header.blocks[i] = header.mcus * (ch * cv) as u64;
                }
                while pos < bytes.len() {
                    let byte = bytes[pos];
                    pos += 1;
                    if byte == 255 {
                        if pos == bytes.len() {
                            return Err(Invalid("bare entropy FF"));
                        }
                        let next = bytes[pos];
                        pos += 1;
                        if next == 0xd9 {
                            return if pos == bytes.len() {
                                Ok(header)
                            } else {
                                Err(Invalid("trailing bytes"))
                            };
                        }
                        if (0xd0..=0xd7).contains(&next) || next == 0xda {
                            return Err(Unsupported("restart/multiscan"));
                        }
                        if next == 255 {
                            return Err(Unsupported("marker fill"));
                        }
                        if next != 0 {
                            return Err(Invalid("entropy marker/stuffing"));
                        }
                    }
                    header.entropy_bytes += 1;
                    header.final_byte = byte;
                }
                return Err(Invalid("missing EOI"));
            }
            _ => return Err(Unsupported("coding/metadata marker")),
        }
    }
}
