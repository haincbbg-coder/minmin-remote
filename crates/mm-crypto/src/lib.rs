//! mm-crypto — mã hoá đầu cuối cho MinMin Remote.
//!
//! Dùng Noise Protocol Framework (pattern `XXpsk3`) qua thư viện `snow`:
//! - Trao đổi khoá ephemeral X25519 cho mỗi kết nối (forward secrecy).
//! - Mã hoá ChaCha20-Poly1305.
//! - Một PSK (pre-shared key) trộn vào handshake, dẫn xuất từ MẬT KHẨU của
//!   Server bằng Argon2id. Nhờ vậy chỉ ai biết mật khẩu mới hoàn tất được
//!   handshake — đây là lớp chặn dò mật khẩu cơ bản của P1.
//!
//! Crate này KHÔNG tự đọc/ghi socket; mm-transport lo phần IO và gọi vào đây.

use thiserror::Error;

/// Bộ tham số Noise. BLAKE2s + ChaCha20-Poly1305 + X25519.
pub const NOISE_PARAMS: &str = "Noise_XXpsk3_25519_ChaChaPoly_BLAKE2s";

/// Salt cố định của ứng dụng dùng khi dẫn xuất PSK từ mật khẩu.
/// Mật khẩu mới là bí mật thật; Argon2 chỉ kéo dãn nó thành khoá 32 byte,
/// nên một salt cố định ở đây là chấp nhận được cho P1 (hai bên ra cùng PSK
/// mà không phải trao đổi salt trước). Có thể nâng thành salt theo từng máy ở P2.
const APP_SALT: &[u8; 16] = b"MinMinRemote-v1!";

#[derive(Debug, Error)]
pub enum CryptoError {
    #[error("lỗi Noise: {0}")]
    Noise(#[from] snow::Error),
    #[error("lỗi Argon2: {0}")]
    Argon2(String),
}

pub type Result<T> = std::result::Result<T, CryptoError>;

/// Dẫn xuất PSK 32 byte từ mật khẩu bằng Argon2id.
pub fn derive_psk(password: &str) -> Result<[u8; 32]> {
    let argon2 = argon2::Argon2::default();
    let mut out = [0u8; 32];
    argon2
        .hash_password_into(password.as_bytes(), APP_SALT, &mut out)
        .map_err(|e| CryptoError::Argon2(e.to_string()))?;
    Ok(out)
}

/// Tạo HandshakeState phía người điều khiển (Viewer = initiator).
pub fn initiator(psk: &[u8; 32]) -> Result<Handshake> {
    build(psk, true)
}

/// Tạo HandshakeState phía máy bị điều khiển (Server = responder).
pub fn responder(psk: &[u8; 32]) -> Result<Handshake> {
    build(psk, false)
}

fn build(psk: &[u8; 32], is_initiator: bool) -> Result<Handshake> {
    let params = NOISE_PARAMS.parse().expect("tham số Noise hợp lệ");
    let builder = snow::Builder::new(params);
    let static_key = builder.generate_keypair()?.private;
    let builder = builder.local_private_key(&static_key).psk(3, psk);
    let state = if is_initiator {
        builder.build_initiator()?
    } else {
        builder.build_responder()?
    };
    Ok(Handshake { state })
}

/// Bọc một bước handshake Noise. mm-transport lần lượt gọi
/// `write_message`/`read_message` theo đúng thứ tự của pattern XX (3 thông điệp).
pub struct Handshake {
    state: snow::HandshakeState,
}

impl Handshake {
    /// Ghi thông điệp handshake kế tiếp vào buffer trả về.
    pub fn write_message(&mut self, payload: &[u8]) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; payload.len() + 1024];
        let n = self.state.write_message(payload, &mut buf)?;
        buf.truncate(n);
        Ok(buf)
    }

    /// Đọc thông điệp handshake nhận được, trả về payload (thường rỗng).
    pub fn read_message(&mut self, message: &[u8]) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; message.len() + 1024];
        let n = self.state.read_message(message, &mut buf)?;
        buf.truncate(n);
        Ok(buf)
    }

    /// Handshake đã xong chưa.
    pub fn is_finished(&self) -> bool {
        self.state.is_handshake_finished()
    }

    /// Chuyển sang trạng thái truyền dữ liệu đã mã hoá.
    pub fn into_transport(self) -> Result<Session> {
        Ok(Session {
            state: self.state.into_transport_mode()?,
        })
    }
}

/// Kênh đã mã hoá sau khi handshake xong. Dùng để mã hoá/giải mã từng gói tin.
pub struct Session {
    state: snow::TransportState,
}

impl Session {
    /// Mã hoá một gói tin.
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; plaintext.len() + 16];
        let n = self.state.write_message(plaintext, &mut buf)?;
        buf.truncate(n);
        Ok(buf)
    }

    /// Giải mã một gói tin.
    pub fn decrypt(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; ciphertext.len()];
        let n = self.state.read_message(ciphertext, &mut buf)?;
        buf.truncate(n);
        Ok(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Chạy trọn handshake XX (3 thông điệp) giữa 2 bên trong bộ nhớ,
    /// rồi mã hoá/giải mã một gói tin. Kiểm tra mật khẩu đúng thì thông nhau.
    #[test]
    fn handshake_same_password() {
        let psk = derive_psk("matkhau-cua-bo").unwrap();
        let mut ini = initiator(&psk).unwrap();
        let mut res = responder(&psk).unwrap();

        // -> e
        let m1 = ini.write_message(&[]).unwrap();
        res.read_message(&m1).unwrap();
        // <- e, ee, s, es
        let m2 = res.write_message(&[]).unwrap();
        ini.read_message(&m2).unwrap();
        // -> s, se, psk
        let m3 = ini.write_message(&[]).unwrap();
        res.read_message(&m3).unwrap();

        assert!(ini.is_finished());
        assert!(res.is_finished());

        let mut ini_t = ini.into_transport().unwrap();
        let mut res_t = res.into_transport().unwrap();

        let ct = ini_t.encrypt(b"xin chao server").unwrap();
        let pt = res_t.decrypt(&ct).unwrap();
        assert_eq!(&pt, b"xin chao server");
    }

    /// Mật khẩu khác nhau thì handshake phải thất bại (chặn kẻ lạ).
    #[test]
    fn handshake_wrong_password_fails() {
        let psk_good = derive_psk("matkhau-dung").unwrap();
        let psk_bad = derive_psk("matkhau-sai").unwrap();
        let mut ini = initiator(&psk_bad).unwrap();
        let mut res = responder(&psk_good).unwrap();

        let m1 = ini.write_message(&[]).unwrap();
        // XX trộn PSK ở thông điệp 3, nhưng ee/es đã khác khoá -> sẽ hỏng
        // chậm nhất khi responder đọc thông điệp 3. Ta chạy tới đó.
        let _ = res.read_message(&m1);
        let m2 = match res.write_message(&[]) {
            Ok(m) => m,
            Err(_) => return, // đã hỏng sớm, đạt yêu cầu
        };
        if ini.read_message(&m2).is_err() {
            return;
        }
        let m3 = match ini.write_message(&[]) {
            Ok(m) => m,
            Err(_) => return,
        };
        // Thông điệp 3 có PSK sai -> responder phải từ chối.
        assert!(res.read_message(&m3).is_err(), "mật khẩu sai mà vẫn thông");
    }
}
