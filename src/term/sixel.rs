//! Sixel frames use a palette, not Kitty's stored images and placements.

use super::graphics::Pixels;
use std::fmt::Write;

fn color(index: usize) -> [u8; 3] {
    if index >= 216 {
        [((index - 216) * 255 / 39) as u8; 3]
    } else {
        [(index / 36 * 51) as u8, (index / 6 % 6 * 51) as u8, (index % 6 * 51) as u8]
    }
}

fn quantize(rgb: [u8; 3]) -> usize {
    let min = *rgb.iter().min().unwrap();
    let max = *rgb.iter().max().unwrap();
    if max - min <= 12 {
        let gray = (rgb.iter().map(|v| *v as usize).sum::<usize>() + 1) / 3;
        216 + (gray * 39 + 127) / 255
    } else {
        let level = |v: u8| (v as usize + 25) / 51;
        level(rgb[0]) * 36 + level(rgb[1]) * 6 + level(rgb[2])
    }
}

fn run(out: &mut String, value: u8, count: usize) {
    let character = (value + b'?') as char;
    if count > 3 {
        let _ = write!(out, "!{count}{character}");
    } else {
        out.extend(std::iter::repeat_n(character, count));
    }
}

fn channels(pixels: &Pixels) -> Result<usize, String> {
    let channels = if pixels.alpha { 4 } else { 3 };
    let source_len = (pixels.width as usize)
        .checked_mul(pixels.height as usize)
        .and_then(|n| n.checked_mul(channels))
        .ok_or("the frame is too large")?;
    if source_len != pixels.data.len() || pixels.width == 0 || pixels.height == 0 {
        return Err("invalid frame dimensions for Sixel".to_owned());
    }
    Ok(channels)
}

pub fn encode(pixels: &Pixels, width: u32, height: u32) -> Result<Vec<u8>, String> {
    let channels = channels(pixels)?;
    let count = (width as usize).checked_mul(height as usize).ok_or("the viewport is too large")?;
    if width == 0 || height == 0 || width > 16_384 || height > 16_384 || count > 64_000_000 {
        return Err("invalid frame or viewport dimensions for Sixel".to_owned());
    }
    let mut indices = Vec::with_capacity(count);
    let mut used = [false; 256];
    for y in 0..height {
        let sy = y as u64 * pixels.height as u64 / height as u64;
        for x in 0..width {
            let sx = x as u64 * pixels.width as u64 / width as u64;
            let at = (sy as usize * pixels.width as usize + sx as usize) * channels;
            let alpha = if pixels.alpha { pixels.data[at + 3] as u32 } else { 255 };
            let rgb = std::array::from_fn(|c| (pixels.data[at + c] as u32 * alpha / 255) as u8);
            let index = quantize(rgb);
            used[index] = true;
            indices.push(index as u8);
        }
    }
    let mut out = format!("\x1bP0;1q\"1;1;{width};{height}");
    for (index, _) in used.into_iter().enumerate().filter(|(_, used)| *used) {
        let [r, g, b] = color(index).map(|c| (c as u32 * 100 + 127) / 255);
        let _ = write!(out, "#{index};2;{r};{g};{b}");
    }
    let width = width as usize;
    let mut bands = vec![0u8; width * 256];
    for y in (0..height as usize).step_by(6) {
        bands.fill(0);
        let mut used = [false; 256];
        for dy in 0..6.min(height as usize - y) {
            for x in 0..width {
                let index = indices[(y + dy) * width + x] as usize;
                used[index] = true;
                bands[index * width + x] |= 1 << dy;
            }
        }
        let mut first = true;
        for index in (0..256).filter(|index| used[*index]) {
            if !first {
                out.push('$');
            }
            first = false;
            let _ = write!(out, "#{index}");
            let row = &bands[index * width..(index + 1) * width];
            let end = row.iter().rposition(|byte| *byte != 0).map_or(0, |x| x + 1);
            let mut x = 0;
            while x < end {
                let count = row[x..end].iter().take_while(|byte| **byte == row[x]).count();
                run(&mut out, row[x], count);
                x += count;
            }
        }
        if y + 6 < height as usize {
            out.push('-');
        }
    }
    out.push_str("\x1b\\");
    Ok(out.into_bytes())
}

pub fn scale(pixels: &Pixels, width: u32, height: u32) -> Result<Pixels, String> {
    let count = (width as usize).checked_mul(height as usize).ok_or("the viewport is too large")?;
    let channels = channels(pixels)?;
    if width == 0 || height == 0 || width > 16_384 || height > 16_384 || count > 64_000_000 {
        return Err("invalid frame or viewport dimensions for Sixel".to_owned());
    }
    let mut data = Vec::with_capacity(count * 3);
    for y in 0..height {
        let sy = y as u64 * pixels.height as u64 / height as u64;
        for x in 0..width {
            let sx = x as u64 * pixels.width as u64 / width as u64;
            let at = (sy as usize * pixels.width as usize + sx as usize) * channels;
            let alpha = if pixels.alpha { pixels.data[at + 3] as u32 } else { 255 };
            for c in 0..3 {
                data.push((pixels.data[at + c] as u32 * alpha / 255) as u8);
            }
        }
    }
    Ok(Pixels { width, height, alpha: false, data })
}

pub fn overlay(canvas: &mut Pixels, image: &Pixels, x: u32, y: u32, opacity: f64) {
    let channels = if image.alpha { 4 } else { 3 };
    for sy in 0..image.height.min(canvas.height.saturating_sub(y)) {
        for sx in 0..image.width.min(canvas.width.saturating_sub(x)) {
            let from = (sy as usize * image.width as usize + sx as usize) * channels;
            let to = ((y + sy) as usize * canvas.width as usize + (x + sx) as usize) * 3;
            let alpha = (if image.alpha { image.data[from + 3] as f64 / 255.0 } else { 1.0 }) * opacity.clamp(0.0, 1.0);
            for c in 0..3 {
                canvas.data[to + c] =
                    (image.data[from + c] as f64 * alpha + canvas.data[to + c] as f64 * (1.0 - alpha)).round() as u8;
            }
        }
    }
}

pub fn draw(out: &mut Vec<u8>, pixels: &Pixels, top: u32) -> Result<(), String> {
    // Display mode clamps at the bottom but always starts at the screen origin.
    // Include the rows above the page; the address bar is painted over them next.
    let height = pixels.height.checked_add(top).ok_or("the viewport is too large")?;
    let count = (pixels.width as usize).checked_mul(height as usize).ok_or("the viewport is too large")?;
    if count == 0 || pixels.width > 16_384 || height > 16_384 || count > 64_000_000 {
        return Err("invalid viewport dimensions for Sixel".to_owned());
    }
    channels(pixels)?;
    let mut canvas = Pixels { width: pixels.width, height, alpha: false, data: vec![0; count * 3] };
    overlay(&mut canvas, pixels, 0, top, 1.0);
    let image = encode(&canvas, canvas.width, canvas.height)?;
    out.extend_from_slice(b"\x1b7\x1b[1;1H\x1b[?80h");
    out.extend_from_slice(&image);
    out.extend_from_slice(b"\x1b8");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_partial_bands_and_runs() {
        let pixels = Pixels { width: 4, height: 7, alpha: false, data: vec![255; 4 * 7 * 3] };
        let out = String::from_utf8(encode(&pixels, 4, 7).unwrap()).unwrap();
        assert!(out.contains("\"1;1;4;7"));
        assert!(out.contains("#255!4~-#255!4@"), "{out:?}");
        assert!(out.ends_with("\x1b\\"));
    }

    #[test]
    fn scales_and_composites_alpha() {
        let pixels = Pixels { width: 2, height: 1, alpha: true, data: vec![255, 0, 0, 255, 255, 255, 255, 0] };
        let out = String::from_utf8(encode(&pixels, 4, 1).unwrap()).unwrap();
        assert!(out.contains("#180;2;100;0;0"));
        assert!(out.contains("#216;2;0;0;0"));
        assert!(out.contains("#180@@"));
        assert!(out.contains("#216??@@"));
    }

    #[test]
    fn rejects_bad_frames_before_writing() {
        let pixels = Pixels { width: 1, height: 1, alpha: false, data: vec![] };
        assert!(encode(&pixels, 1, 1).is_err());
        assert!(encode(&pixels, 0, 1).is_err());
        let mut out = vec![];
        assert!(draw(&mut out, &pixels, 0).is_err());
        assert!(draw(&mut out, &pixels, u32::MAX).is_err());
        assert!(out.is_empty());
        let huge = Pixels { width: u32::MAX, height: u32::MAX, alpha: true, data: vec![] };
        assert!(scale(&huge, 1, 1).is_err());
    }

    #[test]
    fn composited_cursor_is_clipped_without_changing_the_frame() {
        let source = Pixels { width: 2, height: 2, alpha: false, data: vec![0; 12] };
        let mut canvas = scale(&source, 2, 2).unwrap();
        let image = Pixels { width: 2, height: 2, alpha: true, data: vec![255; 16] };
        overlay(&mut canvas, &image, 1, 1, 0.5);
        assert_eq!(&canvas.data[9..12], &[128; 3]);
        assert_eq!(source.data, vec![0; 12]);
        assert_eq!(&canvas.data[..9], &[0; 9]);
    }
}
