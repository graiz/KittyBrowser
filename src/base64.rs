//! Base64, for the frames Chrome sends and the file names the terminal is given.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// The value of each byte as a base64 digit, or `INVALID`, which has the
/// top bit set where no digit does.
const INVALID: u8 = 0xff;
const VALUES: [u8; 256] = {
    let mut values = [INVALID; 256];
    let mut i = 0;
    while i < 64 {
        values[ALPHABET[i] as usize] = i as u8;
        i += 1;
    }
    values
};

/// Decode standard base64. Padding is optional; anything else that isn't in
/// the alphabet is an error.
pub fn decode(text: &str) -> Option<Vec<u8>> {
    let text = text.trim_end_matches('=').as_bytes();
    let mut out = Vec::with_capacity(text.len() * 3 / 4 + 3);
    // A frame is a megabyte of this, so whole groups go through the table
    // with one check for a bad digit each.
    let whole = text.len() / 4 * 4;
    for quad in text[..whole].chunks_exact(4) {
        let [a, b, c, d] = [quad[0], quad[1], quad[2], quad[3]].map(|c| VALUES[c as usize]);
        if (a | b | c | d) & 0x80 != 0 {
            return None;
        }
        let bits = (a as u32) << 18 | (b as u32) << 12 | (c as u32) << 6 | d as u32;
        out.extend_from_slice(&bits.to_be_bytes()[1..]);
    }
    // 2 or 3 characters at the end carry 1 or 2 bytes.
    let rest = &text[whole..];
    let bytes = match rest.len() {
        0 => return Some(out),
        3 => 2,
        2 => 1,
        _ => return None,
    };
    let mut bits = 0u32;
    for &c in rest {
        let value = VALUES[c as usize];
        if value == INVALID {
            return None;
        }
        bits = bits << 6 | value as u32;
    }
    bits <<= 6 * (4 - rest.len());
    out.extend_from_slice(&bits.to_be_bytes()[1..1 + bytes]);
    Some(out)
}

/// Encode as standard base64, padded.
pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for triple in bytes.chunks(3) {
        let mut bits = 0u32;
        for (i, byte) in triple.iter().enumerate() {
            bits |= (*byte as u32) << (16 - 8 * i);
        }
        for i in 0..4 {
            if i <= triple.len() {
                out.push(ALPHABET[(bits >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes() {
        assert_eq!(encode(b""), "");
        assert_eq!(encode(b"hello"), "aGVsbG8=");
        assert_eq!(encode(b"hello!"), "aGVsbG8h");
        assert_eq!(encode(b"hi"), "aGk=");
        assert_eq!(encode(&[0xff, 0xef]), "/+8=");
    }

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
