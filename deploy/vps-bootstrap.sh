#!/usr/bin/env bash
# Build và cài mm-relay trên VPS Ubuntu từ nguồn, rồi chạy nền bằng systemd.
# Chạy trong thư mục repo đã clone, với quyền root:  bash deploy/vps-bootstrap.sh [PORT]
set -euo pipefail

PORT="${1:-5000}"
export DEBIAN_FRONTEND=noninteractive

echo "==> Cài gói cần thiết"
apt-get update -y
apt-get install -y curl build-essential pkg-config git

if ! command -v cargo >/dev/null 2>&1; then
  echo "==> Cài Rust"
  curl https://sh.rustup.rs -sSf | sh -s -- -y
fi
. "$HOME/.cargo/env"

echo "==> Build mm-relay (vài phút)"
cargo build --release -p mm-relay
install -m0755 target/release/mm-relay /usr/local/bin/mm-relay

echo "==> Tạo dịch vụ systemd"
cat >/etc/systemd/system/mm-relay.service <<UNIT
[Unit]
Description=MinMin Remote relay
After=network.target

[Service]
ExecStart=/usr/local/bin/mm-relay --host 0.0.0.0 --port ${PORT}
Restart=always
RestartSec=3
Environment=RUST_LOG=info

[Install]
WantedBy=multi-user.target
UNIT

systemctl daemon-reload
systemctl enable --now mm-relay

# Mở cổng UDP nếu ufw đang bật (QUIC chạy trên UDP).
if command -v ufw >/dev/null 2>&1 && ufw status 2>/dev/null | grep -q "Status: active"; then
  ufw allow "${PORT}/udp" || true
fi

sleep 1
echo "==> Xong. Trạng thái:"
systemctl --no-pager status mm-relay | head -5 || true
echo ""
echo "mm-relay đang chạy trên UDP ${PORT}. Xem log: journalctl -u mm-relay -f"
