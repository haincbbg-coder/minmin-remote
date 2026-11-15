# Cài mm-relay lên VPS Ubuntu (Hostinger)

Relay là máy chủ cầu nối, đặt trên VPS có IP công khai để các máy con và máy bố
tìm thấy nhau qua Internet.

## Lấy file chạy

Bản Linux build sẵn ở GitHub Actions: repo → tab **Actions** → lần chạy mới nhất
→ Artifacts → **mm-remote-linux** (chứa `mm-relay`). Build trên Ubuntu 22.04 nên
chạy được cả Ubuntu 22.04 và 24.04.

## Cài (chạy trên VPS)

Tải `mm-relay` và `install-relay.sh` lên VPS (cùng một thư mục), rồi:

```bash
chmod +x install-relay.sh
sudo bash install-relay.sh 5000
```

Script sẽ: đặt `mm-relay` vào `/usr/local/bin`, tạo dịch vụ systemd chạy nền,
bật tự khởi động, và mở cổng **UDP 5000** (nếu dùng ufw).

> Quan trọng: QUIC chạy trên **UDP**. Trên bảng điều khiển Hostinger (firewall),
> nhớ mở **UDP cổng 5000**, không phải TCP.

## Kiểm tra

```bash
systemctl status mm-relay
journalctl -u mm-relay -f
```

## Dùng

- Máy con: `mm-server --relay <ip-vps>:5000 --account halimi --device-id vm01 --device-name "VM-01"` (đặt `MM_PASSWORD`).
- Máy bố: `mm-viewer --relay <ip-vps>:5000 --account halimi --connect vm01` (cùng `MM_PASSWORD`).
