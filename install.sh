#!/usr/bin/env bash
set -euo pipefail

TMP_DIR=""
cleanup() {
  if [ -n "${TMP_DIR:-}" ] && [ -d "${TMP_DIR:-}" ]; then
    rm -rf "$TMP_DIR"
  fi
}
trap cleanup EXIT

main() {
  local REPO="InvictusNavarchus/batmon"
  local BIN_DIR="${BIN_DIR:-$HOME/.local/bin}"
  local DATA_DIR="$HOME/.local/share/batmon"
  local SYSTEMD_DIR="$HOME/.config/systemd/user"
  local SCRIPT_DIR=""

  if [ -n "${BASH_SOURCE[0]:-}" ] && [ -f "${BASH_SOURCE[0]}" ]; then
    SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  fi

  echo "==> batmon installer"

  # ── platform & architecture check ───────────────────────────────────
  local os
  os="$(uname -s)"
  if [ "$os" != "Linux" ]; then
    echo "ERROR: batmon only runs on Linux (detected: $os)" >&2
    exit 1
  fi

  local arch target
  arch="$(uname -m)"
  case "$arch" in
    x86_64|amd64)
      target="x86_64-unknown-linux-musl"
      ;;
    aarch64|arm64)
      target="aarch64-unknown-linux-musl"
      ;;
    *)
      echo "ERROR: Unsupported architecture: $arch (supported: x86_64, aarch64)" >&2
      exit 1
      ;;
  esac

  # ── prerequisites ───────────────────────────────────────────────────
  if ! command -v systemctl &>/dev/null; then
    echo "ERROR: systemctl not found. batmon requires systemd user services." >&2
    exit 1
  fi

  # ── binary acquisition (local build vs. release download) ───────────
  local binary=""
  local force_build=0

  for arg in "$@"; do
    if [ "$arg" = "--build" ] || [ "$arg" = "-b" ]; then
      force_build=1
    fi
  done

  # Build from source if explicitly requested or running locally in repo with Cargo
  if [ "$force_build" -eq 1 ] || { [ -n "$SCRIPT_DIR" ] && [ -f "$SCRIPT_DIR/Cargo.toml" ] && [ "${BATMON_FROM_RELEASE:-0}" != "1" ]; }; then
    if command -v cargo &>/dev/null; then
      echo "==> Building from source (local repository detected)…"
      cargo build --release --manifest-path "$SCRIPT_DIR/Cargo.toml"

      local target_dir
      target_dir="$(cargo metadata --format-version 1 --no-deps --manifest-path "$SCRIPT_DIR/Cargo.toml" 2>/dev/null \
        | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"
      binary="${target_dir:-$SCRIPT_DIR/target}/release/batmon"
    elif [ "$force_build" -eq 1 ]; then
      echo "ERROR: cargo not found. Install Rust: https://rustup.rs" >&2
      exit 1
    else
      echo "    cargo not found, falling back to pre-compiled binary download…"
    fi
  fi

  # Download release binary if not built locally
  if [ -z "$binary" ] || [ ! -x "$binary" ]; then
    local version="${BATMON_VERSION:-latest}"
    local download_url
    if [ "$version" = "latest" ]; then
      download_url="https://github.com/$REPO/releases/latest/download/batmon-${target}.tar.gz"
    else
      download_url="https://github.com/$REPO/releases/download/${version}/batmon-${target}.tar.gz"
    fi

    echo "==> Downloading pre-compiled binary ($target)…"
    TMP_DIR="$(mktemp -d -t batmon-install.XXXXXX)"
    local archive="$TMP_DIR/batmon.tar.gz"

    if command -v curl &>/dev/null; then
      curl -fsSL "$download_url" -o "$archive"
    elif command -v wget &>/dev/null; then
      wget -qO "$archive" "$download_url"
    else
      echo "ERROR: Neither curl nor wget found. Please install curl or wget." >&2
      exit 1
    fi

    tar -xzf "$archive" -C "$TMP_DIR"
    binary="$TMP_DIR/batmon"
  fi

  if [ ! -x "$binary" ]; then
    echo "ERROR: Failed to obtain an executable batmon binary." >&2
    exit 1
  fi

  # ── stop running service ────────────────────────────────────────────
  if systemctl --user is-active --quiet batmon.service 2>/dev/null; then
    echo "==> Stopping active batmon service…"
    systemctl --user stop batmon.service 2>/dev/null || true
  fi

  # ── install binary ──────────────────────────────────────────────────
  mkdir -p "$BIN_DIR"
  install -m 755 "$binary" "$BIN_DIR/batmon"
  echo "    binary → $BIN_DIR/batmon"

  case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *) echo "    note: $BIN_DIR is not on your PATH; the service does not need it, but you will for 'batmon --oneshot'" ;;
  esac

  # ── data directory ──────────────────────────────────────────────────
  mkdir -p "$DATA_DIR"

  if command -v chattr &>/dev/null && [ "$(stat -f -c %T "$DATA_DIR" 2>/dev/null || true)" = "btrfs" ]; then
    if chattr +C "$DATA_DIR" 2>/dev/null; then
      echo "    btrfs detected: disabled CoW (chattr +C) on $DATA_DIR"
    else
      echo "    warning: could not disable CoW on $DATA_DIR" >&2
    fi
  fi

  if [ -d "$DATA_DIR/src" ]; then
    rm -rf "$DATA_DIR/src"
    echo "    removed superseded TypeScript sources from $DATA_DIR/src"
  fi

  # ── install systemd unit ────────────────────────────────────────────
  mkdir -p "$SYSTEMD_DIR"

  systemctl --user disable --now batmon.timer 2>/dev/null || true
  rm -f "$SYSTEMD_DIR/batmon.timer"

  cat > "$SYSTEMD_DIR/batmon.service" <<EOF
[Unit]
Description=batmon – battery health monitor & flight recorder (sysfs → SQLite)
Documentation=https://github.com/InvictusNavarchus/batmon

[Service]
Type=simple
ExecStart=%h/.local/bin/batmon
Restart=on-failure
RestartSec=5s
Nice=10

[Install]
WantedBy=default.target
EOF

  echo "    service → $SYSTEMD_DIR/batmon.service"
  systemctl --user daemon-reload

  # ── verify ──────────────────────────────────────────────────────────
  echo ""
  echo "==> Running sample verification…"
  if "$BIN_DIR/batmon" --oneshot; then
    echo "    ✓ sample stored"
  else
    echo "    ✗ test run failed – check your system configuration"
    exit 1
  fi

  # ── enable & start service ──────────────────────────────────────────
  systemctl --user enable --now batmon.service
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
}

main "$@"
