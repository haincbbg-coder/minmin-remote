# MinMin Remote

App điều khiển máy tính từ xa, học theo Radmin. Dùng **nội bộ** để bố điều
khiển các máy ảo và server của mình. Viết bằng Rust + Tauri.

> Thiết kế đầy đủ nằm trong tài liệu "MinMin Remote — Thiết kế P0".
> Đây là mã nguồn **v2**: xem màn hình mượt với codec vùng-thay-đổi và transport
> QUIC, mã hoá, đăng nhập bằng mật khẩu.

## Nâng cấp v2 — điều khiển mượt hơn

Hai thay đổi lõi so với bản P1 đầu, đều đã chạy và đo trên máy chủ Linux:

1. **Codec dirty-rect (`mm-codec`)** — màn hình chia ô 64px, chỉ gửi ô *thay
   đổi* so với khung trước. Màn hình đứng yên ⇒ gần như không tốn băng thông.
2. **Transport QUIC (`mm-quic`)** — chạy trên UDP, TLS 1.3 sẵn, mỗi khung đi
   một luồng riêng nên không bị "head-of-line blocking" (một khung chậm không
   chặn khung sau). Hợp cho kết nối Internet ở P5. Xác thực mật khẩu bằng
   challenge-response (Argon2).

**Số đo thật** (desktop 1080p@30, cùng một cảnh):

| Cách | fps | Băng thông/khung |
|------|-----|------------------|
| P1: JPEG cả khung + TCP | 25,6 | 58,2 KB |
| **v2: tiled + QUIC** | **29,4** | **7,3 KB** |

Giảm ~8 lần băng thông. Trên mạng yếu, đây là khác biệt giữa giật và mượt.

## Trạng thái các giai đoạn

| GĐ | Nội dung | Trạng thái |
|----|----------|-----------|
| P0 | Thiết kế + giao thức | ✅ |
| P1 | Xem màn hình + mã hoá + mật khẩu | ✅ |
| **v2** | Codec dirty-rect + transport QUIC | ✅ lõi xong, đã kiểm thử |
| P2 | Điều khiển chuột/bàn phím, nhiều màn hình, **H.264 phần cứng** | ⏳ |
| P3 | Truyền file, dòng lệnh, nguồn | ⏳ |
| P4 | Danh bạ, Tường màn hình, thao tác hàng loạt, nhật ký | ⏳ |
| P5 | Kết nối qua Internet (NAT traversal), cài im lặng | ⏳ |

## Cấu trúc

```
crates/
  mm-proto      Định nghĩa gói tin                               ✅ test
  mm-crypto     Mã hoá Noise + Argon2 (dùng cho transport TCP)   ✅ test
  mm-transport  Đóng khung + chia nhỏ gói (đường TCP)            ✅ test
  mm-quic       Transport QUIC: control + video uni-stream       ✅ test
  mm-codec      Codec dirty-rect (chỉ gửi ô thay đổi)            ✅ test
  mm-capture    Chụp màn hình: Mock (mọi HĐH) + DXGI (Windows)   ✅ test
  mm-server     Máy bị điều khiển — chạy nền, không giao diện
  mm-viewer     Người điều khiển — bản CLI (Tauri GUI ở P2)
```

Phần **chỉ Windows** còn lại: chụp màn hình thật bằng Desktop Duplication API
(`mm-capture/src/dxgi.rs`) và mã hoá H.264 phần cứng — hoàn thiện khi build
trên máy Windows.

## Build & test

```bash
cargo build --release
cargo test            # 9 test: proto, crypto, transport, quic, codec, capture
```

## Chạy thử v2 (nguồn hình giả, không cần màn hình thật)

Máy bị điều khiển:
```bash
set MM_PASSWORD=matkhau-cua-bo
mm-server --port 4899 --transport quic --codec tiled --fps 30 --mock-size 1920x1080
```

Người điều khiển:
```bash
set MM_PASSWORD=matkhau-cua-bo
mm-viewer --host <ip-server> --port 4899 --transport quic --frames 150 --save 1
```

Trên Windows với màn hình thật, thêm `--real` cho server. Có thể quay lại
đường cũ bằng `--transport tcp --codec jpeg` để so sánh.

## Bảo mật

- QUIC/TLS 1.3 mã hoá toàn bộ; đăng nhập mật khẩu (Argon2 + challenge-response),
  sai mật khẩu bị từ chối ngay.
- Server chạy hoàn toàn ẩn: không icon, không thông báo — hợp với điều khiển
  máy ảo/server không người ngồi. Không ký số nên thêm ngoại lệ Windows
  Defender trên các máy của bố.
- Cert server hiện tự ký, client bỏ qua kiểm tra cert (đã auth bằng mật khẩu) —
  đủ cho mạng nội bộ; P5 sẽ thêm ghim vân tay cert để chống giả mạo khi ra
  Internet.

## Cần bố cung cấp để đi tiếp

1. Repo GitHub để đẩy mã nguồn (CI tự build `.exe`).
2. Máy ảo/server chạy **Windows hay Linux**, nằm ở đâu (máy nhà / VPS).
3. 2 máy Windows (hoặc 1 máy + 1 máy ảo) để kiểm thử P2 với màn hình thật.
