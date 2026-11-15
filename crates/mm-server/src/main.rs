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
    /// Dùng chụp màn hình thật (Windows). Bỏ trống = nguồn giả để kiểm thử.
    #[arg(long, default_value_t = false)]
    real: bool,
    /// Kích thước nguồn giả khi không dùng --real.
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
    if args.transport == "tcp" {
        run_tcp(args).await
    } else {
        run_quic(args).await
    }
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
            loop {
                match control.recv::<ClientMessage>().await {
                    Ok(ClientMessage::Bye) | Err(_) => {
                        stop.store(true, Ordering::Relaxed);
                        break;
                    }
                    Ok(ClientMessage::SelectMonitor { index }) => {
                        monitor.store(index, Ordering::Relaxed);
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
    if args.real {
        #[cfg(windows)]
        {
            return Ok(Box::new(mm_capture::dxgi::DxgiCapture::new()?));
        }
        #[cfg(not(windows))]
        {
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
            loop {
                match reader.recv::<ClientMessage>().await {
                    Ok(ClientMessage::Bye) | Err(_) => {
                        stop.store(true, Ordering::Relaxed);
                        break;
                    }
                    Ok(ClientMessage::SelectMonitor { index }) => {
                        monitor.store(index, Ordering::Relaxed);
                    }
                    // Ack / RequestFullFrame / SetMode: P1 mọi khung đều là keyframe.
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
