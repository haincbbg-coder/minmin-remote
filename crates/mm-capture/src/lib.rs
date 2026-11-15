//! mm-capture — chụp màn hình máy bị điều khiển.
//!
//! Định nghĩa trait [`ScreenCapture`] để phần còn lại của hệ thống không phụ
//! thuộc nền tảng. Có hai bản cài:
//! - [`MockCapture`]: tạo khung hình giả (chạy mọi hệ điều hành) — dùng để
//!   kiểm thử trọn luồng truyền hình trên máy chủ Linux trước khi có Windows.
//! - `dxgi::DxgiCapture` (chỉ Windows): chụp thật bằng Desktop Duplication API.
//!   Hiện là khung sườn, hoàn thiện khi build trên máy Windows của bố.

use mm_proto::MonitorInfo;

/// Một khung hình thô dạng RGBA (4 byte mỗi điểm ảnh).
pub struct RgbaFrame {
    pub width: u32,
    pub height: u32,
    pub monitor: u32,
    /// Dữ liệu RGBA, dài = width * height * 4.
    pub data: Vec<u8>,
}

/// Nguồn chụp màn hình.
pub trait ScreenCapture: Send {
    /// Danh sách màn hình hiện có.
    fn monitors(&self) -> Vec<MonitorInfo>;
    /// Chụp một khung của màn hình `monitor`. Trả về None nếu không có khung mới.
    fn grab(&mut self, monitor: u32) -> anyhow::Result<Option<RgbaFrame>>;
}

/// Nén một khung RGBA thành JPEG với chất lượng cho trước (1..=100).
pub fn encode_jpeg(frame: &RgbaFrame, quality: u8) -> anyhow::Result<Vec<u8>> {
    use image::codecs::jpeg::JpegEncoder;
    use image::{ExtendedColorType, ImageEncoder};
    let mut out = Vec::new();
    let encoder = JpegEncoder::new_with_quality(&mut out, quality);
    // JPEG không có kênh alpha nên chuyển RGBA -> RGB.
    let rgb: Vec<u8> = frame
        .data
        .chunks_exact(4)
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect();
    encoder.write_image(&rgb, frame.width, frame.height, ExtendedColorType::Rgb8)?;
    Ok(out)
}

/// Nguồn chụp giả: vẽ một dải màu chuyển động theo số khung để nhìn thấy được
/// chuyển động khi xem. Dùng kiểm thử P1 mà không cần màn hình thật.
pub struct MockCapture {
    width: u32,
    height: u32,
    frame_no: u64,
}

impl MockCapture {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height, frame_no: 0 }
    }
}

impl ScreenCapture for MockCapture {
    fn monitors(&self) -> Vec<MonitorInfo> {
        vec![MonitorInfo {
            index: 0,
            width: self.width,
            height: self.height,
            is_primary: true,
        }]
    }

    fn grab(&mut self, _monitor: u32) -> anyhow::Result<Option<RgbaFrame>> {
        let (w, h) = (self.width, self.height);
        let t = self.frame_no;
        let mut data = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let idx = ((y * w + x) * 4) as usize;
                // Dải màu dịch chuyển theo thời gian + một ô vuông sáng chạy chéo.
                let r = ((x + t as u32) % 256) as u8;
                let g = ((y + t as u32 * 2) % 256) as u8;
                let b = ((x + y + t as u32 * 3) % 256) as u8;
                let bx = (t * 7 % w as u64) as u32;
                let by = (t * 5 % h as u64) as u32;
                let near = x.abs_diff(bx) < 40 && y.abs_diff(by) < 40;
                data[idx] = if near { 255 } else { r };
                data[idx + 1] = if near { 255 } else { g };
                data[idx + 2] = if near { 255 } else { b };
                data[idx + 3] = 255;
            }
        }
        self.frame_no += 1;
        Ok(Some(RgbaFrame { width: w, height: h, monitor: 0, data }))
    }
}

/// Nguồn giả giống màn hình thật: nền tĩnh, chỉ một ô "con trỏ" di chuyển và một
/// dải "đồng hồ" nhỏ đổi mỗi khung. Dùng để đo lợi ích của codec dirty-rect
/// (phần lớn màn hình đứng yên, như lúc dùng máy thật).
pub struct MockDesktop {
    width: u32,
    height: u32,
    frame_no: u64,
    background: Vec<u8>,
}

impl MockDesktop {
    pub fn new(width: u32, height: u32) -> Self {
        // Nền tĩnh: dải màu dịu, tính một lần.
        let mut bg = vec![0u8; (width * height * 4) as usize];
        for y in 0..height {
            for x in 0..width {
                let i = ((y * width + x) * 4) as usize;
                bg[i] = (30 + x * 60 / width.max(1)) as u8;
                bg[i + 1] = (30 + y * 60 / height.max(1)) as u8;
                bg[i + 2] = 60;
                bg[i + 3] = 255;
            }
        }
        Self { width, height, frame_no: 0, background: bg }
    }
}

impl ScreenCapture for MockDesktop {
    fn monitors(&self) -> Vec<MonitorInfo> {
        vec![MonitorInfo { index: 0, width: self.width, height: self.height, is_primary: true }]
    }

    fn grab(&mut self, _monitor: u32) -> anyhow::Result<Option<RgbaFrame>> {
        let (w, h) = (self.width, self.height);
        let t = self.frame_no;
        let mut data = self.background.clone();

        // Con trỏ 24x24 chạy vòng.
        let cx = (t * 11 % w as u64) as u32;
        let cy = (t * 7 % h as u64) as u32;
        for dy in 0..24u32 {
            for dx in 0..24u32 {
                let (x, y) = (cx.saturating_add(dx), cy.saturating_add(dy));
                if x < w && y < h {
                    let i = ((y * w + x) * 4) as usize;
                    data[i] = 255;
                    data[i + 1] = 255;
                    data[i + 2] = 0;
                }
            }
        }
        // Dải "đồng hồ" 160x28 ở góc trên trái đổi mỗi khung.
        for dy in 0..28u32 {
            for dx in 0..160u32 {
                if dx < w && dy < h {
                    let i = ((dy * w + dx) * 4) as usize;
                    let v = ((dx as u64 + t * 9) % 256) as u8;
                    data[i] = v;
                    data[i + 1] = 200;
                    data[i + 2] = v;
                }
            }
        }

        self.frame_no += 1;
        Ok(Some(RgbaFrame { width: w, height: h, monitor: 0, data }))
    }
}

#[cfg(windows)]
pub mod dxgi;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_grabs_and_encodes() {
        let mut cap = MockCapture::new(320, 240);
        assert_eq!(cap.monitors().len(), 1);
        let frame = cap.grab(0).unwrap().unwrap();
        assert_eq!(frame.data.len(), 320 * 240 * 4);
        let jpeg = encode_jpeg(&frame, 80).unwrap();
        // JPEG hợp lệ bắt đầu bằng 0xFFD8.
        assert_eq!(&jpeg[0..2], &[0xFF, 0xD8]);
    }
}
