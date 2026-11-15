//! mm-quic — transport hiện đại dựa trên QUIC (thư viện `quinn`).
//!
//! Vì sao QUIC thay cho TCP:
//! - Chạy trên UDP, có TLS 1.3 sẵn, bắt tay nhanh.
//! - KHÔNG bị "head-of-line blocking": mỗi khung hình đi trên MỘT luồng (stream)
//!   riêng, nên một khung bị chậm không chặn khung kế — hình mượt hơn khi mạng
//!   chập chờn, mất gói.
//! - Hợp cho P5 kết nối qua Internet (NAT traversal tốt hơn).
//!
//! Kênh điều khiển (control) là một bi-stream: xác thực mật khẩu bằng
//! challenge-response (khoá dẫn xuất Argon2), rồi trao đổi gói tin điều khiển.
//! Video đi trên các uni-stream, mỗi khung một luồng.
//!
//! Mã hoá do chính QUIC/TLS 1.3 lo. Cert server là tự ký; client bỏ qua kiểm
//! tra cert (đã xác thực bằng mật khẩu) — hợp cho mạng nội bộ của bố; P5 sẽ
//! thêm ghim vân tay cert.

use std::sync::{Arc, Once};

use anyhow::{anyhow, Result};
use serde::{de::DeserializeOwned, Serialize};
use mm_transport::{read_frame, write_frame};

const ALPN: &[u8] = b"mmr/1";
static PROVIDER: Once = Once::new();

fn ensure_provider() {
    PROVIDER.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// Dẫn xuất đáp án cho challenge từ mật khẩu: Blake2s(key || challenge).
fn auth_response(password: &str, challenge: &[u8]) -> Result<[u8; 32]> {
    use blake2::{Blake2s256, Digest};
    let key = mm_crypto::derive_psk(password).map_err(|e| anyhow!("{e}"))?;
    let mut h = Blake2s256::new();
    h.update(key);
    h.update(challenge);
    Ok(h.finalize().into())
}

// ----------------------- Server -----------------------

fn server_crypto() -> Result<quinn::ServerConfig> {
    ensure_provider();
    let cert = rcgen::generate_simple_self_signed(vec!["mm".to_string()])?;
    let cert_der = rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::try_from(cert.key_pair.serialize_der())
        .map_err(|e| anyhow!("khoá: {e}"))?;
    let mut tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)?;
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let qsc = quinn::crypto::rustls::QuicServerConfig::try_from(tls)?;
    Ok(quinn::ServerConfig::with_crypto(Arc::new(qsc)))
}

fn client_cfg() -> Result<quinn::ClientConfig> {
    ensure_provider();
    let verifier = Arc::new(SkipServerVerification::new());
    let mut tls = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let qcc = quinn::crypto::rustls::QuicClientConfig::try_from(tls)?;
    Ok(quinn::ClientConfig::new(Arc::new(qcc)))
}

/// Mở endpoint QUIC phía Server với cert tự ký.
pub fn server_endpoint(addr: std::net::SocketAddr) -> Result<quinn::Endpoint> {
    Ok(quinn::Endpoint::server(server_crypto()?, addr)?)
}

/// Mở endpoint QUIC phía Client (Viewer).
pub fn client_endpoint() -> Result<quinn::Endpoint> {
    ensure_provider();
    let mut endpoint = quinn::Endpoint::client("0.0.0.0:0".parse()?)?;
    endpoint.set_default_client_config(client_cfg()?);
    Ok(endpoint)
}

/// Endpoint vừa gọi (client) vừa nhận (server) trên CÙNG một socket — dùng cho
/// đục NAT: kết nối tới relay và kết nối thẳng P2P đi chung một socket để giữ
/// nguyên ánh xạ NAT.
pub fn dual_endpoint(bind: std::net::SocketAddr) -> Result<quinn::Endpoint> {
    let mut endpoint = quinn::Endpoint::server(server_crypto()?, bind)?;
    endpoint.set_default_client_config(client_cfg()?);
    Ok(endpoint)
}

/// Phiên QUIC phía Server sau khi xác thực xong.
pub struct ServerSession {
    pub conn: quinn::Connection,
    pub control: Channel,
}

/// Xác thực mật khẩu trên một Connection đã bắt tay QUIC (phía nhận).
pub async fn server_auth(conn: quinn::Connection, password: &str) -> Result<ServerSession> {
    let (send, recv) = conn.accept_bi().await?;
    let mut control = Channel { send, recv };
    let _opener = control.read_raw().await?;
    let mut challenge = [0u8; 32];
    use rand::RngCore;
    rand::thread_rng().fill_bytes(&mut challenge);
    control.write_raw(&challenge).await?;
    let resp = control.read_raw().await?;
    let expected = auth_response(password, &challenge)?;
    if resp != expected {
        control.write_raw(b"NO").await.ok();
        conn.close(1u32.into(), b"auth failed");
        return Err(anyhow!("sai mật khẩu"));
    }
    control.write_raw(b"OK").await?;
    Ok(ServerSession { conn, control })
}

/// Chờ một kết nối, bắt tay, xác thực mật khẩu.
pub async fn accept(endpoint: &quinn::Endpoint, password: &str) -> Result<ServerSession> {
    let incoming = endpoint.accept().await.ok_or_else(|| anyhow!("endpoint đóng"))?;
    let conn = incoming.await?;
    server_auth(conn, password).await
}

// ----------------------- Client -----------------------

/// Phiên QUIC phía Client sau khi xác thực xong.
pub struct ClientSession {
    pub conn: quinn::Connection,
    pub control: Channel,
}

/// Xác thực mật khẩu trên một Connection đã bắt tay QUIC (phía gọi).
pub async fn client_auth(conn: quinn::Connection, password: &str) -> Result<ClientSession> {
    let (send, recv) = conn.open_bi().await?;
    let mut control = Channel { send, recv };
    control.write_raw(b"MMR1").await?;
    let challenge = control.read_raw().await?;
    let resp = auth_response(password, &challenge)?;
    control.write_raw(&resp).await?;
    let verdict = control.read_raw().await?;
    if verdict != b"OK" {
        return Err(anyhow!("server từ chối: sai mật khẩu"));
    }
    Ok(ClientSession { conn, control })
}

/// Nối tới Server, bắt tay, chứng minh mật khẩu.
pub async fn connect(
    endpoint: &quinn::Endpoint,
    addr: std::net::SocketAddr,
    password: &str,
) -> Result<ClientSession> {
    let conn = endpoint.connect(addr, "mm")?.await?;
    client_auth(conn, password).await
}

// ----------------------- Đục NAT (hole punching) -----------------------

/// Phía gọi (viewer): liên tục thử nối thẳng tới địa chỉ công khai của peer đến
/// khi được hoặc quá hạn. Trả về Connection đã bắt tay QUIC (chưa auth app).
pub async fn punch_connect(
    endpoint: &quinn::Endpoint,
    peer: std::net::SocketAddr,
    total: std::time::Duration,
) -> Result<quinn::Connection> {
    let deadline = tokio::time::Instant::now() + total;
    loop {
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("đục NAT (connect) quá hạn");
        }
        if let Ok(connecting) = endpoint.connect(peer, "mm") {
            if let Ok(Ok(conn)) =
                tokio::time::timeout(std::time::Duration::from_millis(900), connecting).await
            {
                return Ok(conn);
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

/// Phía nhận (device): vừa bắn gói "đục lỗ" tới peer để mở NAT, vừa chờ kết nối
/// thẳng tới. Trả về Connection đã bắt tay QUIC (chưa auth app).
pub async fn punch_accept(
    endpoint: &quinn::Endpoint,
    peer: std::net::SocketAddr,
    total: std::time::Duration,
) -> Result<quinn::Connection> {
    let ep = endpoint.clone();
    let punch = tokio::spawn(async move {
        let end = tokio::time::Instant::now() + total;
        while tokio::time::Instant::now() < end {
            if let Ok(connecting) = ep.connect(peer, "mm") {
                let _ = tokio::time::timeout(std::time::Duration::from_millis(300), connecting).await;
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    });
    let res = tokio::time::timeout(total, async {
        loop {
            let incoming = endpoint.accept().await.ok_or_else(|| anyhow!("endpoint đóng"))?;
            let remote = incoming.remote_address();
            match incoming.await {
                Ok(conn) => {
                    if remote.ip() == peer.ip() {
                        return Ok::<quinn::Connection, anyhow::Error>(conn);
                    }
                    conn.close(0u32.into(), b"not peer");
                }
                Err(_) => {}
            }
        }
    })
    .await;
    punch.abort();
    match res {
        Ok(Ok(conn)) => Ok(conn),
        Ok(Err(e)) => Err(e),
        Err(_) => anyhow::bail!("đục NAT (accept) quá hạn"),
    }
}

// ----------------------- Kênh control -----------------------

/// Một bi-stream QUIC dùng trao đổi gói tin điều khiển (có length-prefix).
pub struct Channel {
    pub send: quinn::SendStream,
    pub recv: quinn::RecvStream,
}

impl Channel {
    async fn write_raw(&mut self, data: &[u8]) -> Result<()> {
        write_frame(&mut self.send, data).await?;
        Ok(())
    }
    async fn read_raw(&mut self) -> Result<Vec<u8>> {
        Ok(read_frame(&mut self.recv).await?)
    }
    /// Gửi một gói tin điều khiển.
    pub async fn send<T: Serialize>(&mut self, msg: &T) -> Result<()> {
        let bytes = mm_proto::encode(msg)?;
        self.write_raw(&bytes).await
    }
    /// Nhận một gói tin điều khiển.
    pub async fn recv<T: DeserializeOwned>(&mut self) -> Result<T> {
        let bytes = self.read_raw().await?;
        Ok(mm_proto::decode(&bytes)?)
    }
}

/// Gửi một khung hình trên một uni-stream riêng: header rồi payload.
pub async fn send_frame(conn: &quinn::Connection, header: &[u8], payload: &[u8]) -> Result<()> {
    let mut uni = conn.open_uni().await?;
    write_frame(&mut uni, header).await?;
    write_frame(&mut uni, payload).await?;
    uni.finish()?;
    Ok(())
}

/// Nhận một khung hình từ uni-stream kế tiếp: trả về (header, payload).
pub async fn recv_frame(conn: &quinn::Connection) -> Result<(Vec<u8>, Vec<u8>)> {
    let mut uni = conn.accept_uni().await?;
    let header = read_frame(&mut uni).await?;
    let payload = read_frame(&mut uni).await?;
    Ok((header, payload))
}

// ----------------------- Bỏ kiểm tra cert (client) -----------------------

#[derive(Debug)]
struct SkipServerVerification(Arc<rustls::crypto::CryptoProvider>);

impl SkipServerVerification {
    fn new() -> Self {
        Self(Arc::new(rustls::crypto::ring::default_provider()))
    }
}

impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mm_proto::{ClientMessage, PROTOCOL_VERSION};

    async fn run(pw_server: &str, pw_client: &str) -> Result<()> {
        let addr: std::net::SocketAddr = "127.0.0.1:0".parse().unwrap();
        let server_ep = server_endpoint(addr)?;
        let server_addr = server_ep.local_addr()?;
        let pw_s = pw_server.to_string();

        let server = tokio::spawn(async move {
            let mut sess = accept(&server_ep, &pw_s).await?;
            // Nhận 1 gói control, rồi gửi 1 khung qua uni.
            let _msg: ClientMessage = sess.control.recv().await?;
            send_frame(&sess.conn, b"HDR", b"PIXELS").await?;
            // giữ kết nối một nhịp để client nhận xong
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            anyhow::Ok(())
        });

        let client_ep = client_endpoint()?;
        let mut sess = connect(&client_ep, server_addr, pw_client).await?;
        sess.control
            .send(&ClientMessage::Hello {
                protocol_version: PROTOCOL_VERSION,
                viewer_name: "bố".into(),
            })
            .await?;
        let (h, p) = recv_frame(&sess.conn).await?;
        assert_eq!(h, b"HDR");
        assert_eq!(p, b"PIXELS");
        server.await??;
        Ok(())
    }

    #[tokio::test]
    async fn quic_good_password() {
        run("bo-quic", "bo-quic").await.expect("mật khẩu đúng phải thông");
    }

    #[tokio::test]
    async fn quic_bad_password() {
        let r = run("dung", "sai").await;
        assert!(r.is_err(), "mật khẩu sai mà vẫn thông");
    }
}
