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
  echo "      Install: sudo dnf install lm_sensors"
fi

if ! command -v busctl &>/dev/null; then
  echo "WARN: busctl not found. Time estimates will use instantaneous math."
fi

# ── install source ────────────────────────────────────────────────────
mkdir -p "$INSTALL_DIR"
cp -r "$SCRIPT_DIR/src" "$INSTALL_DIR/"
echo "    source → $INSTALL_DIR/src/"

# ── install systemd units ─────────────────────────────────────────────
mkdir -p "$SYSTEMD_DIR"

# resolve bun path for the service file
BUN_PATH="$(command -v bun)"

cat > "$SYSTEMD_DIR/batmon.service" <<EOF
[Unit]
Description=batmon – battery health logger (sysfs → SQLite)

[Service]
Type=oneshot
ExecStart=${BUN_PATH} run ${INSTALL_DIR}/src/index.ts
Nice=10
EOF

cp "$SCRIPT_DIR/systemd/batmon.timer" "$SYSTEMD_DIR/batmon.timer"
echo "    units  → $SYSTEMD_DIR/"

# ── enable ────────────────────────────────────────────────────────────
systemctl --user daemon-reload
systemctl --user enable --now batmon.timer
echo "    timer  → enabled (every 60 s)"

# ── verify ────────────────────────────────────────────────────────────
echo ""
echo "==> Running first sample…"
if "$BUN_PATH" run "$INSTALL_DIR/src/index.ts"; then
  echo "    ✓ first sample stored"
  echo ""
  echo "    Check data (if sqlite3 CLI is installed):"
  echo "      sqlite3 $INSTALL_DIR/battery.db 'SELECT * FROM samples ORDER BY id DESC LIMIT 1;'"
  echo ""
  echo "    Check timer:"
  echo "      systemctl --user status batmon.timer"
  echo ""
  echo "    View logs:"
  echo "      journalctl --user -u batmon -n 20"
else
  echo "    ✗ first run failed – check: journalctl --user -u batmon -n 10"
  exit 1
fi

echo ""
echo "==> Done. Battery health monitoring is active."