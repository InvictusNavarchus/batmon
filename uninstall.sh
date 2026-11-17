#!/usr/bin/env bash
set -euo pipefail

DATA_DIR="$HOME/.local/share/batmon"
if [ -f "$DATA_DIR/uninstall.sh" ]; then
  exec bash "$DATA_DIR/uninstall.sh"
fi

# Compatibility for installations made before paths were recorded.
BIN_DIR="${BIN_DIR:-$HOME/.local/bin}"
SYSTEMD_DIR="$HOME/.config/systemd/user"

echo "==> batmon uninstaller"

for unit in batmon.timer batmon.service; do
  state=$(systemctl --user show --property=LoadState,ActiveState "$unit")
  # Missing files do not prove the manager has no running collector.
  if [[ "$state" == *"LoadState=not-found"* && "$state" == *"ActiveState=inactive"* ]]; then
    continue
  fi
  systemctl --user disable --now "$unit"
done
rm -f "$SYSTEMD_DIR/batmon.service" "$SYSTEMD_DIR/batmon.timer"
systemctl --user daemon-reload

echo "    service and timer stopped and removed."

rm -f "$BIN_DIR/batmon" "$BIN_DIR/batmon-plot"
echo "    binary removed from $BIN_DIR/batmon"

echo ""
echo "    Databases preserved at: $DATA_DIR/battery.db and $DATA_DIR/debug.db"
echo "    To remove all data:     rm -rf $DATA_DIR"
echo ""
echo "==> Done."
