//! mm-viewer-gui — cửa sổ điều khiển máy từ xa (egui).
//!
//! Màn hình kết nối: nhập địa chỉ máy con, cổng, mật khẩu → Kết nối.
//! Sau khi nối: hiện màn hình máy kia theo thời gian thực; khi ở chế độ Điều
//! khiển, chuột và bàn phím thật của bố được gửi sang máy kia.

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use mm_codec::{Decoder, EncodedFrame};
use mm_proto::{
    ClientMessage, FrameCodec, FrameHeader, InputEvent, MouseButton, ServerMessage, SessionMode,
    PROTOCOL_VERSION,
};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

struct FrameBuf {
    w: u32,
    h: u32,
    rgba: Vec<u8>,
}

#[derive(Default, Clone, Copy)]
struct Stats {
    fps: f32,
    kbps_frame: f32,
}

#[derive(Default)]
struct Shared {
    frame: Mutex<Option<FrameBuf>>,
    frame_id: AtomicU64,
    connected: AtomicBool,
    err: Mutex<Option<String>>,
    title: Mutex<String>,
    stats: Mutex<Stats>,
}

impl Shared {
    fn reset(&self) {
        *self.frame.lock().unwrap() = None;
        self.frame_id.store(0, Ordering::SeqCst);
        self.connected.store(false, Ordering::SeqCst);
        *self.err.lock().unwrap() = None;
        *self.title.lock().unwrap() = String::new();
        *self.stats.lock().unwrap() = Stats::default();
    }
}

enum Screen {
    Connect,
    Session,
}

struct App {
    rt: tokio::runtime::Runtime,
    shared: Arc<Shared>,
    screen: Screen,
    host: String,
    port: String,
    password: String,
    name: String,
    control_mode_full: bool,
    input_tx: Option<UnboundedSender<InputEvent>>,
    tex: Option<egui::TextureHandle>,
    last_frame_id: u64,
}

impl Default for App {
    fn default() -> Self {
        Self {
            rt: tokio::runtime::Runtime::new().expect("tạo runtime tokio"),
            shared: Arc::new(Shared::default()),
            screen: Screen::Connect,
            host: String::new(),
            port: mm_proto::DEFAULT_PORT.to_string(),
            password: String::new(),
            name: "bố".to_string(),
            control_mode_full: true,
            input_tx: None,
            tex: None,
            last_frame_id: 0,
        }
    }
}

impl App {
    fn start_connect(&mut self, ctx: &egui::Context) {
        let addr: std::net::SocketAddr = match format!("{}:{}", self.host.trim(), self.port.trim())
            .parse()
        {
            Ok(a) => a,
            Err(_) => {
                *self.shared.err.lock().unwrap() =
                    Some("Địa chỉ không hợp lệ (ví dụ 192.168.1.21)".into());
                return;
            }
        };
        self.shared.reset();
        let (tx, mut rx) = unbounded_channel::<InputEvent>();
        self.input_tx = Some(tx);
        self.screen = Screen::Session;
        self.tex = None;
        self.last_frame_id = 0;

        let shared = self.shared.clone();
        let ctx = ctx.clone();
        let password = self.password.clone();
        let name = self.name.clone();

        self.rt.spawn(async move {
            let res = async {
                let ep = mm_quic::client_endpoint()?;
                let mut sess = mm_quic::connect(&ep, addr, &password).await?;
                sess.control
                    .send(&ClientMessage::Hello {
                        protocol_version: PROTOCOL_VERSION,
                        viewer_name: name,
                    })
                    .await?;
                if let ServerMessage::Welcome { host_name, .. } =
                    sess.control.recv::<ServerMessage>().await?
                {
                    *shared.title.lock().unwrap() = host_name;
                }
                sess.control
                    .send(&ClientMessage::SetMode(SessionMode::FullControl))
                    .await?;
                Ok::<_, anyhow::Error>((ep, sess.conn, sess.control))
            }
            .await;

            match res {
                Ok((ep, conn, mut control)) => {
                    shared.connected.store(true, Ordering::SeqCst);
                    ctx.request_repaint();
                    let sh = shared.clone();
                    let ctx2 = ctx.clone();
                    tokio::spawn(async move { frame_loop(ep, conn, sh, ctx2).await });
                    tokio::spawn(async move {
                        while let Some(ev) = rx.recv().await {
                            if control.send(&ClientMessage::Input(ev)).await.is_err() {
                                break;
                            }
                        }
                    });
                }
                Err(e) => {
                    *shared.err.lock().unwrap() = Some(e.to_string());
                    ctx.request_repaint();
                }
            }
        });
    }
}

async fn frame_loop(
    _ep: quinn::Endpoint,
    conn: quinn::Connection,
    shared: Arc<Shared>,
    ctx: egui::Context,
) {
    let mut dec = Decoder::new();
    let mut count: u64 = 0;
    let mut bytes: u64 = 0;
    let mut win_start = Instant::now();
    let mut win_count: u64 = 0;

    loop {
        match mm_quic::recv_frame(&conn).await {
            Ok((hb, payload)) => {
                bytes += (hb.len() + payload.len()) as u64;
                let header: FrameHeader = match mm_proto::decode(&hb) {
                    Ok(h) => h,
                    Err(_) => continue,
                };
                if header.codec == FrameCodec::Tiled {
                    let ef: EncodedFrame = match mm_proto::decode(&payload) {
                        Ok(e) => e,
                        Err(_) => continue,
                    };
                    if let Ok(fb) = dec.apply(&ef) {
                        *shared.frame.lock().unwrap() = Some(FrameBuf {
                            w: header.width,
                            h: header.height,
                            rgba: fb.to_vec(),
                        });
                        shared.frame_id.fetch_add(1, Ordering::SeqCst);
                    }
                }
                count += 1;
                win_count += 1;
                let dt = win_start.elapsed().as_secs_f32();
                if dt >= 0.5 {
                    let fps = win_count as f32 / dt;
                    let kb = bytes as f32 / count.max(1) as f32 / 1024.0;
                    *shared.stats.lock().unwrap() = Stats { fps, kbps_frame: kb };
                    win_start = Instant::now();
                    win_count = 0;
                }
                ctx.request_repaint();
            }
            Err(_) => {
                shared.connected.store(false, Ordering::SeqCst);
                ctx.request_repaint();
                break;
            }
        }
    }
}

fn map_button(b: egui::PointerButton) -> Option<MouseButton> {
    match b {
        egui::PointerButton::Primary => Some(MouseButton::Left),
        egui::PointerButton::Secondary => Some(MouseButton::Right),
        egui::PointerButton::Middle => Some(MouseButton::Middle),
        _ => None,
    }
}

fn map_key(k: egui::Key) -> Option<u32> {
    use egui::Key::*;
    let vk = match k {
        A => 0x41, B => 0x42, C => 0x43, D => 0x44, E => 0x45, F => 0x46, G => 0x47, H => 0x48,
        I => 0x49, J => 0x4A, K => 0x4B, L => 0x4C, M => 0x4D, N => 0x4E, O => 0x4F, P => 0x50,
        Q => 0x51, R => 0x52, S => 0x53, T => 0x54, U => 0x55, V => 0x56, W => 0x57, X => 0x58,
        Y => 0x59, Z => 0x5A,
        Num0 => 0x30, Num1 => 0x31, Num2 => 0x32, Num3 => 0x33, Num4 => 0x34,
        Num5 => 0x35, Num6 => 0x36, Num7 => 0x37, Num8 => 0x38, Num9 => 0x39,
        Space => 0x20, Enter => 0x0D, Backspace => 0x08, Tab => 0x09, Escape => 0x1B,
        Delete => 0x2E, ArrowLeft => 0x25, ArrowUp => 0x26, ArrowRight => 0x27, ArrowDown => 0x28,
        Home => 0x24, End => 0x23, _ => return None,
    };
    Some(vk)
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        match self.screen {
            Screen::Connect => self.connect_ui(ctx),
            Screen::Session => self.session_ui(ctx),
        }
    }
}

impl App {
    fn connect_ui(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                ui.heading("MinMin Remote");
                ui.label("Điều khiển máy từ xa");
            });
            ui.add_space(24.0);
            egui::Grid::new("form").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
                ui.label("Địa chỉ máy con");
                ui.text_edit_singleline(&mut self.host);
                ui.end_row();
                ui.label("Cổng");
                ui.text_edit_singleline(&mut self.port);
                ui.end_row();
                ui.label("Mật khẩu");
                ui.add(egui::TextEdit::singleline(&mut self.password).password(true));
                ui.end_row();
                ui.label("Tên của bố");
                ui.text_edit_singleline(&mut self.name);
                ui.end_row();
            });
            ui.add_space(16.0);
            if ui.button("Kết nối").clicked() && !self.host.trim().is_empty() {
                self.start_connect(ctx);
            }
            if let Some(err) = self.shared.err.lock().unwrap().clone() {
                ui.add_space(12.0);
                ui.colored_label(egui::Color32::from_rgb(0xE2, 0x4B, 0x4A), err);
            }
        });
    }

    fn session_ui(&mut self, ctx: &egui::Context) {
        let connected = self.shared.connected.load(Ordering::SeqCst);
        let stats = *self.shared.stats.lock().unwrap();
        let title = self.shared.title.lock().unwrap().clone();

        egui::TopBottomPanel::top("bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!("Máy: {}  {}:{}", title, self.host, self.port));
                ui.separator();
                ui.checkbox(&mut self.control_mode_full, "Điều khiển");
                ui.separator();
                ui.label(format!("{:.0} fps · {:.1} KB/khung", stats.fps, stats.kbps_frame));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Ngắt").clicked() {
                        if let Some(tx) = &self.input_tx {
                            let _ = tx; // input task tự dừng khi mất kết nối
                        }
                        self.input_tx = None;
                        self.shared.connected.store(false, Ordering::SeqCst);
                        self.screen = Screen::Connect;
                    }
                    if !connected {
                        ui.colored_label(
                            egui::Color32::from_rgb(0xBA, 0x75, 0x17),
                            "đang kết nối…",
                        );
                    } else {
                        ui.colored_label(egui::Color32::from_rgb(0x1D, 0x9E, 0x75), "● đã nối");
                    }
                });
            });
        });

        // Cập nhật texture nếu có khung mới.
        let fid = self.shared.frame_id.load(Ordering::SeqCst);
        if fid != self.last_frame_id {
            if let Some(fb) = self.shared.frame.lock().unwrap().as_ref() {
                let img = egui::ColorImage::from_rgba_unmultiplied(
                    [fb.w as usize, fb.h as usize],
                    &fb.rgba,
                );
                match &mut self.tex {
                    Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                    None => {
                        self.tex =
                            Some(ctx.load_texture("screen", img, egui::TextureOptions::LINEAR))
                    }
                }
            }
            self.last_frame_id = fid;
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(tex) = &self.tex {
                let tsize = tex.size_vec2();
                let avail = ui.available_size();
                let scale = (avail.x / tsize.x).min(avail.y / tsize.y).min(4.0);
                let draw = tsize * scale;
                let (rect, _resp) =
                    ui.allocate_exact_size(draw, egui::Sense::click_and_drag());
                ui.painter().image(
                    tex.id(),
                    rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );

                if connected && self.control_mode_full {
                    if let Some(tx) = &self.input_tx {
                        let norm = |p: egui::Pos2| -> (f32, f32) {
                            (
                                ((p.x - rect.min.x) / rect.width()).clamp(0.0, 1.0),
                                ((p.y - rect.min.y) / rect.height()).clamp(0.0, 1.0),
                            )
                        };
                        ctx.input(|i| {
                            for e in &i.events {
                                match e {
                                    egui::Event::PointerMoved(p) if rect.contains(*p) => {
                                        let (x, y) = norm(*p);
                                        let _ = tx.send(InputEvent::MouseMove { x, y });
                                    }
                                    egui::Event::PointerButton {
                                        pos, button, pressed, ..
                                    } if rect.contains(*pos) => {
                                        if let Some(b) = map_button(*button) {
                                            let (x, y) = norm(*pos);
                                            let ev = if *pressed {
                                                InputEvent::MouseDown { button: b, x, y }
                                            } else {
                                                InputEvent::MouseUp { button: b, x, y }
                                            };
                                            let _ = tx.send(ev);
                                        }
                                    }
                                    egui::Event::MouseWheel { delta, .. } => {
                                        let d = if delta.y > 0.0 {
                                            1
                                        } else if delta.y < 0.0 {
                                            -1
                                        } else {
                                            0
                                        };
                                        if d != 0 {
                                            let _ = tx.send(InputEvent::MouseWheel { delta: d });
                                        }
                                    }
                                    egui::Event::Key { key, pressed, repeat, .. } if !*repeat => {
                                        if let Some(vk) = map_key(*key) {
                                            let ev = if *pressed {
                                                InputEvent::KeyDown { vk }
                                            } else {
                                                InputEvent::KeyUp { vk }
                                            };
                                            let _ = tx.send(ev);
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        });
                    }
                }
            } else {
                ui.centered_and_justified(|ui| {
                    ui.label("Đang chờ khung hình đầu tiên…");
                });
            }
        });

        ctx.request_repaint();
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1100.0, 720.0]),
        ..Default::default()
    };
    eframe::run_native(
        "MinMin Remote",
        options,
        Box::new(|_cc| Ok(Box::<App>::default())),
    )
}
