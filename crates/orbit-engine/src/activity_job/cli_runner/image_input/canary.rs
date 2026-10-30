//! The canary image: a PNG showing a random six-character code in large
//! type, rendered per invocation without an image library.
//!
//! The alphabet leaves out glyphs a model could confuse (`0`/`O`/`D`,
//! `1`/`I`/`L`, `2`/`Z`, `5`/`S`, `6`/`G`, `8`/`B`), so a correct reading is
//! unambiguous and a guess is unlikely to match.

/// Characters a canary code is drawn from.
const ALPHABET: &[u8] = b"ACEFHJKMNPQRTUVWXY3479";

/// Characters in one code.
pub(super) const CODE_LENGTH: usize = 6;

/// Pixels per glyph cell.
const SCALE: usize = 12;
/// Glyph cells around the code.
const MARGIN_CELLS: usize = 3;
const GLYPH_WIDTH: usize = 5;
const GLYPH_HEIGHT: usize = 7;

/// A fresh random code, or `None` when the host has no randomness source.
pub(super) fn random_code() -> Option<String> {
    let mut code = String::with_capacity(CODE_LENGTH);
    // Rejection sampling keeps every character equally likely.
    let limit = 256 - 256 % ALPHABET.len();
    while code.len() < CODE_LENGTH {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).ok()?;
        for byte in bytes {
            if usize::from(byte) < limit && code.len() < CODE_LENGTH {
                code.push(char::from(ALPHABET[usize::from(byte) % ALPHABET.len()]));
            }
        }
    }
    Some(code)
}

/// `code` drawn black on white as an 8-bit grayscale PNG.
pub(super) fn render_png(code: &str) -> Vec<u8> {
    let glyphs: Vec<[u8; GLYPH_HEIGHT]> = code.bytes().map(glyph).collect();
    let columns = 2 * MARGIN_CELLS + glyphs.len() * (GLYPH_WIDTH + 1) - 1;
    let rows = 2 * MARGIN_CELLS + GLYPH_HEIGHT;
    let (width, height) = (columns * SCALE, rows * SCALE);
    let mut raw = Vec::with_capacity((width + 1) * height);
    for y in 0..height {
        raw.push(0); // filter: none
        let cell_y = y / SCALE;
        for x in 0..width {
            let cell_x = x / SCALE;
            let ink = cell_y
                .checked_sub(MARGIN_CELLS)
                .filter(|row| *row < GLYPH_HEIGHT)
                .zip(cell_x.checked_sub(MARGIN_CELLS))
                .is_some_and(|(row, column)| {
                    let (index, offset) = (column / (GLYPH_WIDTH + 1), column % (GLYPH_WIDTH + 1));
                    offset < GLYPH_WIDTH
                        && glyphs
                            .get(index)
                            .is_some_and(|bits| bits[row] & (0x10 >> offset) != 0)
                });
            raw.push(if ink { 0x00 } else { 0xff });
        }
    }
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&(width as u32).to_be_bytes());
    header.extend_from_slice(&(height as u32).to_be_bytes());
    // Bit depth 8, grayscale, deflate, adaptive filtering, no interlace.
    header.extend_from_slice(&[8, 0, 0, 0, 0]);
    push_chunk(&mut png, b"IHDR", &header);
    push_chunk(&mut png, b"IDAT", &zlib_stored(&raw));
    push_chunk(&mut png, b"IEND", &[]);
    png
}

fn push_chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    png.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = png.len();
    png.extend_from_slice(kind);
    png.extend_from_slice(data);
    let crc = crc32(&png[start..]);
    png.extend_from_slice(&crc.to_be_bytes());
}

/// A zlib stream of uncompressed deflate blocks: the image is small and
/// mostly white, and a stored stream needs no compressor.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    const BLOCK: usize = 0xffff;
    let mut out = vec![0x78, 0x01];
    let mut chunks = data.chunks(BLOCK).peekable();
    if chunks.peek().is_none() {
        out.extend_from_slice(&[1, 0, 0, 0xff, 0xff]);
    }
    while let Some(chunk) = chunks.next() {
        out.push(u8::from(chunks.peek().is_none()));
        let length = chunk.len() as u16;
        out.extend_from_slice(&length.to_le_bytes());
        out.extend_from_slice(&(!length).to_le_bytes());
        out.extend_from_slice(chunk);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for byte in data {
        a = (a + u32::from(*byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    out.extend_from_slice(&((b << 16) | a).to_be_bytes());
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// The 5×7 bitmap of one alphabet character, top row first, leftmost pixel
/// in bit 4. A character outside the alphabet draws blank.
fn glyph(character: u8) -> [u8; GLYPH_HEIGHT] {
    match character {
        b'A' => [0x0e, 0x11, 0x11, 0x1f, 0x11, 0x11, 0x11],
        b'C' => [0x0e, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0e],
        b'E' => [0x1f, 0x10, 0x10, 0x1e, 0x10, 0x10, 0x1f],
        b'F' => [0x1f, 0x10, 0x10, 0x1e, 0x10, 0x10, 0x10],
        b'H' => [0x11, 0x11, 0x11, 0x1f, 0x11, 0x11, 0x11],
        b'J' => [0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0c],
        b'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        b'M' => [0x11, 0x1b, 0x15, 0x15, 0x11, 0x11, 0x11],
        b'N' => [0x11, 0x11, 0x19, 0x15, 0x13, 0x11, 0x11],
        b'P' => [0x1e, 0x11, 0x11, 0x1e, 0x10, 0x10, 0x10],
        b'Q' => [0x0e, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0d],
        b'R' => [0x1e, 0x11, 0x11, 0x1e, 0x14, 0x12, 0x11],
        b'T' => [0x1f, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        b'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0e],
        b'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0a, 0x04],
        b'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x15, 0x0a],
        b'X' => [0x11, 0x11, 0x0a, 0x04, 0x0a, 0x11, 0x11],
        b'Y' => [0x11, 0x11, 0x0a, 0x04, 0x04, 0x04, 0x04],
        b'3' => [0x1f, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0e],
        b'4' => [0x02, 0x06, 0x0a, 0x12, 0x1f, 0x02, 0x02],
        b'7' => [0x1f, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        b'9' => [0x0e, 0x11, 0x11, 0x0f, 0x01, 0x02, 0x0c],
        _ => [0; GLYPH_HEIGHT],
    }
}
