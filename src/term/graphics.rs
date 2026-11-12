//! Kitty graphics protocol: send frames to the terminal and place them.
//!
//! Frames go as base64 PNG in chunks through the pty, the one transmission
//! every kitty-graphics terminal accepts. Faster paths (shared memory, temp
//! files, changed rectangles) are noted in docs/ARCHITECTURE.md.

/// Every frame reuses this id, so a new frame replaces the previous one.
const IMAGE_ID: u32 = 1;

/// Removes the page image from the screen.
pub const DELETE: &[u8] = b"\x1b_Ga=d,d=i,i=1,q=2\x1b\\";

/// The protocol's limit on the payload of one escape.
const CHUNK: usize = 4096;

/// Append the escapes that draw `png_base64` at the cursor, scaled to fill
/// `cols` x `rows` cells. The cursor doesn't move.
pub fn put_png(out: &mut Vec<u8>, png_base64: &str, cols: u16, rows: u16) {
    let mut chunks = png_base64.as_bytes().chunks(CHUNK).peekable();
    let mut first = true;
    while let Some(chunk) = chunks.next() {
        let more = u8::from(chunks.peek().is_some());
        if first {
            let keys = format!("a=T,f=100,i={IMAGE_ID},p=1,q=2,C=1,c={cols},r={rows},m={more}");
            out.extend_from_slice(b"\x1b_G");
            out.extend_from_slice(keys.as_bytes());
            first = false;
        } else {
            out.extend_from_slice(format!("\x1b_Gm={more}").as_bytes());
        }
        out.push(b';');
        out.extend_from_slice(chunk);
        out.extend_from_slice(b"\x1b\\");
    }
}

/// Width and height of a base64 PNG, read from its header. For diagnostics.
pub fn png_size(png_base64: &str) -> Option<(u32, u32)> {
    // The size sits in bytes 16..24, inside the first 32 base64 characters.
    let header = crate::base64::decode(png_base64.get(..32)?)?;
    let field = |at: usize| u32::from_be_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]]);
    Some((field(16), field(20)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_png_size() {
        // The first 24 bytes of a 1000x800 PNG.
        assert_eq!(png_size("iVBORw0KGgoAAAANSUhEUgAAA+gAAAMg"), Some((1000, 800)));
        assert_eq!(png_size("short"), None);
    }

    fn put(data: &str) -> String {
        let mut out = Vec::new();
        put_png(&mut out, data, 80, 23);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn small_image_is_one_escape() {
        assert_eq!(put("QUJD"), "\x1b_Ga=T,f=100,i=1,p=1,q=2,C=1,c=80,r=23,m=0;QUJD\x1b\\");
    }

    #[test]
    fn large_image_is_chunked() {
        let data = "A".repeat(CHUNK * 2 + 8);
        let out = put(&data);
        let escapes: Vec<&str> = out.split_terminator("\x1b\\").collect();
        assert_eq!(escapes.len(), 3);
        assert!(escapes[0].starts_with("\x1b_Ga=T,") && escapes[0].contains(",m=1;"));
        assert_eq!(escapes[1], format!("\x1b_Gm=1;{}", "A".repeat(CHUNK)));
        assert_eq!(escapes[2], "\x1b_Gm=0;AAAAAAAA");
    }

    #[test]
    fn empty_image_sends_nothing() {
        assert_eq!(put(""), "");
    }
}
