#!/usr/bin/env bash
# Cài mm-relay làm dịch vụ systemd trên Ubuntu VPS (Hostinger…).
# Dùng: sudo bash install-relay.sh [PORT]
# Mặc định cổng 5000. LƯU Ý: QUIC chạy trên UDP nên firewall phải mở UDP.
set -euo pipefail

PORT="${1:-5000}"
BIN_SRC="$(dirname "$0")/mm-relay"
BIN_DST="/usr/local/bin/mm-relay"

if [[ ! -f "$BIN_SRC" ]]; then
  echo "Không thấy file mm-relay cạnh script này. Hãy đặt mm-relay cùng thư mục." >&2
  exit 1
fi

install -m 0755 "$BIN_SRC" "$BIN_DST"

cat >/etc/systemd/system/mm-relay.service <<UNIT
[Unit]
Description=MinMin Remote relay
After=network.target

[Service]
ExecStart=${BIN_DST} --host 0.0.0.0 --port ${PORT}
Restart=always
RestartSec=3
Environment=RUST_LOG=info

[Install]
WantedBy=multi-user.target
UNIT

systemctl daemon-reload
systemctl enable --now mm-relay

# Mở cổng UDP nếu có ufw.
if command -v ufw >/dev/null 2>&1; then
  ufw allow "${PORT}/udp" || true
fi

echo "Đã cài và chạy mm-relay trên cổng UDP ${PORT}."
echo "Xem log:   journalctl -u mm-relay -f"
echo "Trạng thái: systemctl status mm-relay"
