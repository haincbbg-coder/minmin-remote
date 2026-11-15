//! mm-codec — nén khung hình theo vùng thay đổi (dirty-rect tiling).
//!
//! Màn hình được chia thành các ô vuông [`TILE`] px. Mỗi khung, encoder chỉ gửi
//! những ô KHÁC so với khung trước (so sánh byte). Màn hình đứng yên ⇒ gần như
//! không tốn băng thông; chỉ vùng chuyển động mới được gửi. Đây là bước giúp
//! điều khiển mượt mà hơn hẳn so với gửi cả khung JPEG mỗi lần.
//!
//! Khung đầu (hoặc khi đổi kích thước) là keyframe: gửi mọi ô.
//! Mỗi ô nén JPEG độc lập. Về sau có thể thay JPEG-mỗi-ô bằng H.264 (P2).

use mm_capture::RgbaFrame;
use serde::{Deserialize, Serialize};

/// Cạnh một ô (pixel).
pub const TILE: u32 = 64;

/// Một ô đã nén.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tile {
    /// Chỉ số ô theo cột, hàng (toạ độ pixel = tx*TILE, ty*TILE).
    pub tx: u32,
    pub ty: u32,
    pub w: u32,
    pub h: u32,
    /// JPEG của ô.
    pub jpeg: Vec<u8>,
}

/// Một khung đã nén theo tile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncodedFrame {
    pub width: u32,
    pub height: u32,
    /// true nếu là keyframe (chứa toàn bộ ô).
    pub keyframe: bool,
    /// Các ô thay đổi (keyframe: toàn bộ ô).
    pub tiles: Vec<Tile>,
}

impl EncodedFrame {
    /// Tổng số byte JPEG trong khung (để đo băng thông).
    pub fn payload_bytes(&self) -> usize {
        self.tiles.iter().map(|t| t.jpeg.len()).sum()
    }
}

fn tiles_across(n: u32) -> u32 {
    (n + TILE - 1) / TILE
}

/// Trích một ô RGBA liền mạch từ khung lớn.
fn extract_tile(data: &[u8], w: u32, h: u32, tx: u32, ty: u32) -> (u32, u32, Vec<u8>) {
    let x0 = tx * TILE;
    let y0 = ty * TILE;
    let tw = TILE.min(w - x0);
    let th = TILE.min(h - y0);
    let mut out = Vec::with_capacity((tw * th * 4) as usize);
    for y in 0..th {
        let row = ((y0 + y) * w + x0) as usize * 4;
        out.extend_from_slice(&data[row..row + (tw * 4) as usize]);
    }
    (tw, th, out)
}

fn jpeg_encode(rgba: &[u8], w: u32, h: u32, quality: u8) -> anyhow::Result<Vec<u8>> {
    use image::codecs::jpeg::JpegEncoder;
    use image::{ExtendedColorType, ImageEncoder};
    let rgb: Vec<u8> = rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, quality)
        .write_image(&rgb, w, h, ExtendedColorType::Rgb8)?;
    Ok(out)
}

/// Encoder giữ khung trước để tìm ô thay đổi.
pub struct Encoder {
    prev: Option<Vec<u8>>,
    pw: u32,
    ph: u32,
    quality: u8,
}

impl Encoder {
    pub fn new(quality: u8) -> Self {
        Self { prev: None, pw: 0, ph: 0, quality }
    }

    /// Nén một khung. Chỉ các ô khác khung trước được đưa vào kết quả.
    pub fn encode(&mut self, frame: &RgbaFrame) -> anyhow::Result<EncodedFrame> {
        let (w, h) = (frame.width, frame.height);
        let keyframe = self.prev.is_none() || self.pw != w || self.ph != h;
        let prev = self.prev.take();
        let mut tiles = Vec::new();

        for ty in 0..tiles_across(h) {
            for tx in 0..tiles_across(w) {
                let (tw, th, cur_tile) = extract_tile(&frame.data, w, h, tx, ty);
                let changed = if keyframe {
                    true
                } else {
                    let prev = prev.as_ref().unwrap();
                    let (_, _, prev_tile) = extract_tile(prev, w, h, tx, ty);
                    cur_tile != prev_tile
                };
                if changed {
                    let jpeg = jpeg_encode(&cur_tile, tw, th, self.quality)?;
                    tiles.push(Tile { tx, ty, w: tw, h: th, jpeg });
                }
            }
        }

        self.prev = Some(frame.data.clone());
        self.pw = w;
        self.ph = h;
        Ok(EncodedFrame { width: w, height: h, keyframe, tiles })
    }
}

/// Decoder giữ framebuffer và cập nhật các ô thay đổi.
pub struct Decoder {
    buf: Vec<u8>,
    w: u32,
    h: u32,
}

impl Decoder {
    pub fn new() -> Self {
        Self { buf: Vec::new(), w: 0, h: 0 }
    }

    /// Áp một khung đã nén; trả về tham chiếu tới framebuffer RGBA đầy đủ.
    pub fn apply(&mut self, frame: &EncodedFrame) -> anyhow::Result<&[u8]> {
        if frame.keyframe || self.w != frame.width || self.h != frame.height {
            self.w = frame.width;
            self.h = frame.height;
            self.buf = vec![0u8; (self.w * self.h * 4) as usize];
        }
        for tile in &frame.tiles {
            let img = image::load_from_memory(&tile.jpeg)?.to_rgb8();
            let x0 = tile.tx * TILE;
            let y0 = tile.ty * TILE;
            for y in 0..tile.h {
                for x in 0..tile.w {
                    let src = img.get_pixel(x, y);
                    let dst = (((y0 + y) * self.w + x0 + x) * 4) as usize;
                    self.buf[dst] = src[0];
                    self.buf[dst + 1] = src[1];
                    self.buf[dst + 2] = src[2];
                    self.buf[dst + 3] = 255;
                }
            }
        }
        Ok(&self.buf)
    }

    pub fn size(&self) -> (u32, u32) {
        (self.w, self.h)
    }

    pub fn framebuffer(&self) -> &[u8] {
        &self.buf
    }
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mm_capture::{MockDesktop, ScreenCapture};

    #[test]
    fn keyframe_then_small_delta() {
        let mut cap = MockDesktop::new(640, 480);
        let mut enc = Encoder::new(75);
        let mut dec = Decoder::new();

        // Khung 1: keyframe, nhiều ô.
        let f1 = cap.grab(0).unwrap().unwrap();
        let e1 = enc.encode(&f1).unwrap();
        assert!(e1.keyframe);
        let key_tiles = e1.tiles.len();
        assert!(key_tiles > 10);
        dec.apply(&e1).unwrap();
        assert_eq!(dec.size(), (640, 480));

        // Khung 2: chỉ vùng nhỏ đổi ⇒ ít ô hơn nhiều.
        let f2 = cap.grab(0).unwrap().unwrap();
        let e2 = enc.encode(&f2).unwrap();
        assert!(!e2.keyframe);
        assert!(
            e2.tiles.len() < key_tiles / 2,
            "delta {} ô, keyframe {} ô — lẽ ra ít hơn nhiều",
            e2.tiles.len(),
            key_tiles
        );
        dec.apply(&e2).unwrap();
    }
}
