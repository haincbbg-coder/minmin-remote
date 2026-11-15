//! Tiêm chuột/bàn phím thật trên Windows bằng SendInput.

use mm_proto::{InputEvent, MouseButton};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_KEYUP, MOUSEINPUT, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL, MOUSE_EVENT_FLAGS, VIRTUAL_KEY,
};

use crate::{to_absolute, InputInjector};

pub struct WinInjector;

impl WinInjector {
    pub fn new() -> Self {
        WinInjector
    }
}

impl Default for WinInjector {
    fn default() -> Self {
        Self::new()
    }
}

fn mouse(flags: MOUSE_EVENT_FLAGS, x_abs: i32, y_abs: i32, data: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: x_abs,
                dy: y_abs,
                mouseData: data as u32,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn key(vk: u32, up: bool) -> INPUT {
    let flags = if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) };
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk as u16),
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send(inputs: &[INPUT]) {
    unsafe {
        SendInput(inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

impl InputInjector for WinInjector {
    fn inject(&mut self, ev: &InputEvent) -> anyhow::Result<()> {
        match *ev {
            InputEvent::MouseMove { x, y } => {
                let (ax, ay) = to_absolute(x, y);
                send(&[mouse(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE, ax, ay, 0)]);
            }
            InputEvent::MouseDown { button, x, y } => {
                let (ax, ay) = to_absolute(x, y);
                let btn = match button {
                    MouseButton::Left => MOUSEEVENTF_LEFTDOWN,
                    MouseButton::Right => MOUSEEVENTF_RIGHTDOWN,
                    MouseButton::Middle => MOUSEEVENTF_MIDDLEDOWN,
                };
                send(&[mouse(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | btn, ax, ay, 0)]);
            }
            InputEvent::MouseUp { button, x, y } => {
                let (ax, ay) = to_absolute(x, y);
                let btn = match button {
                    MouseButton::Left => MOUSEEVENTF_LEFTUP,
                    MouseButton::Right => MOUSEEVENTF_RIGHTUP,
                    MouseButton::Middle => MOUSEEVENTF_MIDDLEUP,
                };
                send(&[mouse(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | btn, ax, ay, 0)]);
            }
            InputEvent::MouseWheel { delta } => {
                send(&[mouse(MOUSEEVENTF_WHEEL, 0, 0, delta * 120)]);
            }
            InputEvent::KeyDown { vk } => send(&[key(vk, false)]),
            InputEvent::KeyUp { vk } => send(&[key(vk, true)]),
        }
        Ok(())
    }
}
