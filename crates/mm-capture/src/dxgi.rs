//! Chụp màn hình thật trên Windows bằng Desktop Duplication API (DXGI).
//!
//! KHUNG SƯỜN — hoàn thiện khi build trên máy Windows. Trên Linux file này
//! không được biên dịch (đã `cfg(windows)`). Luồng chuẩn của Desktop
//! Duplication:
//!   1. D3D11CreateDevice -> ID3D11Device + context.
//!   2. Từ device lấy IDXGIDevice -> IDXGIAdapter -> liệt kê IDXGIOutput.
//!   3. Mỗi output: QueryInterface IDXGIOutput1 -> DuplicateOutput ->
//!      IDXGIOutputDuplication.
//!   4. Mỗi khung: AcquireNextFrame -> map texture GPU sang CPU -> copy RGBA.
//!      API chỉ trả về VÙNG THAY ĐỔI nên rất tiết kiệm băng thông (khớp mục
//!      "nén ảnh cho mạng chậm" của thiết kế).
//!   5. Chạy trong tiến trình agent ở phiên người dùng; service dưới SYSTEM
//!      lo phần phiên đăng nhập/UAC.

use crate::{RgbaFrame, ScreenCapture};
use mm_proto::MonitorInfo;

pub struct DxgiCapture {
    // TODO(P1-Windows): giữ ID3D11Device, ID3D11DeviceContext,
    // Vec<IDXGIOutputDuplication> cho từng màn hình.
    monitors: Vec<MonitorInfo>,
}

impl DxgiCapture {
    pub fn new() -> anyhow::Result<Self> {
        // TODO(P1-Windows): khởi tạo D3D11 + Desktop Duplication như mô tả trên.
        // Tạm liệt kê 1 màn hình để Server chạy được khung sườn.
        Ok(Self {
            monitors: vec![MonitorInfo {
                index: 0,
                width: 1920,
                height: 1080,
                is_primary: true,
            }],
        })
    }
}

impl ScreenCapture for DxgiCapture {
    fn monitors(&self) -> Vec<MonitorInfo> {
        self.monitors.clone()
    }

    fn grab(&mut self, _monitor: u32) -> anyhow::Result<Option<RgbaFrame>> {
        // TODO(P1-Windows): AcquireNextFrame + map + copy RGBA.
        anyhow::bail!("DxgiCapture.grab chưa cài — hoàn thiện khi build trên Windows")
    }
}
