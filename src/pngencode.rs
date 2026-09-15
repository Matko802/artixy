const CRC_TABLE: [u32; 256] = make_crc_table();

const fn make_crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 == 1 { 0xedb88320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
}

pub(crate) fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in data {
        crc = CRC_TABLE[((crc ^ b as u32) & 0xff) as usize] ^ (crc >> 8);
    }
    crc ^ 0xffff_ffff
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut c = Vec::with_capacity(4 + data.len());
    c.extend_from_slice(kind);
    c.extend_from_slice(data);
    out.extend_from_slice(&crc32(&c).to_be_bytes());
}


pub(crate) fn encode_rgb(width: u32, height: u32, rgb: &[u8]) -> Option<Vec<u8>> {
    const LEVEL: u8 = 6;
    if width == 0 || height == 0 {
        return None;
    }
    let stride = width as u64 * 3;
    let expect = stride.checked_mul(height as u64)?;
    if rgb.len() as u64 != expect {
        return None;
    }
    let mut raw = Vec::with_capacity(rgb.len() + height as usize);
    for row in 0..height as usize {
        raw.push(0x00);
        let s = row * stride as usize;
        raw.extend_from_slice(&rgb[s..s + stride as usize]);
    }
    let compressed = miniz_oxide::deflate::compress_to_vec_zlib(&raw, LEVEL);
    let mut out = Vec::with_capacity(compressed.len() + 64);
    out.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
    let mut ihdr = [0u8; 13];
    ihdr[0..4].copy_from_slice(&width.to_be_bytes());
    ihdr[4..8].copy_from_slice(&height.to_be_bytes());
    ihdr[8] = 8;
    ihdr[9] = 2;
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &compressed);
    chunk(&mut out, b"IEND", &[]);
    Some(out)
}


fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let (a, b, c) = (a as i32, b as i32, c as i32);
    let p = a + b - c;
    let pa = (p - a).abs();
    let pb = (p - b).abs();
    let pc = (p - c).abs();
    if pa <= pb && pa <= pc {
        a as u8
    } else if pb <= pc {
        b as u8
    } else {
        c as u8
    }
}

pub(crate) fn decode_png_rgba(png: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    if png.len() < 8 || &png[..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let mut i = 8usize;
    let mut w = 0u32;
    let mut h = 0u32;
    let mut color = 0u8;
    let mut seen_ihdr = false;
    let mut idat: Vec<u8> = Vec::new();
    while i + 8 <= png.len() {
        let len = u32::from_be_bytes([png[i], png[i + 1], png[i + 2], png[i + 3]]) as usize;
        let kind = [png[i + 4], png[i + 5], png[i + 6], png[i + 7]];
        let ds = i + 8;
        let de = ds.checked_add(len)?;
        if de.checked_add(4)? > png.len() {
            return None;
        }
        match &kind {
            b"IHDR" => {
                if len != 13 || seen_ihdr {
                    return None;
                }
                w = u32::from_be_bytes([png[ds], png[ds + 1], png[ds + 2], png[ds + 3]]);
                h = u32::from_be_bytes([png[ds + 4], png[ds + 5], png[ds + 6], png[ds + 7]]);
                if png[ds + 8] != 8 || png[ds + 10] != 0 || png[ds + 11] != 0 || png[ds + 12] != 0 {
                    return None;
                }
                color = png[ds + 9];
                if !matches!(color, 0 | 2 | 4 | 6) {
                    return None;
                }
                if w == 0 || h == 0 || w > 4096 || h > 4096 {
                    return None;
                }
                seen_ihdr = true;
            }
            b"IDAT" => {
                if !seen_ihdr {
                    return None;
                }
                if idat.len().checked_add(len)? > 32_000_000 {
                    return None;
                }
                idat.extend_from_slice(&png[ds..de]);
            }
            b"IEND" => break,
            _ => {}
        }
        i = de + 4;
    }
    if !seen_ihdr || idat.is_empty() {
        return None;
    }
    let ch = match color {
        0 => 1usize,
        2 => 3,
        4 => 2,
        6 => 4,
        _ => return None,
    };
    let stride = (w as usize).checked_mul(ch)?;
    let raw = miniz_oxide::inflate::decompress_to_vec_zlib(&idat).ok()?;
    if raw.len() != (h as usize).checked_mul(stride + 1)? {
        return None;
    }
    let mut px = vec![0u8; (h as usize).checked_mul(stride)?];
    let mut prev = vec![0u8; stride];
    for (row, line) in raw.chunks_exact(stride + 1).enumerate() {
        let dst = &mut px[row * stride..(row + 1) * stride];
        let src = &line[1..];
        match line[0] {
            0 => dst.copy_from_slice(src),
            1 => {
                for k in 0..stride {
                    let a = if k >= ch { dst[k - ch] } else { 0 };
                    dst[k] = src[k].wrapping_add(a);
                }
            }
            2 => {
                for k in 0..stride {
                    dst[k] = src[k].wrapping_add(prev[k]);
                }
            }
            3 => {
                for k in 0..stride {
                    let a = if k >= ch { dst[k - ch] } else { 0 };
                    dst[k] = src[k].wrapping_add(((a as u16 + prev[k] as u16) / 2) as u8);
                }
            }
            4 => {
                for k in 0..stride {
                    let a = if k >= ch { dst[k - ch] } else { 0 };
                    let b = prev[k];
                    let c = if k >= ch { prev[k - ch] } else { 0 };
                    dst[k] = src[k].wrapping_add(paeth(a, b, c));
                }
            }
            _ => return None,
        }
        prev.copy_from_slice(dst);
    }
    let rgba = match color {
        6 => px,
        2 => {
            let mut o = Vec::with_capacity(px.len() / 3 * 4);
            for p in px.chunks_exact(3) {
                o.extend_from_slice(&[p[0], p[1], p[2], 0xff]);
            }
            o
        }
        4 => {
            let mut o = Vec::with_capacity(px.len() / 2 * 4);
            for p in px.chunks_exact(2) {
                o.extend_from_slice(&[p[0], p[0], p[0], p[1]]);
            }
            o
        }
        _ => {
            let mut o = Vec::with_capacity(px.len() * 4);
            for &g in &px {
                o.extend_from_slice(&[g, g, g, 0xff]);
            }
            o
        }
    };
    Some((w, h, rgba))
}
