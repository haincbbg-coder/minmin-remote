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
    ClientMessage, DeviceInfo, FrameCodec, FrameHeader, InputEvent, MouseButton, PowerAction,
    RelayMessage, ServerMessage, SessionMode, PROTOCOL_VERSION,
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
const TEAL: Color32 = Color32::from_rgb(0x17, 0xB6, 0xC9);
const TEAL_DK: Color32 = Color32::from_rgb(0x0E, 0x7E, 0x8C);
const ROW_ALT: Color32 = Color32::from_rgb(0x11, 0x16, 0x1F);
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

// ---- nhớ đăng nhập (lưu ra file để lần sau tự kết nối) ----
fn config_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("APPDATA").or_else(|| std::env::var_os("HOME"))?;
    Some(std::path::Path::new(&base).join("MinMinRemote").join("viewer.cfg"))
}

fn log_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("APPDATA").or_else(|| std::env::var_os("HOME"))?;
    let dir = std::path::Path::new(&base).join("MinMinRemote");
    let _ = std::fs::create_dir_all(&dir);
    Some(dir.join("viewer.log"))
}

/// Ghi 1 dòng chẩn đoán (để soi lỗi bàn phím). Không chạy trong hook.
fn dbglog(msg: &str) {
    use std::io::Write;
    if let Some(p) = log_path() {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
            let _ = writeln!(f, "{msg}");
        }
    }
}
/// Trả về (relay, account, password, name).
fn load_config() -> Option<(String, String, String, String)> {
    let s = std::fs::read_to_string(config_path()?).ok()?;
    let mut it = s.lines();
    let relay = it.next()?.to_string();
    let account = it.next()?.to_string();
    let password = it.next().unwrap_or("").to_string();
    let name = it.next().unwrap_or("bố").to_string();
    Some((relay, account, password, name))
}
fn save_config(relay: &str, account: &str, password: &str, name: &str) {
    if let Some(p) = config_path() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, format!("{relay}\n{account}\n{password}\n{name}\n"));
    }
}

const WIN_NARROW: [f32; 2] = [380.0, 640.0];

// ---------------- bàn phím: bắt thẳng từ Windows (bỏ qua egui) ----------------
// egui "nuốt" Ctrl+C/V/X và lọc phím -> vá mãi không hết. Nên bắt phím ở tầng
// hệ điều hành (low-level keyboard hook), lấy mã VK thô, gửi nguyên sang máy con.
static KB_CAPTURE: AtomicBool = AtomicBool::new(false);
static KB_QUEUE: Mutex<Vec<(u32, bool)>> = Mutex::new(Vec::new());

#[cfg(windows)]
mod kbhook {
    use super::{dbglog, KB_CAPTURE, KB_QUEUE};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
    use std::sync::Once;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, DispatchMessageW, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId,
        SetWindowsHookExW, TranslateMessage, HHOOK, KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WM_KEYDOWN,
        WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
    };

    /// true nếu cửa sổ đang foreground thuộc đúng tiến trình viewer này
    /// (để không gửi phím lúc bố đang dùng ứng dụng khác).
    unsafe fn fg_is_ours() -> bool {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        pid != 0 && pid == GetCurrentProcessId()
    }

    unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code >= 0 && KB_CAPTURE.load(Relaxed) {
            HOOK_HITS.fetch_add(1, Relaxed);
            if fg_is_ours() {
                let kb = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
                let msg = wparam.0 as u32;
                let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
                let up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
                if down || up {
                    if let Ok(mut q) = KB_QUEUE.lock() {
                        q.push((kb.vkCode, down));
                    }
                }
            }
        }
        CallNextHookEx(HHOOK::default(), code, wparam, lparam)
    }

    pub static HOOK_OK: AtomicBool = AtomicBool::new(false);
    pub static HOOK_HITS: AtomicU64 = AtomicU64::new(0);
    static INSTALL: Once = Once::new();

    /// Cài móc trên MỘT LUỒNG RIÊNG chỉ ngồi chờ thông điệp. Luồng chính của
    /// egui vẽ lại liên tục nên Windows bỏ qua móc cấp thấp (timeout 300ms);
    /// luồng riêng này luôn ở trạng thái chờ nên móc được gọi tức thì.
    pub fn install() {
        INSTALL.call_once(|| {
            std::thread::spawn(|| unsafe {
                let hmod = GetModuleHandleW(PCWSTR::null()).unwrap_or_default();
                match SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), HINSTANCE(hmod.0), 0) {
                    Ok(_) => {
                        HOOK_OK.store(true, Relaxed);
                        dbglog("[kbhook] móc cài trên luồng riêng: OK");
                    }
                    Err(e) => {
                        HOOK_OK.store(false, Relaxed);
                        dbglog(&format!("[kbhook] cài móc THAT BAI: {e:?}"));
                    }
                }
                // Vòng lặp thông điệp giữ luồng ở trạng thái phục vụ móc.
                let mut msg = MSG::default();
                while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            });
        });
    }
}

#[cfg(not(windows))]
mod kbhook {
    use std::sync::atomic::{AtomicBool, AtomicU64};
    pub static HOOK_OK: AtomicBool = AtomicBool::new(false);
    pub static HOOK_HITS: AtomicU64 = AtomicU64::new(0);
    pub fn install() {}
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
        input_rx: UnboundedReceiver<ClientMessage>,
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
    input_tx: Option<UnboundedSender<ClientMessage>>,
    tex: Option<egui::TextureHandle>,
    last_frame_id: u64,
    session_full: bool,
    current_device: String,
    auto_login: bool,
    clip: Option<arboard::Clipboard>,
    last_clip: String,
    last_clip_poll: Instant,
    kb_sent: u64,
    last_kb_log: Instant,
    poller_spawned: bool,
}

impl Default for App {
    fn default() -> Self {
        // Đọc cấu hình đã lưu -> lần sau tự kết nối, khỏi gõ lại.
        let (relay, account, password, name, auto) = match load_config() {
            Some((r, a, p, n)) => {
                let auto = !p.is_empty();
                (r, a, p, n, auto)
            }
            None => (DEFAULT_RELAY.to_string(), "halimi".to_string(), String::new(), "bố".to_string(), false),
        };
        Self {
            rt: tokio::runtime::Runtime::new().expect("runtime tokio"),
            shared: Arc::new(Shared::default()),
            screen: Screen::Login,
            relay_addr: relay,
            account,
            password,
            name,
            cmd_tx: None,
            input_tx: None,
            tex: None,
            last_frame_id: 0,
            session_full: true,
            current_device: String::new(),
            auto_login: auto,
            clip: arboard::Clipboard::new().ok(),
            last_clip: String::new(),
            last_clip_poll: Instant::now(),
            kb_sent: 0,
            last_kb_log: Instant::now(),
            poller_spawned: false,
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
    input_rx: UnboundedReceiver<ClientMessage>,
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
    mut input_rx: UnboundedReceiver<ClientMessage>,
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

    while let Some(msg) = input_rx.recv().await {
        if cs.control.send(&msg).await.is_err() {
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
    mut input_rx: UnboundedReceiver<ClientMessage>,
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

    while let Some(msg) = input_rx.recv().await {
        if writer.send(&msg).await.is_err() {
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

/// Mở một kết nối ngắn tới relay, đăng nhập và lấy danh sách máy mới nhất.
async fn fetch_devices(
    endpoint: &quinn::Endpoint,
    addr: std::net::SocketAddr,
    account: &str,
) -> anyhow::Result<Vec<DeviceInfo>> {
    let conn = endpoint.connect(addr, "mm")?.await?;
    let (mut cs, mut cr) = conn.open_bi().await?;
    relay_send(&mut cs, &RelayMessage::LoginViewer { account: account.to_string() }).await?;
    match relay_recv(&mut cr).await? {
        RelayMessage::Devices(list) => Ok(list),
        other => anyhow::bail!("chờ Devices, nhận {other:?}"),
    }
}

/// Nền: cứ vài giây hỏi lại relay để danh bạ "sống" (máy mới hiện, máy tắt
/// chuyển offline) mà không cần nâng cấp server.
async fn device_poller(
    relay_addr: String,
    account: String,
    shared: Arc<Shared>,
    ctx: egui::Context,
) {
    let addr: std::net::SocketAddr = match relay_addr.trim().parse() {
        Ok(a) => a,
        Err(_) => return,
    };
    let endpoint = match mm_quic::client_endpoint() {
        Ok(e) => e,
        Err(_) => return,
    };
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(4)).await;
        if !shared.logged_in.load(Ordering::SeqCst) {
            continue;
        }
        if let Ok(list) = fetch_devices(&endpoint, addr, &account).await {
            let changed = {
                let mut cur = shared.devices.lock().unwrap();
                if *cur != list {
                    *cur = list;
                    true
                } else {
                    false
                }
            };
            if changed {
                ctx.request_repaint();
            }
        }
    }
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
        Delete => 0x2E, Insert => 0x2D, Home => 0x24, End => 0x23, PageUp => 0x21, PageDown => 0x22,
        ArrowLeft => 0x25, ArrowUp => 0x26, ArrowRight => 0x27, ArrowDown => 0x28,
        // Phím chức năng F1..F24
        F1 => 0x70, F2 => 0x71, F3 => 0x72, F4 => 0x73, F5 => 0x74, F6 => 0x75,
        F7 => 0x76, F8 => 0x77, F9 => 0x78, F10 => 0x79, F11 => 0x7A, F12 => 0x7B,
        F13 => 0x7C, F14 => 0x7D, F15 => 0x7E, F16 => 0x7F, F17 => 0x80, F18 => 0x81,
        F19 => 0x82, F20 => 0x83, F21 => 0x84, F22 => 0x85, F23 => 0x86, F24 => 0x87,
        // Dấu câu (bố cục US)
        Minus => 0xBD, Plus | Equals => 0xBB, Comma => 0xBC, Period => 0xBE,
        Slash | Questionmark => 0xBF, Semicolon | Colon => 0xBA, Quote => 0xDE,
        OpenBracket => 0xDB, CloseBracket => 0xDD, Backslash | Pipe => 0xDC, Backtick => 0xC0,
        // Lệnh copy/cut/paste (phòng khi egui phát ra dạng lệnh)
        Copy => 0x43, Paste => 0x56, Cut => 0x58,
        _ => return None,
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

/// Vẽ biểu tượng cột sóng (4 cột cao dần) như Radmin VPN.
fn paint_signal(p: &egui::Painter, center: egui::Pos2, online: bool, pulse: f32) {
    let bars = 4;
    let bw = 3.0;
    let gap = 2.4;
    let total = bars as f32 * bw + (bars - 1) as f32 * gap;
    let x0 = center.x - total / 2.0;
    let base_y = center.y + 8.0;
    for i in 0..bars {
        let h = 4.0 + i as f32 * 3.6;
        let x = x0 + i as f32 * (bw + gap);
        let rect = egui::Rect::from_min_max(egui::pos2(x, base_y - h), egui::pos2(x + bw, base_y));
        let col = if online {
            mix(GREEN, Color32::from_rgb(0x1A, 0x4A, 0x33), (1.0 - pulse) * 0.35)
        } else {
            Color32::from_rgb(0x3C, 0x45, 0x55)
        };
        p.rect_filled(rect, Rounding::same(1.0), col);
    }
}

/// Vẽ nút nguồn tròn kiểu Radmin VPN (ON = teal sáng).
fn paint_power(p: &egui::Painter, center: egui::Pos2, r: f32, on: bool) {
    let base = if on { TEAL } else { CARD2 };
    // quầng sáng
    if on {
        p.circle_filled(center, r + 5.0, Color32::from_rgba_unmultiplied(0x17, 0xB6, 0xC9, 40));
    }
    // vành ngoài gradient (2 lớp)
    p.circle_filled(center, r, if on { TEAL_DK } else { BORDER });
    p.circle_filled(center - egui::vec2(0.0, r * 0.12), r * 0.9, base);
    // ký hiệu nguồn: vòng khuyết + vạch đứng
    let glyph = if on { WHITE } else { MUTED };
    p.circle_stroke(center, r * 0.42, Stroke::new(r * 0.12, glyph));
    // che đỉnh vòng để tạo khe
    p.circle_filled(egui::pos2(center.x, center.y - r * 0.42), r * 0.16, base);
    // vạch đứng
    p.line_segment(
        [egui::pos2(center.x, center.y - r * 0.58), egui::pos2(center.x, center.y - r * 0.12)],
        Stroke::new(r * 0.12, glyph),
    );
}

/// Một dòng máy kiểu Radmin VPN: cột sóng + tên + IP. Chuột phải ra menu.
/// Trả về Some(true)=điều khiển hoàn toàn, Some(false)=chỉ xem.
fn device_row(ui: &mut egui::Ui, d: &DeviceInfo, idx: usize, pulse: f32) -> Option<bool> {
    let w = ui.available_width();
    let sense = if d.online { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 40.0), sense);
    let hovered = resp.hovered() && d.online;
    let base = if idx % 2 == 0 { BG } else { ROW_ALT };
    let bg = if hovered { HOVER } else { base };
    let p = ui.painter();
    p.rect_filled(rect, Rounding::ZERO, bg);
    if hovered {
        let bar =
            egui::Rect::from_min_max(rect.left_top(), egui::pos2(rect.left() + 3.0, rect.bottom()));
        p.rect_filled(bar, Rounding::ZERO, ACCENT);
    }
    // cột sóng (xanh = online)
    paint_signal(p, egui::pos2(rect.left() + 24.0, rect.center().y), d.online, pulse);
    // tên máy
    p.text(
        egui::pos2(rect.left() + 46.0, rect.center().y),
        Align2::LEFT_CENTER,
        &d.device_name,
        FontId::new(14.0, sbfam()),
        if d.online { TEXT } else { MUTED },
    );
    // trạng thái bên phải
    p.text(
        egui::pos2(rect.right() - 14.0, rect.center().y),
        Align2::RIGHT_CENTER,
        if d.online { "● trực tuyến" } else { "ngoại tuyến" },
        FontId::new(11.5, FontFamily::Proportional),
        if d.online { mix(GREEN, MUTED, 0.2) } else { MUTED },
    );
    // kẻ mảnh dưới dòng
    p.line_segment(
        [egui::pos2(rect.left(), rect.bottom()), egui::pos2(rect.right(), rect.bottom())],
        Stroke::new(1.0, mix(BORDER, bg, 0.35)),
    );

    if !d.online {
        return None;
    }
    let mut choice: Option<bool> = None;
    if resp.double_clicked() {
        choice = Some(true);
    }
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    resp.context_menu(|ui| {
        ui.set_min_width(210.0);
        ui.label(sb(&d.device_name).size(13.5).color(TEXT));
        ui.separator();
        if ui.button(sb("🖱  Điều khiển hoàn toàn")).clicked() {
            choice = Some(true);
            ui.close_menu();
        }
        if ui.button(sb("👁  Chỉ xem")).clicked() {
            choice = Some(false);
            ui.close_menu();
        }
        ui.separator();
        ui.add_enabled(false, egui::Button::new(RichText::new("Truyền tệp (sắp có)").color(MUTED)));
        ui.add_enabled(false, egui::Button::new(RichText::new("Nhắn tin (sắp có)").color(MUTED)));
    });
    choice
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
        kbhook::install();
        // Lần mở đầu: nếu đã lưu mật khẩu -> tự kết nối luôn (như Radmin VPN).
        if self.auto_login {
            self.auto_login = false;
            if !self.password.trim().is_empty() && !self.account.trim().is_empty() {
                self.start_login(ctx);
            }
        }
        match self.screen {
            Screen::Login => {
                KB_CAPTURE.store(false, Ordering::Relaxed);
                self.login_ui(ctx)
            }
            Screen::Directory => {
                KB_CAPTURE.store(false, Ordering::Relaxed);
                self.directory_ui(ctx)
            }
            Screen::Session => self.session_ui(ctx),
        }
    }
}

impl App {
    fn login_ui(&mut self, ctx: &egui::Context) {
        let frame = egui::Frame::none().fill(BG).inner_margin(egui::Margin::same(18.0));
        egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
            let avail_h = ui.available_height();
            ui.add_space((avail_h * 0.06).min(40.0));
            ui.vertical_centered(|ui| {
                ui.set_max_width(320.0);
                let card = egui::Frame::none()
                    .fill(CARD)
                    .rounding(Rounding::same(16.0))
                    .stroke(Stroke::new(1.0, BORDER))
                    .inner_margin(egui::Margin::same(20.0));
                card.show(ui, |ui| {
                    ui.set_width(276.0);
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
        // nhớ cho lần sau
        save_config(&self.relay_addr, &self.account, &self.password, &self.name);
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
        // Nền làm mới danh bạ mỗi vài giây (chỉ spawn một lần).
        if !self.poller_spawned {
            self.poller_spawned = true;
            self.rt.spawn(device_poller(
                self.relay_addr.clone(),
                self.account.clone(),
                self.shared.clone(),
                ctx.clone(),
            ));
        }
        self.screen = Screen::Directory;
    }

    fn directory_ui(&mut self, ctx: &egui::Context) {
        let logged = self.shared.logged_in.load(Ordering::SeqCst);
        let devices = self.shared.devices.lock().unwrap().clone();
        let online = devices.iter().filter(|d| d.online).count();
        let err = self.shared.err.lock().unwrap().clone();
        let status = self.shared.status.lock().unwrap().clone();
        let relay_addr = self.relay_addr.clone();
        let account = self.account.clone();

        // cờ xử lý sau khi vẽ (tránh mượn self trong closure)
        let mut do_logout = false;
        let mut do_quit = false;
        let mut go_login = false;
        let mut to_connect: Option<(String, bool)> = None;

        // ---- thanh menu: Hệ thống / Mạng / Trợ giúp ----
        let menuframe = egui::Frame::none()
            .fill(CARD)
            .inner_margin(egui::Margin::symmetric(8.0, 3.0))
            .stroke(Stroke::new(1.0, BORDER));
        egui::TopBottomPanel::top("menubar").frame(menuframe).show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button(sb("Hệ thống"), |ui| {
                    if ui.button("➕  Cửa sổ mới (xem máy khác)").clicked() {
                        if let Ok(exe) = std::env::current_exe() {
                            let _ = std::process::Command::new(exe).spawn();
                        }
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Đăng xuất").clicked() {
                        do_logout = true;
                        ui.close_menu();
                    }
                    if ui.button("Thoát").clicked() {
                        do_quit = true;
                        ui.close_menu();
                    }
                });
                ui.menu_button(sb("Mạng"), |ui| {
                    ui.label(RichText::new(format!("Cầu nối: {relay_addr}")).size(12.5).color(MUTED));
                    ui.label(RichText::new(format!("Tài khoản: {account}")).size(12.5).color(MUTED));
                });
                ui.menu_button(sb("Trợ giúp"), |ui| {
                    ui.label(sb("MinMin Remote").color(TEXT));
                    ui.label(
                        RichText::new("Điều khiển máy từ xa · mã hoá đầu cuối")
                            .size(12.5)
                            .color(MUTED),
                    );
                });
            });
        });

        // ---- header: nút nguồn ON + tài khoản + trạng thái ----
        let headframe = egui::Frame::none()
            .fill(CARD)
            .inner_margin(egui::Margin::symmetric(16.0, 14.0))
            .stroke(Stroke::new(1.0, BORDER));
        egui::TopBottomPanel::top("powerbar").frame(headframe).show(ctx, |ui| {
            ui.horizontal(|ui| {
                let (prect, presp) = ui.allocate_exact_size(egui::vec2(58.0, 58.0), Sense::click());
                paint_power(ui.painter(), prect.center() - egui::vec2(0.0, 4.0), 22.0, logged);
                ui.painter().text(
                    egui::pos2(prect.center().x, prect.bottom() - 2.0),
                    Align2::CENTER_BOTTOM,
                    if logged { "ON" } else { "OFF" },
                    FontId::new(12.0, sbfam()),
                    if logged { TEAL } else { MUTED },
                );
                if presp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                    do_logout = true;
                }
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.add_space(4.0);
                    ui.label(sb(account.to_uppercase()).size(18.0).color(TEXT));
                    ui.add_space(3.0);
                    ui.horizontal(|ui| {
                        if logged {
                            ui.label(RichText::new("● Trực tuyến").size(12.5).color(GREEN));
                        } else {
                            ui.label(RichText::new("● Đang kết nối…").size(12.5).color(AMBER));
                        }
                        ui.label(
                            RichText::new(format!("· {online}/{} máy", devices.len()))
                                .size(12.5)
                                .color(MUTED),
                        );
                    });
                });
            });
        });

        // ---- thân: danh sách máy ----
        let frame = egui::Frame::none().fill(BG).inner_margin(egui::Margin::same(0.0));
        egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
            if !logged {
                ui.add_space(50.0);
                ui.vertical_centered(|ui| {
                    if let Some(e) = &err {
                        ui.label(RichText::new(format!("⚠  {e}")).color(RED).size(14.0));
                        ui.add_space(10.0);
                        if ui.button(sb("Về đăng nhập")).clicked() {
                            go_login = true;
                        }
                    } else {
                        ui.spinner();
                        ui.add_space(8.0);
                        ui.label(RichText::new(status.clone()).color(MUTED).size(14.0));
                    }
                });
                ctx.request_repaint_after(Duration::from_millis(120));
                return;
            }

            if devices.is_empty() {
                ui.add_space(60.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("🖥").size(34.0).color(MUTED));
                    ui.add_space(6.0);
                    ui.label(sb("Chưa có máy nào online").size(15.0).color(TEXT));
                    ui.add_space(2.0);
                    ui.label(
                        RichText::new("Mở MinMin Server trên máy con rồi chờ chút.")
                            .size(13.0)
                            .color(MUTED),
                    );
                });
                ctx.request_repaint_after(Duration::from_millis(400));
                return;
            }

            let pulse = {
                let t = ui.input(|i| i.time);
                0.5 + 0.5 * ((t * 2.0).sin() as f32)
            };
            egui::ScrollArea::vertical().show(ui, |ui| {
                for (i, d) in devices.iter().enumerate() {
                    if let Some(full) = device_row(ui, d, i, pulse) {
                        to_connect = Some((d.device_id.clone(), full));
                    }
                }
            });
            ctx.request_repaint_after(Duration::from_millis(40));
        });

        // ---- áp dụng hành động ----
        if do_quit {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if go_login {
            self.screen = Screen::Login;
        }
        if do_logout {
            self.do_logout();
        }
        if let Some((device_id, full)) = to_connect {
            let (tx, rx) = unbounded_channel::<ClientMessage>();
            self.input_tx = Some(tx);
            self.session_full = full;
            self.current_device = device_id.clone();
            self.tex = None;
            self.last_frame_id = 0;
            *self.shared.frame.lock().unwrap() = None;
            if let Some(cmd) = &self.cmd_tx {
                let _ = cmd.send(Cmd::Connect { device_id, full, input_rx: rx });
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
            self.screen = Screen::Session;
        }
    }

    fn do_logout(&mut self) {
        self.cmd_tx = None;
        self.shared.logged_in.store(false, Ordering::SeqCst);
        self.shared.devices.lock().unwrap().clear();
        self.screen = Screen::Login;
    }

    fn session_ui(&mut self, ctx: &egui::Context) {
        let active = self.shared.session_active.load(Ordering::SeqCst);
        let stats = *self.shared.stats.lock().unwrap();
        let title = self.shared.title.lock().unwrap().clone();
        let status = self.shared.status.lock().unwrap().clone();

        // Bật bắt phím khi đang điều khiển toàn phần. Việc "chỉ gửi khi cửa sổ
        // của mình đang foreground" được chính hook kiểm tra bằng OS (fg_is_ours),
        // đáng tin hơn cờ focused của egui (hay sai sau khi phóng to cửa sổ).
        KB_CAPTURE.store(active && self.session_full, Ordering::Relaxed);

        // Log chẩn đoán mỗi 3s khi đang điều khiển (để soi nếu bàn phím lỗi).
        if active && self.session_full && self.last_kb_log.elapsed() >= Duration::from_secs(3) {
            self.last_kb_log = Instant::now();
            dbglog(&format!(
                "[kb] hook_ok={} capture={} hits={} da_gui={} phim",
                kbhook::HOOK_OK.load(Ordering::Relaxed),
                KB_CAPTURE.load(Ordering::Relaxed),
                kbhook::HOOK_HITS.load(Ordering::Relaxed),
                self.kb_sent
            ));
        }

        // Đồng bộ clipboard: copy bên máy xem -> dán được ngay bên máy con.
        if active && self.session_full
            && self.last_clip_poll.elapsed() >= Duration::from_millis(500)
        {
            self.last_clip_poll = Instant::now();
            if let Some(cb) = &mut self.clip {
                if let Ok(txt) = cb.get_text() {
                    if !txt.is_empty() && txt != self.last_clip {
                        self.last_clip = txt.clone();
                        if let Some(tx) = &self.input_tx {
                            let _ = tx.send(ClientMessage::Clipboard(txt));
                        }
                    }
                }
            }
        }

        let mut power_action: Option<PowerAction> = None;
        let full = self.session_full;

        let topframe = egui::Frame::none()
            .fill(CARD)
            .inner_margin(egui::Margin::symmetric(14.0, 9.0))
            .stroke(Stroke::new(1.0, BORDER));
        egui::TopBottomPanel::top("sbar").frame(topframe).show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button(sb("←  Danh bạ")).clicked() {
                    self.input_tx = None; // đóng kênh input -> dừng phiên
                    self.shared.session_active.store(false, Ordering::SeqCst);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                        WIN_NARROW[0],
                        WIN_NARROW[1],
                    )));
                    self.screen = Screen::Directory;
                }
                ui.add_space(6.0);
                ui.label(
                    sb(if title.is_empty() { self.current_device.clone() } else { title })
                        .size(15.0)
                        .color(TEXT),
                );
                if self.session_full {
                    ui.label(RichText::new("· 🖱 Điều khiển").size(12.5).color(MUTED));
                } else {
                    ui.label(RichText::new("· 👁 Chỉ xem").size(12.5).color(AMBER));
                }
                if full {
                    ui.add_space(6.0);
                    ui.menu_button(sb("⏻ Nguồn"), |ui| {
                        ui.set_min_width(170.0);
                        if ui.button("🔒  Khóa máy").clicked() {
                            power_action = Some(PowerAction::Lock);
                            ui.close_menu();
                        }
                        if ui.button("🔁  Khởi động lại").clicked() {
                            power_action = Some(PowerAction::Restart);
                            ui.close_menu();
                        }
                        if ui.button("⏻  Tắt máy").clicked() {
                            power_action = Some(PowerAction::Shutdown);
                            ui.close_menu();
                        }
                    });
                }

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

        if let Some(a) = power_action {
            if let Some(tx) = &self.input_tx {
                let _ = tx.send(ClientMessage::Power(a));
            }
        }

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
        let mut kb_sent_now: u64 = 0;
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
                    if active && self.session_full {
                        if let Some(tx) = &self.input_tx {
                            // Bàn phím: lấy mã phím thô từ hook OS -> gửi nguyên
                            // sang máy con (đủ mọi phím/tổ hợp, không qua egui).
                            if let Ok(mut q) = KB_QUEUE.lock() {
                                for (vk, down) in q.drain(..) {
                                    let ev = if down {
                                        InputEvent::KeyDown { vk }
                                    } else {
                                        InputEvent::KeyUp { vk }
                                    };
                                    let _ = tx.send(ClientMessage::Input(ev));
                                    kb_sent_now += 1;
                                }
                            }
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
                                            let _ =
                                                tx.send(ClientMessage::Input(InputEvent::MouseMove { x, y }));
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
                                                let _ = tx.send(ClientMessage::Input(ev));
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
                                                    tx.send(ClientMessage::Input(InputEvent::MouseWheel { delta: d }));
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
        self.kb_sent += kb_sent_now;
        ctx.request_repaint();
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(WIN_NARROW)
            .with_min_inner_size([340.0, 460.0])
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
