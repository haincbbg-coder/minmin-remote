//! mm-viewer-gui — cửa sổ điều khiển máy từ xa (egui), kiểu Radmin VPN.
//!
//! Đăng nhập tài khoản trên relay → thấy danh bạ máy online → bấm một máy là
//! xem/điều khiển trực tiếp qua Internet (tự đục NAT nối thẳng, hỏng thì qua
//! relay). Mã hoá đầu cuối. Giao diện tối, font tiếng Việt (Inter nhúng sẵn).

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::{Align2, Color32, CursorIcon, FontFamily, FontId, RichText, Rounding, Sense, Stroke};
use mm_codec::{Decoder, EncodedFrame};
use mm_proto::{
    ClientMessage, DeviceInfo, FrameCodec, FrameHeader, InputEvent, MouseButton, RelayMessage,
    ServerMessage, SessionMode, PROTOCOL_VERSION,
};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

const DEFAULT_RELAY: &str = "187.77.157.250:5000";

// ---------------- bảng màu ----------------
const BG: Color32 = Color32::from_rgb(0x0C, 0x11, 0x18);
const CARD: Color32 = Color32::from_rgb(0x14, 0x1A, 0x24);
const CARD2: Color32 = Color32::from_rgb(0x1B, 0x22, 0x30);
const HOVER: Color32 = Color32::from_rgb(0x20, 0x29, 0x3A);
const BORDER: Color32 = Color32::from_rgb(0x25, 0x2E, 0x3E);
const TEXT: Color32 = Color32::from_rgb(0xE7, 0xEC, 0xF4);
const MUTED: Color32 = Color32::from_rgb(0x8A, 0x93, 0xA6);
const ACCENT: Color32 = Color32::from_rgb(0x5B, 0x7C, 0xFF);
const BADGE: Color32 = Color32::from_rgb(0x63, 0x66, 0xF1);
const GREEN: Color32 = Color32::from_rgb(0x2B, 0xD1, 0x7E);
const AMBER: Color32 = Color32::from_rgb(0xF2, 0xB2, 0x3C);
const RED: Color32 = Color32::from_rgb(0xE2, 0x4B, 0x4A);
const WHITE: Color32 = Color32::WHITE;

fn sbfam() -> FontFamily {
    FontFamily::Name("sb".into())
}
fn sb(t: impl Into<String>) -> RichText {
    RichText::new(t).family(sbfam())
}
fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(f(a.r(), b.r()), f(a.g(), b.g()), f(a.b(), b.b()))
}

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

// ---------------- logo + theme ----------------

/// Vẽ phù hiệu logo: ô bo góc gradient-sáng, hình màn hình trắng ở giữa.
fn paint_badge(ui: &mut egui::Ui, side: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(side, side), Sense::hover());
    let p = ui.painter();
    // quầng sáng
    p.rect_filled(
        rect.expand(3.0),
        Rounding::same(side * 0.34),
        Color32::from_rgba_unmultiplied(0x5B, 0x7C, 0xFF, 26),
    );
    // nền
    p.rect_filled(rect, Rounding::same(side * 0.28), BADGE);
    // vệt sáng phía trên
    let top = egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x, rect.min.y + side * 0.52));
    p.rect_filled(
        top,
        Rounding { nw: side * 0.28, ne: side * 0.28, sw: 0.0, se: 0.0 },
        Color32::from_rgba_unmultiplied(255, 255, 255, 20),
    );
    // màn hình trắng
    let m = egui::Rect::from_center_size(
        rect.center() - egui::vec2(0.0, side * 0.03),
        egui::vec2(side * 0.46, side * 0.30),
    );
    p.rect(m, Rounding::same(side * 0.06), WHITE, Stroke::NONE);
    // chân đế
    let neck = egui::Rect::from_center_size(
        egui::pos2(rect.center().x, m.max.y + side * 0.075),
        egui::vec2(side * 0.06, side * 0.09),
    );
    p.rect_filled(neck, Rounding::same(1.0), WHITE);
    let foot = egui::Rect::from_center_size(
        egui::pos2(rect.center().x, m.max.y + side * 0.14),
        egui::vec2(side * 0.24, side * 0.035),
    );
    p.rect_filled(foot, Rounding::same(side * 0.02), WHITE);
}

fn install_theme(ctx: &egui::Context) {
    // ----- font tiếng Việt (Inter) -----
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "Inter".to_owned(),
        egui::FontData::from_static(include_bytes!("../assets/Inter-Regular.otf")),
    );
    fonts.font_data.insert(
        "Inter-SB".to_owned(),
        egui::FontData::from_static(include_bytes!("../assets/Inter-SemiBold.otf")),
    );
    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, "Inter".to_owned());
    fonts
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .insert(0, "Inter".to_owned());
    fonts
        .families
        .insert(FontFamily::Name("sb".into()), vec!["Inter-SB".to_owned(), "Inter".to_owned()]);
    ctx.set_fonts(fonts);

    // ----- style -----
    let mut style = (*ctx.style()).clone();
    style.text_styles = std::collections::BTreeMap::from([
        (egui::TextStyle::Heading, FontId::new(22.0, sbfam())),
        (egui::TextStyle::Body, FontId::new(15.0, FontFamily::Proportional)),
        (egui::TextStyle::Button, FontId::new(15.0, sbfam())),
        (egui::TextStyle::Small, FontId::new(12.5, FontFamily::Proportional)),
        (egui::TextStyle::Monospace, FontId::new(13.0, FontFamily::Monospace)),
    ]);
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    style.spacing.button_padding = egui::vec2(14.0, 8.0);
    style.spacing.interact_size.y = 38.0;

    let mut v = egui::Visuals::dark();
    v.override_text_color = Some(TEXT);
    v.panel_fill = BG;
    v.window_fill = CARD;
    v.faint_bg_color = CARD2;
    v.extreme_bg_color = CARD2;
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.window_rounding = Rounding::same(14.0);
    v.selection.bg_fill = Color32::from_rgb(0x2A, 0x3A, 0x66);
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.hyperlink_color = ACCENT;

    let r = Rounding::same(10.0);
    v.widgets.noninteractive.bg_fill = CARD;
    v.widgets.noninteractive.weak_bg_fill = CARD;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.noninteractive.rounding = r;

    v.widgets.inactive.bg_fill = CARD2;
    v.widgets.inactive.weak_bg_fill = CARD2;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.inactive.rounding = r;
    v.widgets.inactive.expansion = 0.0;

    v.widgets.hovered.bg_fill = HOVER;
    v.widgets.hovered.weak_bg_fill = HOVER;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.hovered.rounding = r;
    v.widgets.hovered.expansion = 1.0;

    v.widgets.active.bg_fill = ACCENT;
    v.widgets.active.weak_bg_fill = ACCENT;
    v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    v.widgets.active.fg_stroke = Stroke::new(1.0, WHITE);
    v.widgets.active.rounding = r;
    v.widgets.active.expansion = 1.0;

    style.visuals = v;
    ctx.set_style(style);
}

/// Icon cửa sổ (vẽ bằng tay): ô bo góc gradient + màn hình trắng.
fn app_icon() -> egui::IconData {
    let s: usize = 64;
    let mut rgba = vec![0u8; s * s * 4];
    let sf = s as f32;
    // SDF ô bo góc
    let sdf = |px: f32, py: f32, cx: f32, cy: f32, hx: f32, hy: f32, rad: f32| -> f32 {
        let qx = (px - cx).abs() - (hx - rad);
        let qy = (py - cy).abs() - (hy - rad);
        let ox = qx.max(0.0);
        let oy = qy.max(0.0);
        (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - rad
    };
    for y in 0..s {
        for x in 0..s {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            // phù hiệu
            let a_badge = (0.5 - sdf(px, py, sf / 2.0, sf / 2.0, sf / 2.0, sf / 2.0, sf * 0.28))
                .clamp(0.0, 1.0);
            if a_badge <= 0.0 {
                continue;
            }
            // gradient dọc indigo -> tím
            let t = py / sf;
            let cr = (0x6E as f32 + (0x7C as f32 - 0x6E as f32) * t) as u8;
            let cg = (0x8B as f32 + (0x5C as f32 - 0x8B as f32) * t) as u8;
            let cb = (0xFF as f32 + (0xF6 as f32 - 0xFF as f32) * t) as u8;
            let (mut rr, mut gg, mut bb) = (cr, cg, cb);
            // màn hình trắng
            let a_mon = (0.5
                - sdf(px, py, sf / 2.0, sf * 0.44, sf * 0.26, sf * 0.165, sf * 0.05))
            .clamp(0.0, 1.0);
            // chân + đế
            let in_neck = (px - sf / 2.0).abs() < sf * 0.03 && (py - sf * 0.64).abs() < sf * 0.05;
            let in_foot = (px - sf / 2.0).abs() < sf * 0.13 && (py - sf * 0.70).abs() < sf * 0.02;
            let a_white = a_mon.max(if in_neck || in_foot { 1.0 } else { 0.0 });
            if a_white > 0.0 {
                rr = (rr as f32 * (1.0 - a_white) + 255.0 * a_white) as u8;
                gg = (gg as f32 * (1.0 - a_white) + 255.0 * a_white) as u8;
                bb = (bb as f32 * (1.0 - a_white) + 255.0 * a_white) as u8;
            }
            let i = (y * s + x) * 4;
            rgba[i] = rr;
            rgba[i + 1] = gg;
            rgba[i + 2] = bb;
            rgba[i + 3] = (a_badge * 255.0) as u8;
        }
    }
    egui::IconData { rgba, width: s as u32, height: s as u32 }
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
        let frame = egui::Frame::none().fill(BG).inner_margin(egui::Margin::same(18.0));
        egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
            let avail_h = ui.available_height();
            ui.add_space((avail_h * 0.10).min(70.0));
            ui.vertical_centered(|ui| {
                ui.set_max_width(400.0);
                let card = egui::Frame::none()
                    .fill(CARD)
                    .rounding(Rounding::same(16.0))
                    .stroke(Stroke::new(1.0, BORDER))
                    .inner_margin(egui::Margin::same(26.0));
                card.show(ui, |ui| {
                    ui.set_width(340.0);
                    ui.vertical_centered(|ui| {
                        paint_badge(ui, 56.0);
                        ui.add_space(12.0);
                        ui.label(sb("MinMin Remote").size(22.0).color(TEXT));
                        ui.add_space(2.0);
                        ui.label(RichText::new("Điều khiển máy từ xa").size(13.0).color(MUTED));
                    });
                    ui.add_space(18.0);

                    let field = |ui: &mut egui::Ui, label: &str, val: &mut String, pw: bool| {
                        ui.add_space(10.0);
                        ui.label(RichText::new(label).size(12.5).color(MUTED));
                        ui.add_space(3.0);
                        ui.add(
                            egui::TextEdit::singleline(val)
                                .desired_width(f32::INFINITY)
                                .password(pw),
                        );
                    };
                    field(ui, "Máy chủ cầu nối", &mut self.relay_addr, false);
                    field(ui, "Tài khoản", &mut self.account, false);
                    field(ui, "Mật khẩu máy", &mut self.password, true);
                    field(ui, "Tên của bố", &mut self.name, false);

                    ui.add_space(20.0);
                    let btn = egui::Button::new(sb("Đăng nhập").size(15.0).color(WHITE))
                        .fill(ACCENT)
                        .rounding(Rounding::same(10.0))
                        .min_size(egui::vec2(ui.available_width(), 42.0));
                    if ui.add(btn).clicked() && !self.account.trim().is_empty() {
                        self.start_login(ctx);
                    }

                    if let Some(e) = self.shared.err.lock().unwrap().clone() {
                        ui.add_space(12.0);
                        ui.label(RichText::new(format!("⚠  {e}")).color(RED).size(13.0));
                    }
                    ui.add_space(6.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new("Mã hoá đầu cuối · tự đục NAT nối thẳng")
                                .size(11.5)
                                .color(mix(MUTED, BG, 0.3)),
                        );
                    });
                });
            });
        });
    }

    fn start_login(&mut self, ctx: &egui::Context) {
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

    fn directory_ui(&mut self, ctx: &egui::Context) {
        let logged = self.shared.logged_in.load(Ordering::SeqCst);
        let devices = self.shared.devices.lock().unwrap().clone();
        let online = devices.iter().filter(|d| d.online).count();

        let topframe = egui::Frame::none()
            .fill(CARD)
            .inner_margin(egui::Margin::symmetric(16.0, 11.0))
            .stroke(Stroke::new(1.0, BORDER));
        egui::TopBottomPanel::top("dirbar").frame(topframe).show(ctx, |ui| {
            ui.horizontal(|ui| {
                paint_badge(ui, 26.0);
                ui.add_space(4.0);
                ui.label(sb("Danh bạ máy").size(16.0).color(TEXT));
                ui.label(RichText::new(format!("@{}", self.account)).size(12.5).color(MUTED));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(sb("Đăng xuất")).clicked() {
                        self.cmd_tx = None;
                        self.shared.logged_in.store(false, Ordering::SeqCst);
                        self.shared.devices.lock().unwrap().clear();
                        self.screen = Screen::Login;
                    }
                    ui.add_space(4.0);
                    ui.checkbox(&mut self.control_mode_full, "Điều khiển");
                });
            });
        });

        let frame = egui::Frame::none().fill(BG).inner_margin(egui::Margin::same(18.0));
        egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
            if !logged {
                // đang đăng nhập / lỗi
                ui.add_space(60.0);
                ui.vertical_centered(|ui| {
                    if let Some(e) = self.shared.err.lock().unwrap().clone() {
                        ui.label(RichText::new(format!("⚠  {e}")).color(RED).size(14.0));
                        ui.add_space(10.0);
                        if ui.button(sb("Về đăng nhập")).clicked() {
                            self.screen = Screen::Login;
                        }
                    } else {
                        ui.spinner();
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new(self.shared.status.lock().unwrap().clone())
                                .color(MUTED)
                                .size(14.0),
                        );
                    }
                });
                ctx.request_repaint_after(Duration::from_millis(120));
                return;
            }

            ui.horizontal(|ui| {
                ui.label(sb("Máy của bố").size(14.5).color(TEXT));
                ui.label(
                    RichText::new(format!("{online}/{} online", devices.len()))
                        .size(12.5)
                        .color(MUTED),
                );
            });
            ui.add_space(10.0);

            if devices.is_empty() {
                ui.add_space(50.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("🖥").size(34.0).color(MUTED));
                    ui.add_space(6.0);
                    ui.label(sb("Chưa có máy nào online").size(15.0).color(TEXT));
                    ui.add_space(2.0);
                    ui.label(
                        RichText::new("Mở mm-server trên máy con rồi đăng nhập lại.")
                            .size(13.0)
                            .color(MUTED),
                    );
                });
                ctx.request_repaint_after(Duration::from_millis(250));
                return;
            }

            let pulse = {
                let t = ui.input(|i| i.time);
                0.5 + 0.5 * ((t * 2.2).sin() as f32)
            };
            let mut to_connect: Option<String> = None;
            egui::ScrollArea::vertical().show(ui, |ui| {
                for d in &devices {
                    if self.device_card(ui, d, pulse) {
                        to_connect = Some(d.device_id.clone());
                    }
                    ui.add_space(8.0);
                }
            });
            ctx.request_repaint_after(Duration::from_millis(33));

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

    /// Vẽ một thẻ máy; trả về true nếu được bấm để kết nối.
    fn device_card(&self, ui: &mut egui::Ui, d: &DeviceInfo, pulse: f32) -> bool {
        let w = ui.available_width();
        let sense = if d.online { Sense::click() } else { Sense::hover() };
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 66.0), sense);
        let id = egui::Id::new(("card", &d.device_id));
        let hover_t = ui.ctx().animate_bool_with_time(id, resp.hovered() && d.online, 0.12);
        let bg = mix(CARD, HOVER, hover_t);
        let stroke_c = mix(BORDER, ACCENT, hover_t);

        let p = ui.painter();
        p.rect(rect, Rounding::same(14.0), bg, Stroke::new(1.0, stroke_c));

        // avatar
        let ac = egui::pos2(rect.left() + 36.0, rect.center().y);
        p.circle_filled(ac, 20.0, if d.online { BADGE } else { CARD2 });
        let letter: String = d
            .device_name
            .chars()
            .next()
            .unwrap_or('?')
            .to_uppercase()
            .collect();
        p.text(
            ac,
            Align2::CENTER_CENTER,
            letter,
            FontId::new(17.0, sbfam()),
            if d.online { WHITE } else { MUTED },
        );
        // chấm trạng thái (nhấp nháy khi online)
        let dc = ac + egui::vec2(15.0, 15.0);
        p.circle_filled(dc, 7.0, bg);
        let dot = if d.online {
            mix(GREEN, bg, (1.0 - pulse) * 0.45)
        } else {
            Color32::from_rgb(0x4A, 0x53, 0x63)
        };
        p.circle_filled(dc, 5.0, dot);

        // tên + id
        let tx = rect.left() + 66.0;
        p.text(
            egui::pos2(tx, rect.center().y - 9.0),
            Align2::LEFT_CENTER,
            &d.device_name,
            FontId::new(15.5, sbfam()),
            TEXT,
        );
        p.text(
            egui::pos2(tx, rect.center().y + 11.0),
            Align2::LEFT_CENTER,
            &d.device_id,
            FontId::new(12.0, FontFamily::Proportional),
            MUTED,
        );

        // phải: trạng thái / kết nối
        let right = egui::pos2(rect.right() - 18.0, rect.center().y);
        if d.online {
            let col = mix(ACCENT, WHITE, hover_t * 0.6);
            p.text(right, Align2::RIGHT_CENTER, "Kết nối  →", FontId::new(13.5, sbfam()), col);
        } else {
            p.text(
                right,
                Align2::RIGHT_CENTER,
                "Ngoại tuyến",
                FontId::new(13.0, FontFamily::Proportional),
                MUTED,
            );
        }

        if d.online {
            resp.clone().on_hover_cursor(CursorIcon::PointingHand);
        }
        resp.clicked() && d.online
    }

    fn session_ui(&mut self, ctx: &egui::Context) {
        let active = self.shared.session_active.load(Ordering::SeqCst);
        let stats = *self.shared.stats.lock().unwrap();
        let title = self.shared.title.lock().unwrap().clone();
        let status = self.shared.status.lock().unwrap().clone();

        let topframe = egui::Frame::none()
            .fill(CARD)
            .inner_margin(egui::Margin::symmetric(14.0, 9.0))
            .stroke(Stroke::new(1.0, BORDER));
        egui::TopBottomPanel::top("sbar").frame(topframe).show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button(sb("←  Danh bạ")).clicked() {
                    self.input_tx = None; // đóng kênh input -> dừng phiên
                    self.shared.session_active.store(false, Ordering::SeqCst);
                    self.screen = Screen::Directory;
                }
                ui.add_space(6.0);
                ui.label(
                    sb(if title.is_empty() { self.current_device.clone() } else { title })
                        .size(15.0)
                        .color(TEXT),
                );

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if active {
                        let (txt, col) = if stats.direct {
                            ("● nối thẳng", GREEN)
                        } else {
                            ("● relay", AMBER)
                        };
                        ui.label(RichText::new(txt).color(col).size(12.5));
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new(format!(
                                "{:.0} fps · {:.1} KB/khung",
                                stats.fps, stats.kbps_frame
                            ))
                            .color(MUTED)
                            .size(12.5),
                        );
                    } else {
                        ui.label(RichText::new(status.clone()).color(MUTED).size(12.5));
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

        let frame = egui::Frame::none().fill(Color32::BLACK);
        egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
            if let Some(tex) = &self.tex {
                let tsize = tex.size_vec2();
                let avail = ui.available_size();
                let scale = (avail.x / tsize.x).min(avail.y / tsize.y).min(4.0);
                let draw = tsize * scale;
                // căn giữa (letterbox)
                let off = ((avail - draw) * 0.5).max(egui::vec2(0.0, 0.0));
                ui.add_space(off.y);
                ui.horizontal(|ui| {
                    ui.add_space(off.x);
                    let (rect, _resp) =
                        ui.allocate_exact_size(draw, egui::Sense::click_and_drag());
                    ui.painter().image(
                        tex.id(),
                        rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        Color32::WHITE,
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
                                                let _ =
                                                    tx.send(InputEvent::MouseWheel { delta: d });
                                            }
                                        }
                                        egui::Event::Key { key, pressed, repeat, .. }
                                            if !*repeat =>
                                        {
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
                });
            } else {
                ui.centered_and_justified(|ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(ui.available_height() * 0.4);
                        ui.spinner();
                        ui.add_space(10.0);
                        ui.label(
                            RichText::new(if status.is_empty() {
                                "Đang kết nối…".to_string()
                            } else {
                                status
                            })
                            .color(MUTED)
                            .size(14.0),
                        );
                    });
                });
            }
        });
        ctx.request_repaint();
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1120.0, 720.0])
            .with_min_inner_size([760.0, 520.0])
            .with_title("MinMin Remote")
            .with_icon(app_icon()),
        ..Default::default()
    };
    eframe::run_native(
        "MinMin Remote",
        options,
        Box::new(|cc| {
            install_theme(&cc.egui_ctx);
            Ok(Box::<App>::default())
        }),
    )
}
