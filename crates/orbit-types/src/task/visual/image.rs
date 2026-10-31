//! Image dimensions and token estimates.

use super::review::VISUAL_IMAGE_TOKEN_CONSTANT;

/// The pixel dimensions of a PNG, JPEG, WebP or GIF image, read from its
/// header. `None` for any other bytes: an image whose size cannot be
/// read cannot be budgeted or cited by region.
#[must_use]
pub fn visual_image_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        // IHDR: width and height are the first two big-endian u32s.
        let width = u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?);
        let height = u32::from_be_bytes(bytes.get(20..24)?.try_into().ok()?);
        return nonzero(width, height);
    }
    if bytes.starts_with(b"GIF8") {
        let width = u32::from(u16::from_le_bytes(bytes.get(6..8)?.try_into().ok()?));
        let height = u32::from(u16::from_le_bytes(bytes.get(8..10)?.try_into().ok()?));
        return nonzero(width, height);
    }
    if bytes.starts_with(&[0xff, 0xd8]) {
        return jpeg_dimensions(bytes);
    }
    if bytes.get(0..4) == Some(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        return webp_dimensions(bytes);
    }
    None
}

fn nonzero(width: u32, height: u32) -> Option<(u32, u32)> {
    (width > 0 && height > 0).then_some((width, height))
}

fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut offset = 2usize;
    while offset + 4 <= bytes.len() {
        if bytes[offset] != 0xff {
            return None;
        }
        let marker = bytes[offset + 1];
        // Start-of-frame markers carry precision, height, width.
        if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) {
            let height = u32::from(u16::from_be_bytes(
                bytes.get(offset + 5..offset + 7)?.try_into().ok()?,
            ));
            let width = u32::from(u16::from_be_bytes(
                bytes.get(offset + 7..offset + 9)?.try_into().ok()?,
            ));
            return nonzero(width, height);
        }
        if marker == 0xda {
            return None;
        }
        let length = usize::from(u16::from_be_bytes(
            bytes.get(offset + 2..offset + 4)?.try_into().ok()?,
        ));
        if length < 2 {
            return None;
        }
        offset += 2 + length;
    }
    None
}

fn webp_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    match bytes.get(12..16)? {
        b"VP8X" => {
            let width =
                1 + u32::from_le_bytes([*bytes.get(24)?, *bytes.get(25)?, *bytes.get(26)?, 0]);
            let height =
                1 + u32::from_le_bytes([*bytes.get(27)?, *bytes.get(28)?, *bytes.get(29)?, 0]);
            nonzero(width, height)
        }
        b"VP8L" => {
            if *bytes.get(20)? != 0x2f {
                return None;
            }
            let bits = u32::from_le_bytes(bytes.get(21..25)?.try_into().ok()?);
            nonzero((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1)
        }
        b"VP8 " => {
            // Lossy bitstream: 14-bit dimensions after the frame tag and
            // the 9d 01 2a start code, each with two scale bits on top.
            if bytes.get(23..26)? != [0x9d, 0x01, 0x2a] {
                return None;
            }
            let width = u32::from(u16::from_le_bytes(bytes.get(26..28)?.try_into().ok()?)) & 0x3fff;
            let height =
                u32::from(u16::from_le_bytes(bytes.get(28..30)?.try_into().ok()?)) & 0x3fff;
            nonzero(width, height)
        }
        _ => None,
    }
}

/// The estimated image tokens of one `width`×`height` image for
/// `provider` (§10). Claude models cost roughly width × height / 750;
/// a provider without a published rule uses
/// [`VISUAL_IMAGE_TOKEN_CONSTANT`] per image. The estimate decides
/// inclusion only; measured tokens are what an experiment reads.
#[must_use]
pub fn visual_image_tokens(provider: &str, width: u32, height: u32) -> u64 {
    let provider = provider.to_ascii_lowercase();
    if provider.contains("claude") || provider.contains("anthropic") {
        let pixels = u64::from(width) * u64::from(height);
        pixels.div_ceil(750).max(1)
    } else {
        VISUAL_IMAGE_TOKEN_CONSTANT
    }
}
