//! mm-server — máy bị điều khiển (chạy nền, không giao diện).
//!
//! P1: lắng nghe TCP, bắt tay Noise (mật khẩu), chào hỏi, rồi stream màn hình
//! ở chế độ Chỉ xem. Mặc định dùng nguồn chụp giả để kiểm thử trên mọi hệ điều
//! hành; trên Windows thêm cờ `--real` để dùng Desktop Duplication (DXGI).
//!
//! Chạy hoàn toàn ẩn: không icon, không thông báo — đúng yêu cầu điều khiển
//! máy ảo và server nội bộ của bố.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::Parser;
use mm_capture::{encode_jpeg, MockCapture, MockDesktop, ScreenCapture};
use mm_codec::Encoder;
use mm_proto::{
    ClientMessage, FrameCodec, FrameHeader, ServerMessage, SessionMode, PROTOCOL_VERSION,
};
use tokio::net::{TcpListener, TcpStream};
use tracing::{info, warn};

#[derive(Parser, Debug, Clone)]
#[command(name = "mm-server", about = "MinMin Remote — máy bị điều khiển")]
struct Args {
    /// Địa chỉ lắng nghe.
    #[arg(long, default_value = "0.0.0.0")]
    host: String,
    /// Cổng lắng nghe.
    #[arg(long, default_value_t = mm_proto::DEFAULT_PORT)]
    port: u16,
    /// Mật khẩu truy cập. Nên truyền qua biến môi trường MM_PASSWORD.
    #[arg(long, env = "MM_PASSWORD")]
    password: String,
    /// Số khung hình mỗi giây.
    #[arg(long, default_value_t = 15)]
    fps: u32,
    /// Chất lượng JPEG (1..=100).
    #[arg(long, default_value_t = 70)]
    quality: u8,
    /// (Cũ, không cần nữa) ép quay màn hình thật. Windows nay mặc định đã bật.
    #[arg(long, default_value_t = false)]
    real: bool,
    /// Dùng ảnh demo giả để kiểm thử thay vì quay màn hình thật.
    #[arg(long, default_value_t = false)]
    mock: bool,
    /// Kích thước nguồn giả khi dùng --mock.
    #[arg(long, default_value = "1280x720")]
    mock_size: String,
    /// Kiểu nguồn giả: desktop (phần lớn tĩnh) hoặc moving (đổi toàn màn hình).
    #[arg(long, default_value = "desktop")]
    mock_style: String,
    /// Codec: tiled (chỉ gửi ô thay đổi, v2) hoặc jpeg (cả khung, P1 cũ).
    #[arg(long, default_value = "tiled")]
    codec: String,
    /// Transport: quic (v2, khuyên dùng) hoặc tcp (P1).
    #[arg(long, default_value = "quic")]
    transport: String,
    /// Địa chỉ máy chủ cầu nối (mm-relay). Có giá trị = chạy chế độ relay.
    #[arg(long)]
    relay: Option<String>,
    /// Tài khoản (nhóm máy) khi dùng relay.
    #[arg(long, default_value = "halimi")]
    account: String,
    /// Mã máy (duy nhất trong tài khoản) khi dùng relay.
    #[arg(long, default_value = "may-con-1")]
    device_id: String,
    /// Tên máy hiển thị trong danh bạ.
    #[arg(long, default_value = "Máy con")]
    device_name: String,
}

/// Nén một khung theo codec đã chọn, trả về (header, payload).
fn encode_frame(
    encoder: &mut Encoder,
    frame: &mm_capture::RgbaFrame,
    seq: u64,
    mon: u32,
    use_tiled: bool,
    quality: u8,
) -> Result<(FrameHeader, Vec<u8>)> {
    let (codec, keyframe, bytes) = if use_tiled {
        let enc = encoder.encode(frame)?;
        (FrameCodec::Tiled, enc.keyframe, mm_proto::encode(&enc)?)
    } else {
        (FrameCodec::Jpeg, true, encode_jpeg(frame, quality)?)
    };
    Ok((
        FrameHeader { seq, monitor: mon, width: frame.width, height: frame.height, codec, keyframe },
        bytes,
    ))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();
    if args.relay.is_some() {
        run_relay(args).await
    } else if args.transport == "tcp" {
        run_tcp(args).await
    } else {
        run_quic(args).await
    }
}

/// Chế độ relay: đăng ký lên mm-relay, báo danh online, và khi có viewer muốn
/// nối thì mở ống data qua relay rồi phục vụ y như một kết nối trực tiếp.
async fn run_relay(args: Args) -> Result<()> {
    use mm_proto::RelayMessage;
    let relay_addr: std::net::SocketAddr = args.relay.clone().unwrap().parse()?;
    let psk = mm_crypto::derive_psk(&args.password)?;
    // Endpoint hai chiều: cùng socket cho relay và cho đục NAT nối thẳng.
    let endpoint = mm_quic::dual_endpoint("0.0.0.0:0".parse()?)?;
    let conn = endpoint.connect(relay_addr, "mm")?.await?;
    info!("đã nối tới relay {relay_addr}");

    let (mut cs, mut cr) = conn.open_bi().await?;
    relay_send(&mut cs, &RelayMessage::RegisterDevice {
        account: args.account.clone(),
        device_id: args.device_id.clone(),
        device_name: args.device_name.clone(),
    })
    .await?;
    match relay_recv(&mut cr).await? {
        RelayMessage::Registered => info!("đã đăng ký máy \"{}\" lên relay", args.device_name),
        other => warn!("relay trả lời bất ngờ: {:?}", other),
    }

    // Heartbeat giữ kết nối.
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(20)).await;
            if relay_send(&mut cs, &RelayMessage::Heartbeat).await.is_err() {
                break;
            }
        }
    });

    // Chờ thông báo có viewer muốn nối → mở ống data và phục vụ.
    loop {
        match relay_recv(&mut cr).await {
            Ok(RelayMessage::IncomingSession { session, viewer, peer_addr }) => {
                info!("có \"{viewer}\" muốn nối (phiên {session}), thử đục NAT tới {peer_addr}");
                let conn = conn.clone();
                let endpoint = endpoint.clone();
                let args = args.clone();
                tokio::spawn(async move {
                    // Thử nối thẳng (đục NAT) trước; hỏng thì quay về chạy qua relay.
                    if let Ok(peer) = peer_addr.parse::<std::net::SocketAddr>() {
                        match mm_quic::punch_accept(&endpoint, peer, Duration::from_secs(5)).await {
                            Ok(direct) => {
                                info!("phiên {session}: NỐI THẲNG được (không qua relay)");
                                match mm_quic::server_auth(direct, &args.password).await {
                                    Ok(sess) => {
                                        if let Err(e) = handle_conn_quic(sess, args).await {
                                            warn!("phiên thẳng {session} lỗi: {e}");
                                        }
                                        return;
                                    }
                                    Err(e) => warn!("auth phiên thẳng {session} lỗi: {e}"),
                                }
                            }
                            Err(e) => info!("phiên {session}: đục NAT không được ({e}), dùng relay"),
                        }
                    }
                    if let Err(e) = serve_relay_session(conn, session, psk, args).await {
                        warn!("phiên relay {session} lỗi: {e}");
                    }
                });
            }
            Ok(_) => {}
            Err(e) => {
                warn!("mất kết nối relay: {e}");
                break;
            }
        }
    }
    Ok(())
}

async fn serve_relay_session(
    conn: quinn::Connection,
    session: u64,
    psk: [u8; 32],
    args: Args,
) -> Result<()> {
    let (mut send, recv) = conn.open_bi().await?;
    // Khung đầu = số phiên để relay ghép đúng.
    mm_transport::write_frame(&mut send, &session.to_be_bytes()).await?;
    let stream = tokio::io::join(recv, send);
    serve_stream(stream, psk, args).await
}

async fn relay_send(s: &mut quinn::SendStream, m: &mm_proto::RelayMessage) -> Result<()> {
    mm_transport::write_frame(s, &mm_proto::encode(m)?).await?;
    Ok(())
}

async fn relay_recv(r: &mut quinn::RecvStream) -> Result<mm_proto::RelayMessage> {
    let bytes = mm_transport::read_frame(r).await?;
    Ok(mm_proto::decode(&bytes)?)
}

async fn run_tcp(args: Args) -> Result<()> {
    let psk = mm_crypto::derive_psk(&args.password)?;
    let addr = format!("{}:{}", args.host, args.port);
    let listener = TcpListener::bind(&addr).await?;
    info!("MinMin Remote server (TCP) đang lắng nghe tại {addr}");

    loop {
        let (stream, peer) = listener.accept().await?;
        let args = args.clone();
        tokio::spawn(async move {
            info!("kết nối mới từ {peer}");
            if let Err(e) = handle_conn(stream, psk, args).await {
                warn!("phiên {peer} kết thúc: {e}");
            } else {
                info!("phiên {peer} đã đóng");
            }
        });
    }
}

async fn run_quic(args: Args) -> Result<()> {
    let addr: std::net::SocketAddr = format!("{}:{}", args.host, args.port).parse()?;
    let endpoint = mm_quic::server_endpoint(addr)?;
    info!("MinMin Remote server (QUIC) đang lắng nghe tại {addr}");
    loop {
        let password = args.password.clone();
        match mm_quic::accept(&endpoint, &password).await {
            Ok(session) => {
                let peer = session.conn.remote_address();
                let args = args.clone();
                tokio::spawn(async move {
                    info!("kết nối QUIC từ {peer} — đã xác thực");
                    if let Err(e) = handle_conn_quic(session, args).await {
                        warn!("phiên {peer} kết thúc: {e}");
                    } else {
                        info!("phiên {peer} đã đóng");
                    }
                });
            }
            Err(e) => warn!("từ chối một kết nối: {e}"),
        }
    }
}

async fn handle_conn_quic(mut session: mm_quic::ServerSession, args: Args) -> Result<()> {
    // Hello/Welcome trên control stream.
    let hello: ClientMessage = session.control.recv().await?;
    let viewer_name = match hello {
        ClientMessage::Hello { protocol_version, viewer_name } if protocol_version == PROTOCOL_VERSION => {
            viewer_name
        }
        ClientMessage::Hello { protocol_version, .. } => {
            session
                .control
                .send(&ServerMessage::Error {
                    message: format!("khác phiên bản giao thức: {PROTOCOL_VERSION} vs {protocol_version}"),
                })
                .await?;
            return Ok(());
        }
        _ => {
            session
                .control
                .send(&ServerMessage::Error { message: "cần Hello trước".into() })
                .await?;
            return Ok(());
        }
    };
    info!("người điều khiển \"{viewer_name}\" vào phiên (QUIC)");

    let mut cap = make_capture(&args)?;
    let monitors = cap.monitors();
    session
        .control
        .send(&ServerMessage::Welcome {
            protocol_version: PROTOCOL_VERSION,
            host_name: hostname(),
            os: std::env::consts::OS.to_string(),
            monitors,
        })
        .await?;

    // Task đọc control: SetMode/Ack/Bye.
    let stop = Arc::new(AtomicBool::new(false));
    let monitor = Arc::new(std::sync::atomic::AtomicU32::new(0));
    {
        let stop = stop.clone();
        let monitor = monitor.clone();
        let mut control = session.control;
        tokio::spawn(async move {
            let mut injector = mm_input::default_injector();
            let mut allow_input = false;
            loop {
                match control.recv::<ClientMessage>().await {
                    Ok(ClientMessage::Bye) | Err(_) => {
                        stop.store(true, Ordering::Relaxed);
                        break;
                    }
                    Ok(ClientMessage::SetMode(mode)) => {
                        allow_input = matches!(mode, SessionMode::FullControl);
                    }
                    Ok(ClientMessage::SelectMonitor { index }) => {
                        monitor.store(index, Ordering::Relaxed);
                    }
                    Ok(ClientMessage::Input(ev)) => {
                        if allow_input {
                            if let Err(e) = injector.inject(&ev) {
                                warn!("tiêm input lỗi: {e}");
                            }
                        }
                    }
                    Ok(_) => {}
                }
            }
        });
    }

    // Vòng stream hình: mỗi khung một uni-stream.
    let use_tiled = args.codec != "jpeg";
    let mut encoder = Encoder::new(args.quality);
    let frame_interval = Duration::from_millis((1000 / args.fps.max(1)) as u64);
    let mut seq = 0u64;
    while !stop.load(Ordering::Relaxed) {
        let t0 = Instant::now();
        let mon = monitor.load(Ordering::Relaxed);
        if let Some(frame) = cap.grab(mon)? {
            let (header, payload) =
                encode_frame(&mut encoder, &frame, seq, mon, use_tiled, args.quality)?;
            let header_bytes = mm_proto::encode(&header)?;
            if let Err(e) = mm_quic::send_frame(&session.conn, &header_bytes, &payload).await {
                warn!("gửi khung lỗi: {e}");
                break;
            }
            seq += 1;
        }
        let elapsed = t0.elapsed();
        if elapsed < frame_interval {
            tokio::time::sleep(frame_interval - elapsed).await;
        }
    }
    Ok(())
}

fn parse_size(s: &str) -> (u32, u32) {
    let mut it = s.split(['x', 'X']);
    let w = it.next().and_then(|v| v.parse().ok()).unwrap_or(1280);
    let h = it.next().and_then(|v| v.parse().ok()).unwrap_or(720);
    (w, h)
}

fn make_capture(args: &Args) -> Result<Box<dyn ScreenCapture>> {
    // Mặc định quay màn hình THẬT trên Windows; chỉ dùng ảnh giả khi có --mock.
    #[cfg(windows)]
    {
        if !args.mock {
            return Ok(Box::new(mm_capture::gdi::GdiCapture::new()?));
        }
    }
    #[cfg(not(windows))]
    {
        if args.real && !args.mock {
            anyhow::bail!("--real chỉ hỗ trợ trên Windows; bỏ cờ này để dùng nguồn giả");
        }
    }
    let (w, h) = parse_size(&args.mock_size);
    if args.mock_style == "moving" {
        Ok(Box::new(MockCapture::new(w, h)))
    } else {
        Ok(Box::new(MockDesktop::new(w, h)))
    }
}

async fn handle_conn(mut stream: TcpStream, psk: [u8; 32], args: Args) -> Result<()> {
    stream.set_nodelay(true).ok();
    serve_stream(stream, psk, args).await
}

/// Phục vụ một phiên (Noise + chỉ xem/điều khiển) trên BẤT KỲ ống nào:
/// TcpStream (LAN trực tiếp) hoặc ống data do relay bắc cầu (qua Internet).
async fn serve_stream<S>(mut stream: S, psk: [u8; 32], args: Args) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    // Bắt tay Noise: sai mật khẩu sẽ hỏng ngay tại đây.
    let session = mm_transport::handshake_responder(&mut stream, &psk).await?;
    let (mut reader, mut writer) = mm_transport::secure_split(stream, session);

    // Chờ Hello.
    let hello: ClientMessage = reader.recv().await?;
    let viewer_name = match hello {
        ClientMessage::Hello { protocol_version, viewer_name } => {
            if protocol_version != PROTOCOL_VERSION {
                writer
                    .send(&ServerMessage::Error {
                        message: format!(
                            "khác phiên bản giao thức: server {PROTOCOL_VERSION}, viewer {protocol_version}"
                        ),
                    })
                    .await?;
                return Ok(());
            }
            viewer_name
        }
        _ => {
            writer
                .send(&ServerMessage::Error { message: "cần Hello trước".into() })
                .await?;
            return Ok(());
        }
    };
    // NHẬT KÝ KẾT NỐI: ai, lúc nào.
    info!("đã xác thực — người điều khiển \"{viewer_name}\" vào phiên");

    let mut cap = make_capture(&args)?;
    let monitors = cap.monitors();
    writer
        .send(&ServerMessage::Welcome {
            protocol_version: PROTOCOL_VERSION,
            host_name: hostname(),
            os: std::env::consts::OS.to_string(),
            monitors: monitors.clone(),
        })
        .await?;

    // Trạng thái điều khiển chia sẻ với task đọc.
    let stop = Arc::new(AtomicBool::new(false));
    let monitor = Arc::new(std::sync::atomic::AtomicU32::new(0));
    {
        let stop = stop.clone();
        let monitor = monitor.clone();
        tokio::spawn(async move {
            let mut injector = mm_input::default_injector();
            let mut allow_input = false;
            loop {
                match reader.recv::<ClientMessage>().await {
                    Ok(ClientMessage::Bye) | Err(_) => {
                        stop.store(true, Ordering::Relaxed);
                        break;
                    }
                    Ok(ClientMessage::SetMode(mode)) => {
                        allow_input = matches!(mode, SessionMode::FullControl);
                    }
                    Ok(ClientMessage::SelectMonitor { index }) => {
                        monitor.store(index, Ordering::Relaxed);
                    }
                    Ok(ClientMessage::Input(ev)) => {
                        if allow_input {
                            if let Err(e) = injector.inject(&ev) {
                                warn!("tiêm input lỗi: {e}");
                            }
                        }
                    }
                    Ok(_) => {}
                }
            }
        });
    }

    // Vòng stream hình (Chỉ xem).
    let _mode = SessionMode::ViewOnly;
    let use_tiled = args.codec != "jpeg";
    let mut encoder = Encoder::new(args.quality);
    let frame_interval = Duration::from_millis((1000 / args.fps.max(1)) as u64);
    let mut seq = 0u64;
    while !stop.load(Ordering::Relaxed) {
        let t0 = Instant::now();
        let mon = monitor.load(Ordering::Relaxed);
        if let Some(frame) = cap.grab(mon)? {
            let (codec, keyframe, bytes) = if use_tiled {
                let enc = encoder.encode(&frame)?;
                (FrameCodec::Tiled, enc.keyframe, mm_proto::encode(&enc)?)
            } else {
                (FrameCodec::Jpeg, true, encode_jpeg(&frame, args.quality)?)
            };
            writer
                .send(&ServerMessage::FrameHeader(FrameHeader {
                    seq,
                    monitor: mon,
                    width: frame.width,
                    height: frame.height,
                    codec,
                    keyframe,
                }))
                .await?;
            writer
                .send(&ServerMessage::FrameData { seq, bytes })
                .await?;
            seq += 1;
        }
        let elapsed = t0.elapsed();
        if elapsed < frame_interval {
            tokio::time::sleep(frame_interval - elapsed).await;
        }
    }
    Ok(())
}

fn hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown".into())
}
