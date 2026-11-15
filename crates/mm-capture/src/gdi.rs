//! Chụp màn hình thật trên Windows bằng GDI (BitBlt + GetDIBits).
//!
//! Đơn giản và chạy được mọi máy Windows (không cần GPU/DXGI). Mỗi lần gọi
//! `grab` sẽ copy toàn màn hình chính ra bộ nhớ rồi trả về RGBA. DXGI (nhanh
//! hơn, chỉ gửi vùng thay đổi ở tầng GPU) để dành cho bước tối ưu sau; codec
//! dirty-rect của mm-codec đã bù phần lớn băng thông cho GDI.

use crate::{RgbaFrame, ScreenCapture};
use mm_proto::MonitorInfo;

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
    ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HGDIOBJ, SRCCOPY,
};
use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

/// Nguồn chụp màn hình thật bằng GDI. Không giữ trạng thái giữa các lần gọi.
pub struct GdiCapture;

impl GdiCapture {
    pub fn new() -> anyhow::Result<Self> {
        Ok(GdiCapture)
    }
}

impl Default for GdiCapture {
    fn default() -> Self {
        GdiCapture
    }
}

impl ScreenCapture for GdiCapture {
    fn monitors(&self) -> Vec<MonitorInfo> {
        let (w, h) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
        vec![MonitorInfo {
            index: 0,
            width: w.max(0) as u32,
            height: h.max(0) as u32,
            is_primary: true,
        }]
    }

    fn grab(&mut self, _monitor: u32) -> anyhow::Result<Option<RgbaFrame>> {
        unsafe {
            let w = GetSystemMetrics(SM_CXSCREEN);
            let h = GetSystemMetrics(SM_CYSCREEN);
            if w <= 0 || h <= 0 {
                anyhow::bail!("không đọc được kích thước màn hình");
            }

            let screen = GetDC(HWND(core::ptr::null_mut()));
            if screen.0.is_null() {
                anyhow::bail!("GetDC lỗi");
            }
            let mem = CreateCompatibleDC(screen);
            let bmp = CreateCompatibleBitmap(screen, w, h);
            let old = SelectObject(mem, HGDIOBJ(bmp.0));

            let blt = BitBlt(mem, 0, 0, w, h, screen, 0, 0, SRCCOPY);

            let mut data = vec![0u8; w as usize * h as usize * 4];
            let mut bmi: BITMAPINFO = std::mem::zeroed();
            bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bmi.bmiHeader.biWidth = w;
            bmi.bmiHeader.biHeight = -h; // âm = ảnh top-down
            bmi.bmiHeader.biPlanes = 1;
            bmi.bmiHeader.biBitCount = 32;
            bmi.bmiHeader.biCompression = BI_RGB.0;

            let got = if blt.is_ok() {
                GetDIBits(
                    mem,
                    bmp,
                    0,
                    h as u32,
                    Some(data.as_mut_ptr().cast()),
                    &mut bmi,
                    DIB_RGB_COLORS,
                )
            } else {
                0
            };

            // Dọn tài nguyên GDI dù thành công hay lỗi.
            SelectObject(mem, old);
            let _ = DeleteObject(HGDIOBJ(bmp.0));
            let _ = DeleteDC(mem);
            ReleaseDC(HWND(core::ptr::null_mut()), screen);

            blt?;
            if got == 0 {
                anyhow::bail!("GetDIBits lỗi");
            }

            // GDI trả BGRA; đổi sang RGBA (hoán B và R).
            for px in data.chunks_exact_mut(4) {
                px.swap(0, 2);
                px[3] = 255;
            }

            Ok(Some(RgbaFrame { width: w as u32, height: h as u32, monitor: 0, data }))
        }
    }
}
