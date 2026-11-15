//! mm-transport — khung truyền dữ liệu có mã hoá cho MinMin Remote.
//!
//! Mỗi gói tin đi trên dây theo dạng length-prefix: 4 byte độ dài (big-endian)
//! rồi tới nội dung. Giai đoạn handshake gửi các thông điệp Noise ở dạng thô;
//! sau khi handshake xong, mọi gói tin được mã hoá bằng [`mm_crypto::Session`].

use std::sync::{Arc, Mutex};

use serde::{de::DeserializeOwned, Serialize};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadHalf, WriteHalf};

/// Giới hạn kích thước một khung để tránh cấp phát bộ nhớ quá lớn (32 MiB).
pub const MAX_FRAME: usize = 32 * 1024 * 1024;

/// Noise giới hạn mỗi thông điệp 65535 byte (gồm 16 byte tag). Một gói tin lớn
/// (khung hình JPEG) được chia thành nhiều mẩu, mỗi mẩu kèm 1 byte cờ cho biết
/// còn mẩu nữa hay đã hết, rồi mã hoá và đóng khung riêng.
const MAX_CHUNK_PLAIN: usize = 65000;

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("lỗi IO: {0}")]
    Io(#[from] std::io::Error),
    #[error("lỗi mã hoá: {0}")]
    Crypto(#[from] mm_crypto::CryptoError),
    #[error("lỗi gói tin: {0}")]
    Proto(#[from] mm_proto::ProtoError),
    #[error("khung quá lớn: {0} byte")]
    FrameTooLarge(usize),
    #[error("kết nối đã đóng")]
    Closed,
}

pub type Result<T> = std::result::Result<T, TransportError>;

/// Ghi một khung thô (length-prefix) ra stream.
pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, data: &[u8]) -> Result<()> {
    if data.len() > MAX_FRAME {
        return Err(TransportError::FrameTooLarge(data.len()));
    }
    w.write_all(&(data.len() as u32).to_be_bytes()).await?;
    w.write_all(data).await?;
    w.flush().await?;
    Ok(())
}

/// Đọc một khung thô (length-prefix) từ stream.
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    match r.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
            return Err(TransportError::Closed)
        }
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_FRAME {
        return Err(TransportError::FrameTooLarge(len));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    Ok(buf)
}

/// Thực hiện handshake Noise phía initiator (Viewer) trên stream đã mở.
pub async fn handshake_initiator<S>(stream: &mut S, psk: &[u8; 32]) -> Result<mm_crypto::Session>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut hs = mm_crypto::initiator(psk)?;
    // -> e
    let m1 = hs.write_message(&[])?;
    write_frame(stream, &m1).await?;
    // <- e, ee, s, es
    let m2 = read_frame(stream).await?;
    hs.read_message(&m2)?;
    // -> s, se, psk
    let m3 = hs.write_message(&[])?;
    write_frame(stream, &m3).await?;
    Ok(hs.into_transport()?)
}

/// Thực hiện handshake Noise phía responder (Server) trên stream đã nhận.
pub async fn handshake_responder<S>(stream: &mut S, psk: &[u8; 32]) -> Result<mm_crypto::Session>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut hs = mm_crypto::responder(psk)?;
    let m1 = read_frame(stream).await?;
    hs.read_message(&m1)?;
    let m2 = hs.write_message(&[])?;
    write_frame(stream, &m2).await?;
    let m3 = read_frame(stream).await?;
    hs.read_message(&m3)?;
    Ok(hs.into_transport()?)
}

/// Tách một stream đã handshake thành nửa đọc và nửa ghi, cùng chia sẻ một
/// [`mm_crypto::Session`]. Hai chiều Noise độc lập nhau nên khoá chỉ bị giữ
/// trong lúc mã hoá/giải mã (không giữ qua await), gần như không tranh chấp.
pub fn secure_split<S>(stream: S, session: mm_crypto::Session) -> (SecureReader<S>, SecureWriter<S>)
where
    S: AsyncRead + AsyncWrite,
{
    let (r, w) = tokio::io::split(stream);
    let session = Arc::new(Mutex::new(session));
    (
        SecureReader { inner: r, session: session.clone() },
        SecureWriter { inner: w, session },
    )
}

/// Nửa đọc đã mã hoá.
pub struct SecureReader<S> {
    inner: ReadHalf<S>,
    session: Arc<Mutex<mm_crypto::Session>>,
}

impl<S: AsyncRead + AsyncWrite> SecureReader<S> {
    /// Nhận và giải mã một gói tin kiểu `T`, ghép lại từ các mẩu nếu bị chia nhỏ.
    pub async fn recv<T: DeserializeOwned>(&mut self) -> Result<T> {
        let mut message = Vec::new();
        loop {
            let ct = read_frame(&mut self.inner).await?;
            let chunk = {
                let mut s = self.session.lock().unwrap();
                s.decrypt(&ct)?
            };
            if chunk.is_empty() {
                return Err(TransportError::Closed);
            }
            let last = chunk[0] == 1;
            message.extend_from_slice(&chunk[1..]);
            if last {
                break;
            }
            if message.len() > MAX_FRAME {
                return Err(TransportError::FrameTooLarge(message.len()));
            }
        }
        Ok(mm_proto::decode(&message)?)
    }
}

/// Nửa ghi đã mã hoá.
pub struct SecureWriter<S> {
    inner: WriteHalf<S>,
    session: Arc<Mutex<mm_crypto::Session>>,
}

impl<S: AsyncRead + AsyncWrite> SecureWriter<S> {
    /// Mã hoá và gửi một gói tin kiểu `T`, chia nhỏ nếu vượt giới hạn Noise.
    pub async fn send<T: Serialize>(&mut self, msg: &T) -> Result<()> {
        let pt = mm_proto::encode(msg)?;
        // Chia thành các mẩu <= MAX_CHUNK_PLAIN; mỗi mẩu có 1 byte cờ ở đầu.
        let mut chunks = pt.chunks(MAX_CHUNK_PLAIN).peekable();
        // Gói tin rỗng vẫn phải gửi đúng 1 mẩu.
        if chunks.peek().is_none() {
            let ct = {
                let mut s = self.session.lock().unwrap();
                s.encrypt(&[1u8])?
            };
            return write_frame(&mut self.inner, &ct).await;
        }
        while let Some(chunk) = chunks.next() {
            let last = chunks.peek().is_none();
            let mut record = Vec::with_capacity(chunk.len() + 1);
            record.push(if last { 1 } else { 0 });
            record.extend_from_slice(chunk);
            let ct = {
                let mut s = self.session.lock().unwrap();
                s.encrypt(&record)?
            };
            write_frame(&mut self.inner, &ct).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mm_proto::{ClientMessage, ServerMessage, PROTOCOL_VERSION};

    #[tokio::test]
    async fn end_to_end_handshake_and_message() {
        let (client, server) = tokio::io::duplex(64 * 1024);
        let psk = mm_crypto::derive_psk("bo-test").unwrap();
        let psk2 = psk;

        let server_task = tokio::spawn(async move {
            let mut s = server;
            let session = handshake_responder(&mut s, &psk2).await.unwrap();
            let (mut r, mut w) = secure_split(s, session);
            let msg: ClientMessage = r.recv().await.unwrap();
            assert!(matches!(msg, ClientMessage::Hello { .. }));
            w.send(&ServerMessage::Welcome {
                protocol_version: PROTOCOL_VERSION,
                host_name: "may-ao-1".into(),
                os: "Windows".into(),
                monitors: vec![],
            })
            .await
            .unwrap();
        });

        let mut c = client;
        let session = handshake_initiator(&mut c, &psk).await.unwrap();
        let (mut r, mut w) = secure_split(c, session);
        w.send(&ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            viewer_name: "bố".into(),
        })
        .await
        .unwrap();
        let reply: ServerMessage = r.recv().await.unwrap();
        assert!(matches!(reply, ServerMessage::Welcome { .. }));
        server_task.await.unwrap();
    }
}
