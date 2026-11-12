//! Base64 decoding, for the PNGs Chrome sends.

/// Decode standard base64. Padding is optional; anything else that isn't in
/// the alphabet is an error.
pub fn decode(text: &str) -> Option<Vec<u8>> {
    let text = text.trim_end_matches('=').as_bytes();
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    for quad in text.chunks(4) {
        let mut bits = 0u32;
        for c in quad {
            let value = match c {
                b'A'..=b'Z' => c - b'A',
                b'a'..=b'z' => c - b'a' + 26,
                b'0'..=b'9' => c - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                _ => return None,
            };
            bits = bits << 6 | value as u32;
        }
        // 2, 3 or 4 characters carry 1, 2 or 3 bytes.
        let bytes = match quad.len() {
            4 => 3,
            3 => 2,
            2 => 1,
            _ => return None,
        };
        bits <<= 6 * (4 - quad.len());
        out.extend_from_slice(&bits.to_be_bytes()[1..1 + bytes]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes() {
        assert_eq!(decode("").unwrap(), b"");
        assert_eq!(decode("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(decode("aGVsbG8h").unwrap(), b"hello!");
        assert_eq!(decode("aGk").unwrap(), b"hi");
        assert_eq!(decode("/+8=").unwrap(), [0xff, 0xef]);
        assert_eq!(decode("a"), None);
        assert_eq!(decode("a b="), None);
    }
}
