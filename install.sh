#!/usr/bin/env bash
set -euo pipefail

TMP_DIR=""
INSTALL_STAGE=""
cleanup() {
  if [ -n "${INSTALL_STAGE:-}" ]; then rm -rf -- "$INSTALL_STAGE"; fi
  if [ -n "${TMP_DIR:-}" ] && [ -d "${TMP_DIR:-}" ]; then
    rm -rf "$TMP_DIR"
  fi
}
trap cleanup EXIT

# Saved with its installation paths so removal works without this checkout.
uninstall() {
  systemctl --user disable --now batmon.service
  if [ -e "$SYSTEMD_DIR/batmon.timer" ] || [ -L "$SYSTEMD_DIR/batmon.timer" ]; then
    systemctl --user disable --now batmon.timer
  fi
  rm -f "$SYSTEMD_DIR/batmon.service" "$SYSTEMD_DIR/batmon.timer"
  systemctl --user daemon-reload
  rm -f "$BIN_DIR/batmon" "$BIN_DIR/batmon-plot" "$BIN_DIR/batmon-uninstall"
  rm -f "$DATA_DIR/bin-dir" "$DATA_DIR/uninstall.sh"
  echo "batmon removed. Databases preserved in $DATA_DIR."
}

main() {
  local REPO="InvictusNavarchus/batmon"
  local DATA_DIR="$HOME/.local/share/batmon"
  local SYSTEMD_DIR="$HOME/.config/systemd/user"
  local previous_bin_dir=""
  if [ -f "$DATA_DIR/bin-dir" ]; then
    previous_bin_dir=$(cat "$DATA_DIR/bin-dir")
  fi
  local BIN_DIR="${BIN_DIR:-${previous_bin_dir:-$HOME/.local/bin}}"
  local SCRIPT_DIR=""

  if [ -n "${BASH_SOURCE[0]:-}" ] && [ -f "${BASH_SOURCE[0]}" ]; then
    SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  fi

  local force_build=0
  local enable_service=0
  for arg in "$@"; do
    case "$arg" in
      --build | -b) force_build=1 ;;
      --enable) enable_service=1 ;;
      --help | -h)
        echo "Usage: bash install.sh [--build] [--enable]"
        echo "Installs or updates to the latest verified release by default."
        echo "--build compiles a local checkout; BIN_DIR selects the installation directory."
        echo "BATMON_VERSION=vX.Y.Z selects a release."
        echo "--enable explicitly enables and starts the user service."
        echo "Otherwise new installs stay inactive; updates preserve activation state."
        echo "Lingering is never changed; see README for boot/logout recording."
        return
        ;;
      *) echo "ERROR: Unknown option '$arg'. Use --help for usage." >&2; exit 1 ;;
    esac
  done

  if [[ "$BIN_DIR" =~ [[:cntrl:]] ]]; then
    echo "ERROR: BIN_DIR must not contain control characters." >&2
    exit 1
  fi
  case "$BIN_DIR" in
    /*) ;;
    *) BIN_DIR="$PWD/$BIN_DIR" ;;
  esac

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
  local plot_script=""

  # A checkout does not change the default: source builds are always explicit.
  if [ "$force_build" -eq 1 ]; then
    if [ -z "$SCRIPT_DIR" ] || [ ! -f "$SCRIPT_DIR/Cargo.toml" ]; then
      echo "ERROR: --build requires running install.sh from a source checkout." >&2
      exit 1
    fi
    if command -v cargo &>/dev/null; then
      echo "==> Building from source…"
      cargo build --release --locked --manifest-path "$SCRIPT_DIR/Cargo.toml"

      local target_dir
      target_dir="$(cargo metadata --format-version 1 --no-deps --manifest-path "$SCRIPT_DIR/Cargo.toml" 2>/dev/null \
        | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"
      binary="${target_dir:-$SCRIPT_DIR/target}/release/batmon"
    else
      echo "ERROR: cargo not found. Install Rust: https://rustup.rs" >&2
      exit 1
    fi
    if [ ! -x "$binary" ]; then
      echo "ERROR: The source build did not produce an executable at $binary." >&2
      exit 1
    fi
    plot_script="$SCRIPT_DIR/scripts/batmon-plot.py"
    if [ ! -f "$plot_script" ]; then
      echo "ERROR: Plotting helper missing from source checkout: $plot_script." >&2
      exit 1
    fi
  fi

  # Download release binary if not built locally
  if [ -z "$binary" ] || [ ! -x "$binary" ]; then
    for tool in curl tar sha256sum; do
      if ! command -v "$tool" >/dev/null 2>&1; then
        echo "ERROR: $tool is required to download and verify releases." >&2
        exit 1
      fi
    done
    local version="${BATMON_VERSION:-latest}"
    if [ "$version" = "latest" ]; then
      local url
      url=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest")
      version="${url##*/}"
    fi
    if [[ ! "$version" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]]; then
      echo "ERROR: Expected a release tag such as v0.8.4; got '$version'." >&2
      exit 1
    fi
    local base="https://github.com/$REPO/releases/download/$version"
    local filename="batmon-${target}.tar.gz"

    echo "==> Downloading $version ($target)…"
    TMP_DIR="$(mktemp -d -t batmon-install.XXXXXX)"
    local archive="$TMP_DIR/$filename"
    curl -fsSL "$base/$filename" -o "$archive"
    curl -fsSL "$base/$filename.sha256" -o "$archive.sha256"
    if ! (cd "$TMP_DIR" && sha256sum --check "$filename.sha256"); then
      echo "ERROR: Release checksum verification failed; installation was not changed." >&2
      exit 1
    fi

    tar -xzf "$archive" -C "$TMP_DIR"
    binary="$TMP_DIR/batmon"
    # Pinned releases from before the helper was introduced remain installable.
    if [ -f "$TMP_DIR/batmon-plot" ]; then
      plot_script="$TMP_DIR/batmon-plot"
    fi
  fi

  if [ ! -x "$binary" ]; then
    echo "ERROR: Failed to obtain an executable batmon binary." >&2
    exit 1
  fi

  # Verify without recording samples or touching the user's databases.
  "$binary" --version
  systemctl --user show-environment >/dev/null
  local service_was_active=0 service_masked=0 service_state
  service_state=$(systemctl --user is-enabled batmon.service 2>/dev/null || true)
  case "$service_state" in masked|masked-runtime) service_masked=1 ;; esac
  if [ -L "$SYSTEMD_DIR/batmon.service" ]; then
    if [ "$(readlink "$SYSTEMD_DIR/batmon.service")" = /dev/null ]; then
      service_masked=1
    else
      echo "ERROR: Refusing to replace a symlink unit: $SYSTEMD_DIR/batmon.service" >&2
      exit 1
    fi
  elif [ -e "$SYSTEMD_DIR/batmon.service" ] && [ ! -f "$SYSTEMD_DIR/batmon.service" ]; then
    echo "ERROR: Refusing to replace a non-regular service unit." >&2
    exit 1
  fi
  if [ "$enable_service" -eq 1 ] && [ "$service_masked" -eq 1 ]; then
    echo "ERROR: batmon.service is masked; unmask it explicitly before using --enable." >&2
    exit 1
  fi
  if systemctl --user is-active --quiet batmon.service; then
    service_was_active=1
  fi

  # Legacy timer installations need an explicit cutover, not silent activation.
  if [ -e "$SYSTEMD_DIR/batmon.timer" ] || [ -L "$SYSTEMD_DIR/batmon.timer" ]; then
    echo "ERROR: Legacy batmon.timer found. Retire it explicitly before installing the daemon service." >&2
    exit 1
  fi

  # ── install binary ──────────────────────────────────────────────────
  mkdir -p "$BIN_DIR"
  BIN_DIR=$(cd "$BIN_DIR" && pwd -P)
  # Replace the executable inode without stopping a working collector first.
  INSTALL_STAGE=$(mktemp -d "$BIN_DIR/.batmon-install.XXXXXX")
  install -m 755 "$binary" "$INSTALL_STAGE/batmon"
  mv -fT "$INSTALL_STAGE/batmon" "$BIN_DIR/batmon"
  echo "    binary → $BIN_DIR/batmon"
  if [ -n "$plot_script" ]; then
    install -m 755 "$plot_script" "$BIN_DIR/batmon-plot"
    echo "    plotting helper → $BIN_DIR/batmon-plot (optional; requires uv)"
  else
    rm -f "$BIN_DIR/batmon-plot"
  fi

  case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *) echo "    note: $BIN_DIR is not on your PATH; the service does not need it, but you will for 'batmon --oneshot'" ;;
  esac

  # ── data directory ──────────────────────────────────────────────────
  mkdir -p "$DATA_DIR"
  printf '%s\n' "$BIN_DIR" > "$DATA_DIR/bin-dir"
  {
    printf '#!/usr/bin/env bash\nset -euo pipefail\n'
    printf 'BIN_DIR=%q\nDATA_DIR=%q\nSYSTEMD_DIR=%q\n' "$BIN_DIR" "$DATA_DIR" "$SYSTEMD_DIR"
    declare -f uninstall
    printf '\nuninstall\n'
  } > "$DATA_DIR/uninstall.sh"
  chmod 755 "$DATA_DIR/uninstall.sh"
  ln -sf "$DATA_DIR/uninstall.sh" "$BIN_DIR/batmon-uninstall"

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

  local service_binary
  service_binary=$(printf '%s' "$BIN_DIR/batmon" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' -e 's/%/%%/g')
  cat > "$INSTALL_STAGE/batmon.service" <<EOF
[Unit]
Description=batmon – battery health monitor & flight recorder (sysfs → SQLite)
Documentation=https://github.com/InvictusNavarchus/batmon

[Service]
Type=simple
ExecStart=:"$service_binary"
Restart=on-failure
RestartSec=5s
Nice=10

[Install]
WantedBy=default.target
EOF

  if [ "$service_masked" -eq 0 ]; then
    install -m 644 "$INSTALL_STAGE/batmon.service" "$SYSTEMD_DIR/batmon.service"
  fi
  echo "    service → $SYSTEMD_DIR/batmon.service (existing masks and drop-ins preserved)"
  systemctl --user daemon-reload

  # Activation is explicit on first install; upgrades retain operator choices.
  if [ "$enable_service" -eq 1 ]; then
    systemctl --user enable --now batmon.service
    if [ "$service_was_active" -eq 1 ]; then
      systemctl --user try-restart batmon.service
    fi
    echo "    service → enabled and started"
  elif [ "$service_was_active" -eq 1 ] && [ "$service_masked" -eq 0 ]; then
    systemctl --user try-restart batmon.service
    echo "    service → active collector updated; enablement unchanged"
  else
    echo "    service → activation unchanged; use --enable to enable and start"
  fi
  if [ -n "$previous_bin_dir" ] && [ "$previous_bin_dir" != "$BIN_DIR" ]; then
    rm -f "$previous_bin_dir/batmon" "$previous_bin_dir/batmon-plot" "$previous_bin_dir/batmon-uninstall"
  fi
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
  echo "==> Installation complete. Check service status to confirm recording."
}

main "$@"
