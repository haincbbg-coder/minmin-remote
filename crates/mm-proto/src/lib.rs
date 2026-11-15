//! mm-proto — định nghĩa gói tin (wire protocol) cho MinMin Remote.
//!
//! Mọi gói tin được mã hoá bằng bincode rồi gửi qua kênh Noise (mm-crypto)
//! bên trong khung length-prefix của mm-transport. Trường `PROTOCOL_VERSION`
//! cho phép Viewer và Server từ chối nếu khác phiên bản giao thức.

use serde::{Deserialize, Serialize};

/// Phiên bản giao thức. Tăng khi thay đổi cấu trúc gói tin không tương thích.
pub const PROTOCOL_VERSION: u16 = 1;

/// Cổng mặc định cho Server (giống Radmin dùng 4899).
pub const DEFAULT_PORT: u16 = 4899;

/// Gói tin Viewer -> Server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMessage {
    /// Lời chào đầu tiên, gửi ngay sau khi kênh Noise thiết lập xong.
    Hello {
        protocol_version: u16,
        /// Tên hiển thị của người điều khiển (để ghi nhật ký trên Server).
        viewer_name: String,
    },
    /// Chọn chế độ làm việc. P1 chỉ hỗ trợ ViewOnly.
    SetMode(SessionMode),
    /// Sự kiện chuột/bàn phím (chỉ áp dụng ở chế độ FullControl).
    Input(InputEvent),
    /// Yêu cầu Server gửi một khung hình đầy đủ (keyframe) ngay.
    RequestFullFrame,
    /// Báo cho Server biết Viewer đã nhận khung tới số thứ tự này (điều tiết tốc độ).
    Ack { frame_seq: u64 },
    /// Chọn màn hình cần xem khi Server có nhiều màn hình (P2).
    SelectMonitor { index: u32 },
    /// Ngắt kết nối một cách lịch sự.
    Bye,
}

/// Gói tin Server -> Viewer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMessage {
    /// Trả lời Hello: thông tin máy và danh sách màn hình.
    Welcome {
        protocol_version: u16,
        host_name: String,
        os: String,
        monitors: Vec<MonitorInfo>,
    },
    /// Header của một khung hình, đi ngay trước FrameData.
    FrameHeader(FrameHeader),
    /// Dữ liệu ảnh đã nén của một khung (hoặc một vùng thay đổi).
    FrameData {
        seq: u64,
        /// Dữ liệu ảnh đã nén theo `FrameHeader.codec`.
        bytes: Vec<u8>,
    },
    /// Thông báo lỗi dạng chữ (hiển thị cho người điều khiển).
    Error { message: String },
    /// Server chủ động đóng phiên.
    Closed { reason: String },
}

/// Nút chuột.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// Sự kiện nhập liệu từ người điều khiển. Toạ độ chuột chuẩn hoá 0.0..1.0 theo
/// màn hình đang xem, để không phụ thuộc độ phân giải hai máy.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum InputEvent {
    /// Di chuyển chuột tới toạ độ chuẩn hoá (x, y).
    MouseMove { x: f32, y: f32 },
    /// Nhấn giữ một nút chuột tại (x, y).
    MouseDown { button: MouseButton, x: f32, y: f32 },
    /// Nhả một nút chuột tại (x, y).
    MouseUp { button: MouseButton, x: f32, y: f32 },
    /// Lăn chuột; delta > 0 là lăn lên.
    MouseWheel { delta: i32 },
    /// Nhấn phím (mã phím ảo Windows VK_*).
    KeyDown { vk: u32 },
    /// Nhả phím.
    KeyUp { vk: u32 },
}

/// Chế độ phiên làm việc, ánh xạ sang các chế độ của Radmin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionMode {
    /// Chỉ xem màn hình (P1).
    ViewOnly,
    /// Điều khiển toàn quyền: chuột + bàn phím (P2).
    FullControl,
    /// Truyền file (P3).
    FileTransfer,
    /// Dòng lệnh từ xa (P3/P4).
    Shell,
}

/// Thông tin một màn hình của máy bị điều khiển.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorInfo {
    pub index: u32,
    pub width: u32,
    pub height: u32,
    pub is_primary: bool,
}

/// Cách nén ảnh khung hình.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FrameCodec {
    /// Ảnh thô RGBA (chỉ dùng để gỡ lỗi, rất nặng).
    RawRgba,
    /// JPEG cả khung — bản P1 ban đầu.
    Jpeg,
    /// Dirty-rect tiling (mm-codec) — chỉ gửi ô thay đổi. Bản v2.
    Tiled,
    /// H.264 — thêm ở P2 cho mạng chậm.
    H264,
}

/// Header mô tả một khung hình.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameHeader {
    pub seq: u64,
    pub monitor: u32,
    pub width: u32,
    pub height: u32,
    pub codec: FrameCodec,
    /// true nếu đây là khung đầy đủ; false nếu chỉ là vùng thay đổi (P2).
    pub keyframe: bool,
}

/// Lỗi khi mã hoá/giải mã gói tin.
#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    #[error("lỗi bincode: {0}")]
    Bincode(#[from] bincode::Error),
}

/// Mã hoá một gói tin thành bytes.
pub fn encode<T: Serialize>(msg: &T) -> Result<Vec<u8>, ProtoError> {
    Ok(bincode::serialize(msg)?)
}

/// Giải mã bytes thành một gói tin.
pub fn decode<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> Result<T, ProtoError> {
    Ok(bincode::deserialize(bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_client_message() {
        let msg = ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            viewer_name: "bố".into(),
        };
        let bytes = encode(&msg).unwrap();
        let back: ClientMessage = decode(&bytes).unwrap();
        matches!(back, ClientMessage::Hello { .. });
    }

    #[test]
    fn roundtrip_frame() {
        let header = FrameHeader {
            seq: 42,
            monitor: 0,
            width: 1920,
            height: 1080,
            codec: FrameCodec::Jpeg,
            keyframe: true,
        };
        let bytes = encode(&ServerMessage::FrameHeader(header)).unwrap();
        let back: ServerMessage = decode(&bytes).unwrap();
        if let ServerMessage::FrameHeader(h) = back {
            assert_eq!(h.seq, 42);
            assert_eq!(h.width, 1920);
        } else {
            panic!("sai loại gói tin");
        }
    }
}
