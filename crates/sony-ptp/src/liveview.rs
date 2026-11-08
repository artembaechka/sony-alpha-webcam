//! Extraction of the JPEG frame from a Sony live view object.
//!
//! The object returned for handle `0xFFFFC002` starts with a small header
//! (offset / size of the JPEG and, on newer bodies, focus frame data) followed
//! by the JPEG itself. We use the header when it is consistent and fall back
//! to scanning for the SOI / EOI markers otherwise.

/// Returns the JPEG slice inside a live view object.
pub fn extract_jpeg(data: &[u8]) -> Option<&[u8]> {
    if let Some(j) = from_header(data) {
        return Some(j);
    }
    let start = find(data, &[0xFF, 0xD8, 0xFF], 0)?;
    let end = rfind(&data[start..], &[0xFF, 0xD9]).map_or(data.len(), |e| start + e + 2);
    Some(&data[start..end])
}

fn from_header(data: &[u8]) -> Option<&[u8]> {
    if data.len() < 8 {
        return None;
    }
    let offset = u32::from_le_bytes(data[0..4].try_into().ok()?) as usize;
    let size = u32::from_le_bytes(data[4..8].try_into().ok()?) as usize;
    let end = offset.checked_add(size)?;
    if offset < 8 || size < 4 || end > data.len() {
        return None;
    }
    let jpeg = &data[offset..end];
    (jpeg.starts_with(&[0xFF, 0xD8])).then_some(jpeg)
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    hay.get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

fn rfind(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).rposition(|w| w == needle)
}

/// Reads width and height from the SOF marker of a JPEG.
pub fn jpeg_dimensions(jpeg: &[u8]) -> Option<(u16, u16)> {
    let mut i = 2;
    while i + 9 < jpeg.len() {
        if jpeg[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = jpeg[i + 1];
        if marker == 0xFF {
            i += 1;
            continue;
        }
        let len = u16::from_be_bytes([jpeg[i + 2], jpeg[i + 3]]) as usize;
        if matches!(marker, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF) {
            let h = u16::from_be_bytes([jpeg[i + 5], jpeg[i + 6]]);
            let w = u16::from_be_bytes([jpeg[i + 7], jpeg[i + 8]]);
            return Some((w, h));
        }
        i += 2 + len;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const JPEG: &[u8] = &[
        0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x11, 0x08, 0x02, 0x40, 0x04, 0x00, 0x03, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0xFF, 0xD9,
    ];

    #[test]
    fn with_header() {
        let mut data = vec![0u8; 16];
        data[0..4].copy_from_slice(&16u32.to_le_bytes());
        data[4..8].copy_from_slice(&(JPEG.len() as u32).to_le_bytes());
        data.extend_from_slice(JPEG);
        data.extend_from_slice(&[0xAA; 10]);
        assert_eq!(extract_jpeg(&data), Some(JPEG));
        assert_eq!(jpeg_dimensions(JPEG), Some((1024, 576)));
    }

    #[test]
    fn by_scanning() {
        let mut data = vec![0x11u8; 40];
        data.extend_from_slice(JPEG);
        data.extend_from_slice(&[0, 0, 0]);
        assert_eq!(extract_jpeg(&data), Some(JPEG));
        assert_eq!(extract_jpeg(&[1, 2, 3]), None);
    }
}
