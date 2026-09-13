//! Minimal PNG encoder: 8-bit RGB, non-interlaced, filter 0.
//!
//! Replaces the `png` crate (and with it `fdeflate`) for our single use case.
//! Compression still comes from miniz_oxide (already in the tree via flate2),
//! checksums from the `adler2` crate; only framing + CRC32 live here.

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

#[cfg(test)]
fn zlib_stream(raw: &[u8]) -> Vec<u8> {
    // Filter-0 scanlines, then one zlib stream (stored blocks) + adler32.
    let mut out = Vec::with_capacity(raw.len() + raw.len() / 1000 + 16);
    out.extend_from_slice(&[0x78, 0x01]);
    let mut rest = raw;
    if rest.is_empty() {
        out.extend_from_slice(&[0x01, 0x00, 0x00, 0xff, 0xff]);
    }
    while !rest.is_empty() {
        let n = rest.len().min(65535);
        let (block, tail) = rest.split_at(n);
        out.push(if tail.is_empty() { 0x01 } else { 0x00 });
        out.extend_from_slice(&(n as u16).to_le_bytes());
        out.extend_from_slice(&(!(n as u16)).to_le_bytes());
        out.extend_from_slice(block);
        rest = tail;
    }
    let mut adler = adler2::Adler32::new();
    adler.write_slice(raw);
    out.extend_from_slice(&adler.checksum().to_be_bytes());
    out
}

/// Encode raw RGB pixels (row-major, no padding) to a PNG file.
/// Returns None on bad dimensions instead of panicking.
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

/// Same framing but with an uncompressed zlib stream (stored blocks +
/// adler32, no miniz_oxide involved). Used to cross-check the real encoder
/// in tests: any spec-compliant decoder must accept both.
#[cfg(test)]
pub(crate) fn encode_rgb_stored(width: u32, height: u32, rgb: &[u8]) -> Option<Vec<u8>> {
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
    let mut out = Vec::with_capacity(raw.len() + 64);
    out.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
    let mut ihdr = [0u8; 13];
    ihdr[0..4].copy_from_slice(&width.to_be_bytes());
    ihdr[4..8].copy_from_slice(&height.to_be_bytes());
    ihdr[8] = 8;
    ihdr[9] = 2;
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &zlib_stream(&raw));
    chunk(&mut out, b"IEND", &[]);
    Some(out)
}
