//! mm-viewer — người điều khiển (bản CLI của P1).
//!
//! Kết nối tới Server, bắt tay mật khẩu, chào hỏi, rồi nhận luồng hình ở chế độ
//! Chỉ xem. P1 lưu vài khung đầu ra PNG và đo tốc độ khung hình để kiểm chứng
//! luồng chạy. Giao diện Tauri đầy đủ (danh bạ máy, tường màn hình, điều khiển
//! chuột/bàn phím) sẽ làm ở P2.

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{anyhow, Result};
use clap::Parser;
use mm_codec::{Decoder, EncodedFrame};
use mm_proto::{ClientMessage, FrameCodec, ServerMessage, SessionMode, PROTOCOL_VERSION};
use tokio::net::TcpStream;
use tracing::info;

#[derive(Parser, Debug)]
#[command(name = "mm-viewer", about = "MinMin Remote — người điều khiển (CLI)")]
struct Args {
    /// Địa chỉ Server.
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
    #[arg(long, default_value_t = mm_proto::DEFAULT_PORT)]
    port: u16,
    #[arg(long, env = "MM_PASSWORD")]
    password: String,
    /// Tên người điều khiển (ghi vào nhật ký Server).
    #[arg(long, default_value = "bố")]
    name: String,
    /// Số khung sẽ nhận rồi dừng (0 = chạy mãi).
    #[arg(long, default_value_t = 60)]
    frames: u64,
    /// Thư mục lưu các khung PNG đầu tiên (để xem thử).
    #[arg(long, default_value = "frames")]
    out: PathBuf,
    /// Số khung PNG lưu ra đĩa.
    #[arg(long, default_value_t = 3)]
    save: u64,
    /// Transport: quic (v2, khuyên dùng) hoặc tcp (P1).
    #[arg(long, default_value = "quic")]
    transport: String,
}

/// Giải mã payload một khung và lưu PNG nếu cần. Trả về true nếu là keyframe.
fn handle_payload(
    decoder: &mut Decoder,
    header: &mm_proto::FrameHeader,
    payload: &[u8],
    out: &std::path::Path,
    save_this: bool,
) -> Result<bool> {
    match header.codec {
        FrameCodec::Tiled => {
            let ef: EncodedFrame = mm_proto::decode(payload)?;
            let keyframe = ef.keyframe;
            let fb = decoder.apply(&ef)?;
            if save_this {
                save_png(fb, header.width, header.height, out, header.seq)?;
            }
            Ok(keyframe)
        }
        _ => {
            if save_this {
                let img = image::load_from_memory(payload)?;
                img.save(out.join(format!("frame_{:04}.png", header.seq)))?;
            }
            Ok(true)
        }
    }
}

/// Lưu framebuffer RGBA đầy đủ ra PNG.
fn save_png(rgba: &[u8], w: u32, h: u32, out: &std::path::Path, seq: u64) -> Result<()> {
    let img = image::RgbaImage::from_raw(w, h, rgba.to_vec())
        .ok_or_else(|| anyhow!("kích thước framebuffer không khớp"))?;
    img.save(out.join(format!("frame_{seq:04}.png")))?;
    Ok(())
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
    std::fs::create_dir_all(&args.out).ok();
    if args.transport == "tcp" {
        run_tcp(args).await
    } else {
        run_quic(args).await
    }
}

async fn run_quic(args: Args) -> Result<()> {
    let addr: std::net::SocketAddr = format!("{}:{}", args.host, args.port).parse()?;
    let endpoint = mm_quic::client_endpoint()?;
    info!("đang nối QUIC tới {addr}...");
    let mut session = mm_quic::connect(&endpoint, addr, &args.password).await?;

    session
        .control
        .send(&ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            viewer_name: args.name.clone(),
        })
        .await?;
    match session.control.recv::<ServerMessage>().await? {
        ServerMessage::Welcome { host_name, os, monitors, .. } => {
            info!("vào máy \"{host_name}\" ({os}), {} màn hình [QUIC]", monitors.len());
        }
        ServerMessage::Error { message } => return Err(anyhow!("server báo lỗi: {message}")),
        _ => return Err(anyhow!("chờ Welcome nhưng nhận gói khác")),
    }
    session.control.send(&ClientMessage::SetMode(SessionMode::ViewOnly)).await?;

    let start = Instant::now();
    let mut received = 0u64;
    let mut total_bytes = 0u64;
    let mut decoder = Decoder::new();

    while args.frames == 0 || received < args.frames {
        let (header_bytes, payload) = match mm_quic::recv_frame(&session.conn).await {
            Ok(v) => v,
            Err(_) => break,
        };
        total_bytes += (header_bytes.len() + payload.len()) as u64;
        let header: mm_proto::FrameHeader = mm_proto::decode(&header_bytes)?;
        let seq = header.seq;
        let key = handle_payload(&mut decoder, &header, &payload, &args.out, received < args.save)?;
        if received < args.save || key {
            info!("khung {seq}: {} byte{}", payload.len(), if key { " (keyframe)" } else { "" });
        }
        received += 1;
        session.control.send(&ClientMessage::Ack { frame_seq: seq }).await.ok();
    }

    session.control.send(&ClientMessage::Bye).await.ok();
    report(start, received, total_bytes);
    Ok(())
}

async fn run_tcp(args: Args) -> Result<()> {
    let psk = mm_crypto::derive_psk(&args.password)?;
    let addr = format!("{}:{}", args.host, args.port);
    let mut stream = TcpStream::connect(&addr).await?;
    stream.set_nodelay(true).ok();
    info!("đã nối tới {addr}, đang bắt tay...");

    let session = mm_transport::handshake_initiator(&mut stream, &psk).await?;
    let (mut reader, mut writer) = mm_transport::secure_split(stream, session);

    writer
        .send(&ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            viewer_name: args.name.clone(),
        })
        .await?;

    match reader.recv::<ServerMessage>().await? {
        ServerMessage::Welcome { host_name, os, monitors, .. } => {
            info!("vào máy \"{host_name}\" ({os}), {} màn hình", monitors.len());
        }
        ServerMessage::Error { message } => return Err(anyhow!("server báo lỗi: {message}")),
        _ => return Err(anyhow!("chờ Welcome nhưng nhận gói khác")),
    }

    writer.send(&ClientMessage::SetMode(SessionMode::ViewOnly)).await?;

    let start = Instant::now();
    let mut received = 0u64;
    let mut total_bytes = 0u64;
    let mut pending_header: Option<mm_proto::FrameHeader> = None;
    let mut decoder = Decoder::new();

    loop {
        if args.frames != 0 && received >= args.frames {
            break;
        }
        match reader.recv::<ServerMessage>().await? {
            ServerMessage::FrameHeader(h) => pending_header = Some(h),
            ServerMessage::FrameData { seq, bytes } => {
                total_bytes += bytes.len() as u64;
                if let Some(h) = pending_header.take() {
                    match h.codec {
                        FrameCodec::Tiled => {
                            let ef: EncodedFrame = mm_proto::decode(&bytes)?;
                            let ntiles = ef.tiles.len();
                            let fb = decoder.apply(&ef)?;
                            if received < args.save {
                                save_png(fb, h.width, h.height, &args.out, seq)?;
                            }
                            if received < args.save || ef.keyframe {
                                info!(
                                    "khung {seq}: {} ô đổi, {} byte{}",
                                    ntiles,
                                    bytes.len(),
                                    if ef.keyframe { " (keyframe)" } else { "" }
                                );
                            }
                        }
                        FrameCodec::Jpeg | FrameCodec::RawRgba | FrameCodec::H264 => {
                            if received < args.save {
                                let img = image::load_from_memory(&bytes)?;
                                img.save(args.out.join(format!("frame_{seq:04}.png")))?;
                            }
                        }
                    }
                }
                received += 1;
                writer.send(&ClientMessage::Ack { frame_seq: seq }).await?;
            }
            ServerMessage::Welcome { .. } => {} // chào lại giữa luồng: bỏ qua
            ServerMessage::Error { message } => return Err(anyhow!("server báo lỗi: {message}")),
            ServerMessage::Closed { reason } => {
                info!("server đóng phiên: {reason}");
                break;
            }
        }
    }

    writer.send(&ClientMessage::Bye).await.ok();
    report(start, received, total_bytes);
    Ok(())
}

fn report(start: Instant, received: u64, total_bytes: u64) {
    let secs = start.elapsed().as_secs_f64();
    let fps = received as f64 / secs.max(0.0001);
    let avg_kb = total_bytes as f64 / received.max(1) as f64 / 1024.0;
    println!(
        "Nhận {received} khung trong {secs:.2}s = {fps:.1} khung/giây, trung bình {avg_kb:.1} KB/khung"
    );
}
