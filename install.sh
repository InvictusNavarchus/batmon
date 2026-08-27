#!/usr/bin/env bash
set -euo pipefail

INSTALL_DIR="$HOME/.local/share/batmon"
SYSTEMD_DIR="$HOME/.config/systemd/user"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

echo "==> batmon installer"

# ── prerequisites ─────────────────────────────────────────────────────
if ! command -v bun &>/dev/null; then
  echo "ERROR: bun not found. Install: https://bun.sh" >&2
  exit 1
fi

if ! command -v sensors &>/dev/null; then
  echo "WARN: lm_sensors not found. System temps will be NULL."
  echo "      Install: sudo dnf install lm_sensors (or sudo apt install lm-sensors)"
fi

if ! command -v busctl &>/dev/null; then
  echo "WARN: busctl not found. Time estimates will use instantaneous math."
fi

if ! command -v notify-send &>/dev/null; then
  echo "WARN: notify-send (libnotify) not found. Desktop notifications will be disabled."
  echo "      Install: sudo dnf install libnotify (or apt install libnotify-bin)"
fi

# ── install source ────────────────────────────────────────────────────
mkdir -p "$INSTALL_DIR"
rm -rf "$INSTALL_DIR/src"
cp -r "$SCRIPT_DIR/src" "$INSTALL_DIR/"
echo "    source → $INSTALL_DIR/src/"

# ── install systemd units ─────────────────────────────────────────────
mkdir -p "$SYSTEMD_DIR"

# Clean up legacy timer if present
systemctl --user disable --now batmon.timer 2>/dev/null || true
rm -f "$SYSTEMD_DIR/batmon.timer"

# resolve bun path for the service file
BUN_PATH="$(command -v bun)"

cat > "$SYSTEMD_DIR/batmon.service" <<EOF
[Unit]
Description=batmon – battery health monitor & flight recorder (sysfs → SQLite)
Documentation=https://github.com/InvictusNavarchus/batmon

[Service]
Type=simple
ExecStart=${BUN_PATH} run ${INSTALL_DIR}/src/index.ts
Restart=on-failure
RestartSec=5s
Nice=10

[Install]
WantedBy=default.target
EOF

echo "    service → $SYSTEMD_DIR/batmon.service"

# ── verify ────────────────────────────────────────────────────────────
echo ""
echo "==> Running sample verification…"
if "$BUN_PATH" run "$INSTALL_DIR/src/index.ts" --oneshot; then
  echo "    ✓ sample stored"
else
  echo "    ✗ test run failed – check your system configuration"
  exit 1
fi

# ── enable & restart service ──────────────────────────────────────────
systemctl --user daemon-reload
systemctl --user enable --now batmon.service
systemctl --user restart batmon.service
echo "    service → enabled & started (1s flight recorder + 60s history)"
echo ""
echo "    Check historical data (if sqlite3 CLI is installed):"
echo "      sqlite3 $INSTALL_DIR/battery.db 'SELECT * FROM samples ORDER BY id DESC LIMIT 1;'"
echo ""
echo "    Check flight recorder data:"
echo "      sqlite3 $INSTALL_DIR/debug.db 'SELECT * FROM samples_debug ORDER BY id DESC LIMIT 5;'"
echo ""
echo "    Check service status:"
echo "      systemctl --user status batmon.service"
echo ""
echo "    View logs:"
echo "      journalctl --user -u batmon -f"
echo ""
echo "==> Done. Battery health monitoring and flight recording are active."