//! mm-relay — máy chủ cầu nối đặt trên VPS (có IP công khai).
//!
//! Hai việc:
//! 1. Thư mục: máy con đăng ký báo danh online theo tài khoản; viewer đăng nhập
//!    lấy danh bạ máy cùng tài khoản.
//! 2. Bắc cầu: khi viewer chọn một máy, relay ghép hai "ống data" (một của
//!    viewer, một của máy con) rồi **bơm byte hai chiều**. Bắt tay mật khẩu và
//!    mã hoá vẫn chạy đầu-cuối trên ống đó nên relay KHÔNG đọc được nội dung.
//!
//! Chế độ hiện tại là chuyển tiếp qua relay (chạy với mọi NAT). Đục NAT để nối
//! thẳng (bỏ relay khỏi đường dữ liệu) là bước tối ưu sau.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use clap::Parser;
use mm_proto::{DeviceInfo, RelayMessage};
use mm_transport::{read_frame, write_frame};
use quinn::{Connection, RecvStream, SendStream};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use tracing::{info, warn};

#[derive(Parser, Debug)]
#[command(name = "mm-relay", about = "MinMin Remote — máy chủ cầu nối")]
struct Args {
    #[arg(long, default_value = "0.0.0.0")]
    host: String,
    #[arg(long, default_value_t = 5000)]
    port: u16,
}

#[derive(Clone, Copy, PartialEq)]
enum Side {
    Device,
    Viewer,
}

struct Half {
    side: Side,
    send: SendStream,
    recv: RecvStream,
}

struct Device {
    account: String,
    name: String,
    push: UnboundedSender<RelayMessage>,
}

#[derive(Default)]
struct State {
    devices: Mutex<HashMap<String, Device>>,
    pending: Mutex<HashMap<u64, Half>>,
    session_seq: AtomicU64,
}

impl State {
    fn device_list(&self, account: &str) -> Vec<DeviceInfo> {
        self.devices
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, d)| d.account == account)
            .map(|(id, d)| DeviceInfo {
                device_id: id.clone(),
                device_name: d.name.clone(),
                online: true,
            })
            .collect()
    }
}

async fn send_msg(s: &mut SendStream, m: &RelayMessage) -> Result<()> {
    write_frame(s, &mm_proto::encode(m)?).await?;
    Ok(())
}

async fn recv_msg(r: &mut RecvStream) -> Result<RelayMessage> {
    let bytes = read_frame(r).await?;
    Ok(mm_proto::decode(&bytes)?)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args = Args::parse();
    let addr: std::net::SocketAddr = format!("{}:{}", args.host, args.port).parse()?;
    let endpoint = mm_quic::server_endpoint(addr)?;
    let state = Arc::new(State::default());
    info!("mm-relay đang lắng nghe tại {addr}");

    loop {
        let incoming = match endpoint.accept().await {
            Some(i) => i,
            None => break,
        };
        let state = state.clone();
        tokio::spawn(async move {
            match incoming.await {
                Ok(conn) => {
                    if let Err(e) = handle(conn, state).await {
                        warn!("kết nối kết thúc: {e}");
                    }
                }
                Err(e) => warn!("bắt tay QUIC lỗi: {e}"),
            }
        });
    }
    Ok(())
}

async fn handle(conn: Connection, state: Arc<State>) -> Result<()> {
    // Stream đầu tiên là control; gói đầu cho biết đây là máy con hay viewer.
    let (cs, mut cr) = conn.accept_bi().await?;
    let first = recv_msg(&mut cr).await?;
    match first {
        RelayMessage::RegisterDevice { account, device_id, device_name } => {
            run_device(conn, cs, cr, account, device_id, device_name, state).await
        }
        RelayMessage::LoginViewer { account } => {
            run_viewer(conn, cs, cr, account, state).await
        }
        _ => Err(anyhow!("gói mở đầu không hợp lệ")),
    }
}

async fn run_device(
    conn: Connection,
    mut cs: SendStream,
    mut cr: RecvStream,
    account: String,
    device_id: String,
    device_name: String,
    state: Arc<State>,
) -> Result<()> {
    let (push, mut push_rx) = unbounded_channel::<RelayMessage>();
    state.devices.lock().unwrap().insert(
        device_id.clone(),
        Device { account: account.clone(), name: device_name.clone(), push },
    );
    info!("máy con \"{device_name}\" ({device_id}) online [{account}]");
    send_msg(&mut cs, &RelayMessage::Registered).await?;

    // Task đẩy thông báo (IncomingSession) tới máy con.
    tokio::spawn(async move {
        while let Some(m) = push_rx.recv().await {
            if send_msg(&mut cs, &m).await.is_err() {
                break;
            }
        }
    });
    // Task đọc control từ máy con (Heartbeat); kết thúc khi ngắt.
    tokio::spawn(async move { while recv_msg(&mut cr).await.is_ok() {} });

    // Nhận các ống data do máy con mở, ghép theo số phiên.
    let res = accept_data_loop(&conn, Side::Device, &state).await;
    state.devices.lock().unwrap().remove(&device_id);
    info!("máy con \"{device_name}\" offline");
    res
}

async fn run_viewer(
    conn: Connection,
    mut cs: SendStream,
    mut cr: RecvStream,
    account: String,
    state: Arc<State>,
) -> Result<()> {
    send_msg(&mut cs, &RelayMessage::Devices(state.device_list(&account))).await?;

    // Task đọc yêu cầu ConnectTo từ viewer.
    let st = state.clone();
    let acc = account.clone();
    tokio::spawn(async move {
        loop {
            match recv_msg(&mut cr).await {
                Ok(RelayMessage::ConnectTo { device_id }) => {
                    let session = st.session_seq.fetch_add(1, Ordering::SeqCst) + 1;
                    let pushed = {
                        let devices = st.devices.lock().unwrap();
                        match devices.get(&device_id) {
                            Some(d) if d.account == acc => d
                                .push
                                .send(RelayMessage::IncomingSession {
                                    session,
                                    viewer: "viewer".into(),
                                })
                                .is_ok(),
                            _ => false,
                        }
                    };
                    let reply = if pushed {
                        RelayMessage::SessionReady { session }
                    } else {
                        RelayMessage::RelayError { message: "máy không online".into() }
                    };
                    if send_msg(&mut cs, &reply).await.is_err() {
                        break;
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });

    accept_data_loop(&conn, Side::Viewer, &state).await
}

/// Nhận các ống data trên một kết nối; mỗi ống mang 1 khung đầu là số phiên.
async fn accept_data_loop(conn: &Connection, side: Side, state: &Arc<State>) -> Result<()> {
    loop {
        let (send, mut recv) = match conn.accept_bi().await {
            Ok(s) => s,
            Err(_) => return Ok(()), // kết nối đóng
        };
        // Khung đầu = số phiên (u64 big-endian).
        let head = match read_frame(&mut recv).await {
            Ok(h) if h.len() == 8 => h,
            _ => continue,
        };
        let session = u64::from_be_bytes(head.try_into().unwrap());
        pair(state, session, Half { side, send, recv });
    }
}

/// Ghép hai nửa cùng số phiên rồi bơm byte hai chiều.
fn pair(state: &Arc<State>, session: u64, half: Half) {
    let mut pending = state.pending.lock().unwrap();
    if let Some(other) = pending.remove(&session) {
        drop(pending);
        let (viewer, device) = match half.side {
            Side::Viewer => (half, other),
            Side::Device => (other, half),
        };
        // viewer.recv -> device.send
        let mut vr = viewer.recv;
        let mut ds = device.send;
        tokio::spawn(async move {
            let _ = tokio::io::copy(&mut vr, &mut ds).await;
            let _ = ds.finish();
        });
        // device.recv -> viewer.send
        let mut dr = device.recv;
        let mut vs = viewer.send;
        tokio::spawn(async move {
            let _ = tokio::io::copy(&mut dr, &mut vs).await;
            let _ = vs.finish();
        });
        info!("đã ghép phiên {session}");
    } else {
        pending.insert(session, half);
    }
}
