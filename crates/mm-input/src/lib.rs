//! mm-input — tiêm sự kiện chuột/bàn phím vào máy bị điều khiển.
//!
//! Trait [`InputInjector`] để phần còn lại không phụ thuộc nền tảng:
//! - [`MockInjector`]: ghi lại sự kiện (mọi HĐH) — dùng kiểm thử.
//! - `win::WinInjector` (Windows): tiêm thật bằng SendInput.
//!
//! Toạ độ chuột trong [`mm_proto::InputEvent`] là chuẩn hoá 0.0..1.0; trên
//! Windows ánh xạ sang 0..65535 cho SendInput ở chế độ ABSOLUTE.

use std::sync::{Arc, Mutex};

use mm_proto::InputEvent;

/// Nơi nhận sự kiện nhập liệu.
pub trait InputInjector: Send {
    fn inject(&mut self, ev: &InputEvent) -> anyhow::Result<()>;
}

/// Chuyển toạ độ chuẩn hoá 0.0..1.0 sang hệ ABSOLUTE 0..65535 của Windows.
pub fn to_absolute(x: f32, y: f32) -> (i32, i32) {
    let cx = (x.clamp(0.0, 1.0) * 65535.0).round() as i32;
    let cy = (y.clamp(0.0, 1.0) * 65535.0).round() as i32;
    (cx, cy)
}

/// Injector giả: ghi lại mọi sự kiện (dùng kiểm thử và chạy thử trên Linux).
#[derive(Clone, Default)]
pub struct MockInjector {
    pub log: Arc<Mutex<Vec<InputEvent>>>,
}

impl MockInjector {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn events(&self) -> Vec<InputEvent> {
        self.log.lock().unwrap().clone()
    }
}

impl InputInjector for MockInjector {
    fn inject(&mut self, ev: &InputEvent) -> anyhow::Result<()> {
        tracing::info!("[mock input] {:?}", ev);
        self.log.lock().unwrap().push(*ev);
        Ok(())
    }
}

/// Tạo injector phù hợp nền tảng: thật trên Windows, giả ở nơi khác.
pub fn default_injector() -> Box<dyn InputInjector> {
    #[cfg(windows)]
    {
        Box::new(win::WinInjector::new())
    }
    #[cfg(not(windows))]
    {
        Box::new(MockInjector::new())
    }
}

#[cfg(windows)]
pub mod win;

#[cfg(test)]
mod tests {
    use super::*;
    use mm_proto::{InputEvent, MouseButton};

    #[test]
    fn absolute_mapping() {
        assert_eq!(to_absolute(0.0, 0.0), (0, 0));
        assert_eq!(to_absolute(1.0, 1.0), (65535, 65535));
        assert_eq!(to_absolute(0.5, 0.5), (32768, 32768));
        // Vượt biên bị kẹp lại.
        assert_eq!(to_absolute(-1.0, 2.0), (0, 65535));
    }

    #[test]
    fn mock_records() {
        let mut inj = MockInjector::new();
        inj.inject(&InputEvent::MouseMove { x: 0.5, y: 0.5 }).unwrap();
        inj.inject(&InputEvent::MouseDown { button: MouseButton::Left, x: 0.5, y: 0.5 })
            .unwrap();
        inj.inject(&InputEvent::KeyDown { vk: 0x41 }).unwrap();
        assert_eq!(inj.events().len(), 3);
    }
}
