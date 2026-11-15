//! mm-viewer-gui — cửa sổ điều khiển máy từ xa (egui), kiểu Radmin VPN.
//!
//! Đăng nhập tài khoản trên relay → thấy danh bạ máy online → bấm một máy là
//! xem/điều khiển trực tiếp qua Internet (tự đục NAT nối thẳng, hỏng thì qua
//! relay). Mã hoá đầu cuối.

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mm_codec::{Decoder, EncodedFrame};
use mm_proto::{
    ClientMessage, DeviceInfo, FrameCodec, FrameHeader, InputEvent, MouseButton, RelayMessage,
    ServerMessage, SessionMode, PROTOCOL_VERSION,
};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

const DEFAULT_RELAY: &str = "187.77.157.250:5000";

struct FrameBuf {
    w: u32,
    h: u32,
    rgba: Vec<u8>,
}

#[derive(Default, Clone, Copy)]
struct Stats {
    fps: f32,
    kbps_frame: f32,
    direct: bool,
}

#[derive(Default)]
struct Shared {
    frame: Mutex<Option<FrameBuf>>,
    frame_id: AtomicU64,
    session_active: AtomicBool,
    logged_in: AtomicBool,
    err: Mutex<Option<String>>,
    status: Mutex<String>,
    title: Mutex<String>,
    stats: Mutex<Stats>,
    devices: Mutex<Vec<DeviceInfo>>,
}

enum Cmd {
    Connect {
        device_id: String,
        full: bool,
        input_rx: UnboundedReceiver<InputEvent>,
    },
}

enum Screen {
    Login,
    Directory,
    Session,
}

struct App {
    rt: tokio::runtime::Runtime,
    shared: Arc<Shared>,
    screen: Screen,
    relay_addr: String,
    account: String,
    password: String,
    name: String,
    cmd_tx: Option<UnboundedSender<Cmd>>,
    input_tx: Option<UnboundedSender<InputEvent>>,
    tex: Option<egui::TextureHandle>,
    last_frame_id: u64,
    control_mode_full: bool,
    current_device: String,
}

impl Default for App {
    fn default() -> Self {
        Self {
            rt: tokio::runtime::Runtime::new().expect("runtime tokio"),
            shared: Arc::new(Shared::default()),
            screen: Screen::Login,
            relay_addr: DEFAULT_RELAY.to_string(),
            account: "halimi".to_string(),
            password: String::new(),
            name: "bố".to_string(),
            cmd_tx: None,
            input_tx: None,
            tex: None,
            last_frame_id: 0,
            control_mode_full: true,
            current_device: String::new(),
        }
    }
}

// ---------------- khung hình vào bộ nhớ chung ----------------

fn store_frame(shared: &Shared, dec: &mut Decoder, header: &FrameHeader, payload: &[u8]) {
    if header.codec != FrameCodec::Tiled {
        return;
    }
    if let Ok(ef) = mm_proto::decode::<EncodedFrame>(payload) {
        if let Ok(fb) = dec.apply(&ef) {
            *shared.frame.lock().unwrap() = Some(FrameBuf {
                w: header.width,
                h: header.height,
                rgba: fb.to_vec(),
            });
            shared.frame_id.fetch_add(1, Ordering::SeqCst);
        }
    }
}

struct Meter {
    count: u64,
    bytes: u64,
    win_start: Instant,
    win_count: u64,
    direct: bool,
}
impl Meter {
    fn new(direct: bool) -> Self {
        Self { count: 0, bytes: 0, win_start: Instant::now(), win_count: 0, direct }
    }
    fn tick(&mut self, shared: &Shared, nbytes: usize) {
        self.count += 1;
        self.win_count += 1;
        self.bytes += nbytes as u64;
        let dt = self.win_start.elapsed().as_secs_f32();
        if dt >= 0.5 {
            *shared.stats.lock().unwrap() = Stats {
                fps: self.win_count as f32 / dt,
                kbps_frame: self.bytes as f32 / self.count.max(1) as f32 / 1024.0,
                direct: self.direct,
            };
            self.win_start = Instant::now();
            self.win_count = 0;
        }
    }
}

// ---------------- quản lý relay (một task nền) ----------------

async fn relay_manager(
    relay_addr: String,
    account: String,
    password: String,
    name: String,
    shared: Arc<Shared>,
    ctx: egui::Context,
    mut cmd_rx: UnboundedReceiver<Cmd>,
) {
    let set_err = |e: String| {
        *shared.err.lock().unwrap() = Some(e);
        ctx.request_repaint();
    };
    let addr: std::net::SocketAddr = match relay_addr.trim().parse() {
        Ok(a) => a,
        Err(_) => return set_err("Địa chỉ relay không hợp lệ".into()),
    };
    let endpoint = match mm_quic::dual_endpoint("0.0.0.0:0".parse().unwrap()) {
        Ok(e) => e,
        Err(e) => return set_err(format!("endpoint: {e}")),
    };
    let conn = match endpoint.connect(addr, "mm").and_then(|c| Ok(c)) {
        Ok(connecting) => match connecting.await {
            Ok(c) => c,
            Err(e) => return set_err(format!("không nối được relay: {e}")),
        },
        Err(e) => return set_err(format!("relay: {e}")),
    };
    let (mut cs, mut cr) = match conn.open_bi().await {
        Ok(v) => v,
        Err(e) => return set_err(format!("relay: {e}")),
    };
    if relay_send(&mut cs, &RelayMessage::LoginViewer { account: account.clone() })
        .await
        .is_err()
    {
        return set_err("không gửi được đăng nhập".into());
    }
    match relay_recv(&mut cr).await {
        Ok(RelayMessage::Devices(list)) => {
            *shared.devices.lock().unwrap() = list;
            shared.logged_in.store(true, Ordering::SeqCst);
            *shared.status.lock().unwrap() = String::new();
            ctx.request_repaint();
        }
        Ok(other) => return set_err(format!("relay trả lời lạ: {other:?}")),
        Err(e) => return set_err(format!("relay: {e}")),
    }

    while let Some(Cmd::Connect { device_id, full, input_rx }) = cmd_rx.recv().await {
        shared.session_active.store(false, Ordering::SeqCst);
        *shared.title.lock().unwrap() = String::new();
        *shared.status.lock().unwrap() = format!("Đang nối tới {device_id}…");
        ctx.request_repaint();
        if let Err(e) = connect_device(
            &endpoint, &conn, &mut cs, &mut cr, &device_id, full, &password, &name, &shared, &ctx,
            input_rx,
        )
        .await
        {
            *shared.status.lock().unwrap() = format!("Lỗi: {e}");
            ctx.request_repaint();
        }
        shared.session_active.store(false, Ordering::SeqCst);
        ctx.request_repaint();
    }
}

#[allow(clippy::too_many_arguments)]
async fn connect_device(
    endpoint: &quinn::Endpoint,
    conn: &quinn::Connection,
    cs: &mut quinn::SendStream,
    cr: &mut quinn::RecvStream,
    device_id: &str,
    full: bool,
    password: &str,
    name: &str,
    shared: &Arc<Shared>,
    ctx: &egui::Context,
    input_rx: UnboundedReceiver<InputEvent>,
) -> anyhow::Result<()> {
    relay_send(cs, &RelayMessage::ConnectTo { device_id: device_id.to_string() }).await?;
    let (session, peer_addr) = match relay_recv(cr).await? {
        RelayMessage::SessionReady { session, peer_addr } => (session, peer_addr),
        RelayMessage::RelayError { message } => anyhow::bail!(message),
        other => anyhow::bail!("chờ SessionReady, nhận {other:?}"),
    };
    let mode = if full { SessionMode::FullControl } else { SessionMode::ViewOnly };

    // Thử đục NAT nối thẳng.
    if let Ok(peer) = peer_addr.parse::<std::net::SocketAddr>() {
        if let Ok(direct) = mm_quic::punch_connect(endpoint, peer, Duration::from_secs(5)).await {
            if let Ok(csession) = mm_quic::client_auth(direct, password).await {
                *shared.status.lock().unwrap() = "Nối thẳng (đục NAT)".into();
                return drive_quic(csession, mode, name, shared.clone(), ctx.clone(), input_rx, true)
                    .await;
            }
        }
    }

    // Quay về chạy qua relay.
    *shared.status.lock().unwrap() = "Qua relay".into();
    let (mut send, recv) = conn.open_bi().await?;
    mm_transport::write_frame(&mut send, &session.to_be_bytes()).await?;
    let stream = tokio::io::join(recv, send);
    drive_relay(stream, password, mode, name, shared.clone(), ctx.clone(), input_rx).await
}

/// Phiên trên kết nối QUIC trực tiếp (đã đục NAT).
async fn drive_quic(
    mut cs: mm_quic::ClientSession,
    mode: SessionMode,
    name: &str,
    shared: Arc<Shared>,
    ctx: egui::Context,
    mut input_rx: UnboundedReceiver<InputEvent>,
    direct: bool,
) -> anyhow::Result<()> {
    cs.control
        .send(&ClientMessage::Hello { protocol_version: PROTOCOL_VERSION, viewer_name: name.into() })
        .await?;
    if let ServerMessage::Welcome { host_name, .. } = cs.control.recv::<ServerMessage>().await? {
        *shared.title.lock().unwrap() = host_name;
    }
    cs.control.send(&ClientMessage::SetMode(mode)).await?;
    shared.session_active.store(true, Ordering::SeqCst);
    ctx.request_repaint();

    let conn = cs.conn.clone();
    let sh = shared.clone();
    let c2 = ctx.clone();
    let feeder = tokio::spawn(async move {
        let mut dec = Decoder::new();
        let mut meter = Meter::new(direct);
        loop {
            match mm_quic::recv_frame(&conn).await {
                Ok((hb, payload)) => {
                    if let Ok(h) = mm_proto::decode::<FrameHeader>(&hb) {
                        store_frame(&sh, &mut dec, &h, &payload);
                        meter.tick(&sh, hb.len() + payload.len());
                        c2.request_repaint();
                    }
                }
                Err(_) => break,
            }
        }
    });

    while let Some(ev) = input_rx.recv().await {
        if cs.control.send(&ClientMessage::Input(ev)).await.is_err() {
            break;
        }
    }
    feeder.abort();
    Ok(())
}

/// Phiên qua relay (bắc cầu byte, mã hoá Noise).
async fn drive_relay<S>(
    mut stream: S,
    password: &str,
    mode: SessionMode,
    name: &str,
    shared: Arc<Shared>,
    ctx: egui::Context,
    mut input_rx: UnboundedReceiver<InputEvent>,
) -> anyhow::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let psk = mm_crypto::derive_psk(password)?;
    let session = mm_transport::handshake_initiator(&mut stream, &psk).await?;
    let (mut reader, mut writer) = mm_transport::secure_split(stream, session);

    writer
        .send(&ClientMessage::Hello { protocol_version: PROTOCOL_VERSION, viewer_name: name.into() })
        .await?;
    if let ServerMessage::Welcome { host_name, .. } = reader.recv::<ServerMessage>().await? {
        *shared.title.lock().unwrap() = host_name;
    }
    writer.send(&ClientMessage::SetMode(mode)).await?;
    shared.session_active.store(true, Ordering::SeqCst);
    ctx.request_repaint();

    let sh = shared.clone();
    let c2 = ctx.clone();
    let feeder = tokio::spawn(async move {
        let mut dec = Decoder::new();
        let mut meter = Meter::new(false);
        let mut pending: Option<FrameHeader> = None;
        loop {
            match reader.recv::<ServerMessage>().await {
                Ok(ServerMessage::FrameHeader(h)) => pending = Some(h),
                Ok(ServerMessage::FrameData { bytes, .. }) => {
                    if let Some(h) = pending.take() {
                        store_frame(&sh, &mut dec, &h, &bytes);
                        meter.tick(&sh, bytes.len());
                        c2.request_repaint();
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });

    while let Some(ev) = input_rx.recv().await {
        if writer.send(&ClientMessage::Input(ev)).await.is_err() {
            break;
        }
    }
    feeder.abort();
    Ok(())
}

async fn relay_send(s: &mut quinn::SendStream, m: &RelayMessage) -> anyhow::Result<()> {
    mm_transport::write_frame(s, &mm_proto::encode(m)?).await?;
    Ok(())
}
async fn relay_recv(r: &mut quinn::RecvStream) -> anyhow::Result<RelayMessage> {
    Ok(mm_proto::decode(&mm_transport::read_frame(r).await?)?)
}

// ---------------- input ----------------

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

// ---------------- giao diện ----------------

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _f: &mut eframe::Frame) {
        match self.screen {
            Screen::Login => self.login_ui(ctx),
            Screen::Directory => self.directory_ui(ctx),
            Screen::Session => self.session_ui(ctx),
        }
    }
}

impl App {
    fn login_ui(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| {
                ui.heading("MinMin Remote");
                ui.label("Đăng nhập để thấy danh bạ máy");
            });
            ui.add_space(20.0);
            egui::Grid::new("login").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
                ui.label("Máy chủ cầu nối");
                ui.text_edit_singleline(&mut self.relay_addr);
                ui.end_row();
                ui.label("Tài khoản");
                ui.text_edit_singleline(&mut self.account);
                ui.end_row();
                ui.label("Mật khẩu máy");
                ui.add(egui::TextEdit::singleline(&mut self.password).password(true));
                ui.end_row();
                ui.label("Tên của bố");
                ui.text_edit_singleline(&mut self.name);
                ui.end_row();
            });
            ui.add_space(14.0);
            if ui.button("Đăng nhập").clicked() && !self.account.trim().is_empty() {
                let (tx, rx) = unbounded_channel::<Cmd>();
                self.cmd_tx = Some(tx);
                *self.shared.err.lock().unwrap() = None;
                *self.shared.status.lock().unwrap() = "Đang đăng nhập…".into();
                self.shared.logged_in.store(false, Ordering::SeqCst);
                let (relay, acc, pw, nm) = (
                    self.relay_addr.clone(),
                    self.account.clone(),
                    self.password.clone(),
                    self.name.clone(),
                );
                let shared = self.shared.clone();
                let ctx2 = ctx.clone();
                self.rt.spawn(relay_manager(relay, acc, pw, nm, shared, ctx2, rx));
                self.screen = Screen::Directory;
            }
            if let Some(e) = self.shared.err.lock().unwrap().clone() {
                ui.add_space(10.0);
                ui.colored_label(egui::Color32::from_rgb(0xE2, 0x4B, 0x4A), e);
            }
        });
    }

    fn directory_ui(&mut self, ctx: &egui::Context) {
        let logged = self.shared.logged_in.load(Ordering::SeqCst);
        egui::TopBottomPanel::top("dirbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Danh bạ máy");
                ui.label(format!("[{}]", self.account));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Đăng xuất").clicked() {
                        self.cmd_tx = None;
                        self.shared.logged_in.store(false, Ordering::SeqCst);
                        self.shared.devices.lock().unwrap().clear();
                        self.screen = Screen::Login;
                    }
                    ui.checkbox(&mut self.control_mode_full, "Điều khiển");
                });
            });
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            if !logged {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| {
                    ui.label(self.shared.status.lock().unwrap().clone());
                    ui.spinner();
                });
                if let Some(e) = self.shared.err.lock().unwrap().clone() {
                    ui.colored_label(egui::Color32::from_rgb(0xE2, 0x4B, 0x4A), e);
                    if ui.button("Về đăng nhập").clicked() {
                        self.screen = Screen::Login;
                    }
                }
                ctx.request_repaint_after(Duration::from_millis(300));
                return;
            }
            let devices = self.shared.devices.lock().unwrap().clone();
            if devices.is_empty() {
                ui.add_space(16.0);
                ui.label("Chưa có máy nào online. Mở mm-server trên máy con rồi đăng nhập lại.");
                return;
            }
            let mut to_connect: Option<String> = None;
            egui::ScrollArea::vertical().show(ui, |ui| {
                for d in &devices {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        let color = if d.online {
                            egui::Color32::from_rgb(0x1D, 0x9E, 0x75)
                        } else {
                            egui::Color32::GRAY
                        };
                        ui.colored_label(color, "●");
                        ui.strong(&d.device_name);
                        ui.weak(format!("({})", d.device_id));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if d.online && ui.button("Kết nối").clicked() {
                                to_connect = Some(d.device_id.clone());
                            }
                        });
                    });
                    ui.separator();
                }
            });
            if let Some(device_id) = to_connect {
                let (tx, rx) = unbounded_channel::<InputEvent>();
                self.input_tx = Some(tx);
                self.current_device = device_id.clone();
                self.tex = None;
                self.last_frame_id = 0;
                *self.shared.frame.lock().unwrap() = None;
                if let Some(cmd) = &self.cmd_tx {
                    let _ = cmd.send(Cmd::Connect {
                        device_id,
                        full: self.control_mode_full,
                        input_rx: rx,
                    });
                }
                self.screen = Screen::Session;
            }
        });
    }

    fn session_ui(&mut self, ctx: &egui::Context) {
        let active = self.shared.session_active.load(Ordering::SeqCst);
        let stats = *self.shared.stats.lock().unwrap();
        let title = self.shared.title.lock().unwrap().clone();
        let status = self.shared.status.lock().unwrap().clone();

        egui::TopBottomPanel::top("sbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.strong(if title.is_empty() { self.current_device.clone() } else { title });
                ui.separator();
                let via = if stats.direct { "nối thẳng" } else { "relay" };
                ui.label(format!("{:.0} fps · {:.1} KB · {}", stats.fps, stats.kbps_frame, via));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Về danh bạ").clicked() {
                        self.input_tx = None; // đóng kênh input -> dừng phiên
                        self.shared.session_active.store(false, Ordering::SeqCst);
                        self.screen = Screen::Directory;
                    }
                    if active {
                        ui.colored_label(egui::Color32::from_rgb(0x1D, 0x9E, 0x75), "● đang xem");
                    } else {
                        ui.label(status.clone());
                    }
                });
            });
        });

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
                let (rect, _resp) =
                    ui.allocate_exact_size(tsize * scale, egui::Sense::click_and_drag());
                ui.painter().image(
                    tex.id(),
                    rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
                if active && self.control_mode_full {
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
                                    egui::Event::PointerButton { pos, button, pressed, .. }
                                        if rect.contains(*pos) =>
                                    {
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
                                        let d = if delta.y > 0.0 { 1 } else if delta.y < 0.0 { -1 } else { 0 };
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
                    ui.label(if status.is_empty() { "Đang kết nối…".into() } else { status });
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
    eframe::run_native("MinMin Remote", options, Box::new(|_cc| Ok(Box::<App>::default())))
}
