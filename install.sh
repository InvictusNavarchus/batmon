#!/usr/bin/env bash
set -euo pipefail

BIN_DIR="$HOME/.local/bin"
DATA_DIR="$HOME/.local/share/batmon"
SYSTEMD_DIR="$HOME/.config/systemd/user"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

echo "==> batmon installer"

# ── prerequisites ─────────────────────────────────────────────────────
if ! command -v cargo &>/dev/null; then
  echo "ERROR: cargo not found. Install Rust: https://rustup.rs" >&2
  exit 1
fi

# ── build ─────────────────────────────────────────────────────────────
echo "==> Building (this takes a minute the first time)…"
cargo build --release --manifest-path "$SCRIPT_DIR/Cargo.toml"

# ── install binary ────────────────────────────────────────────────────
mkdir -p "$BIN_DIR"
install -m 755 "$SCRIPT_DIR/target/release/batmon" "$BIN_DIR/batmon"
echo "    binary → $BIN_DIR/batmon"

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) echo "    note: $BIN_DIR is not on your PATH; the service does not need it, but you will for 'batmon --oneshot'" ;;
esac

# ── data directory ────────────────────────────────────────────────────
mkdir -p "$DATA_DIR"

# Disable btrfs Copy-on-Write to prevent SQLite write amplification and
# fragmentation. Only affects files created afterwards, which is why it is
# applied to the directory rather than the databases.
if command -v chattr &>/dev/null && [ "$(stat -f -c %T "$DATA_DIR" 2>/dev/null || true)" = "btrfs" ]; then
  if chattr +C "$DATA_DIR" 2>/dev/null; then
    echo "    btrfs detected: disabled CoW (chattr +C) on $DATA_DIR"
  else
    echo "    warning: could not disable CoW on $DATA_DIR" >&2
  fi
fi

# Remove the TypeScript sources left by earlier releases. The databases beside
# them are deliberately untouched — the Rust build reads and continues them.
if [ -d "$DATA_DIR/src" ]; then
  rm -rf "$DATA_DIR/src"
  echo "    removed superseded TypeScript sources from $DATA_DIR/src"
fi

# ── install systemd unit ──────────────────────────────────────────────
mkdir -p "$SYSTEMD_DIR"

# Clean up a legacy timer if one is still present.
systemctl --user disable --now batmon.timer 2>/dev/null || true
rm -f "$SYSTEMD_DIR/batmon.timer"

cat > "$SYSTEMD_DIR/batmon.service" <<EOF
[Unit]
Description=batmon – battery health monitor & flight recorder (sysfs → SQLite)
Documentation=https://github.com/InvictusNavarchus/batmon

[Service]
Type=simple
ExecStart=${BIN_DIR}/batmon
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
if "$BIN_DIR/batmon" --oneshot; then
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
echo "      sqlite3 $DATA_DIR/battery.db 'SELECT * FROM samples ORDER BY id DESC LIMIT 1;'"
echo ""
echo "    Check flight recorder data:"
echo "      sqlite3 $DATA_DIR/debug.db 'SELECT * FROM samples ORDER BY id DESC LIMIT 5;'"
echo ""
echo "    Check service status:"
echo "      systemctl --user status batmon.service"
echo ""
echo "    View logs:"
echo "      journalctl --user -u batmon -f"
echo ""
echo "==> Done. Battery health monitoring and flight recording are active."
