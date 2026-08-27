#!/usr/bin/env bash
set -euo pipefail

INSTALL_DIR="$HOME/.local/share/batmon"
SYSTEMD_DIR="$HOME/.config/systemd/user"

echo "==> batmon uninstaller"

systemctl --user disable --now batmon.service 2>/dev/null || true
systemctl --user disable --now batmon.timer 2>/dev/null || true
rm -f "$SYSTEMD_DIR/batmon.service" "$SYSTEMD_DIR/batmon.timer"
systemctl --user daemon-reload

echo "    service and timer stopped and removed."
echo ""
echo "    Databases preserved at: $INSTALL_DIR/battery.db and $INSTALL_DIR/debug.db"
echo "    To remove all data:     rm -rf $INSTALL_DIR"
echo ""
echo "==> Done."