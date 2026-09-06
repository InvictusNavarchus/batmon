#!/usr/bin/env bash
# Start a long-running differential between the TypeScript and Rust daemons.
#
# Both run concurrently against isolated $HOME roots, so neither touches the
# production databases. Units are persistent and enabled, so the experiment
# survives a reboot — which is deliberate: a reboot exercises the cycle
# integrator's carry-forward path, one of the things a short run cannot reach.
#
#   legacy/tools/differential-run.sh          # start
#   legacy/tools/differential-report.py       # analyse, any time
#   legacy/tools/differential-stop.sh         # stop
set -euo pipefail

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
ROOT="$HOME/.batmon-differential"
UNITS="$HOME/.config/systemd/user"
BINARY="$REPO/target/release/batmon"

echo "==> batmon differential harness"

# ── prerequisites ─────────────────────────────────────────────────────
if ! command -v bun &>/dev/null; then
  echo "ERROR: bun not found; the TypeScript side cannot run" >&2
  exit 1
fi
BUN="$(command -v bun)"

if [ ! -x "$BINARY" ]; then
  echo "    building the release binary…"
  cargo build --release --manifest-path "$REPO/Cargo.toml"
fi

if [ ! -f "$REPO/legacy/src/index.ts" ]; then
  echo "ERROR: legacy/src/index.ts is missing; the TypeScript side cannot run" >&2
  exit 1
fi

# ── isolated roots ────────────────────────────────────────────────────
mkdir -p "$ROOT/rust" "$ROOT/ts" "$ROOT/stats"
echo "    data root → $ROOT"

# ── resource sampler ──────────────────────────────────────────────────
# Appends RSS and thread counts every 30s so the report can say whether either
# implementation grows over a day, which a point-in-time reading cannot.
cat > "$ROOT/stats/sample-resources.sh" <<'INNER'
#!/usr/bin/env bash
CSV="$1"
[ -s "$CSV" ] || echo "ts,impl,rss_kb,threads,restarts" > "$CSV"
while true; do
  now="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  for pair in "rust:batmon-diff-rust" "ts:batmon-diff-ts"; do
    name="${pair%%:*}"; unit="${pair##*:}"
    pid="$(systemctl --user show -p MainPID --value "$unit.service" 2>/dev/null || echo 0)"
    restarts="$(systemctl --user show -p NRestarts --value "$unit.service" 2>/dev/null || echo 0)"
    if [ -n "$pid" ] && [ "$pid" != "0" ] && [ -r "/proc/$pid/status" ]; then
      rss="$(awk '/VmRSS/{print $2}' "/proc/$pid/status")"
      threads="$(awk '/Threads/{print $2}' "/proc/$pid/status")"
      echo "$now,$name,$rss,$threads,$restarts" >> "$CSV"
    fi
  done
  sleep 30
done
INNER
chmod +x "$ROOT/stats/sample-resources.sh"

# ── units ─────────────────────────────────────────────────────────────
mkdir -p "$UNITS"

write_unit() {
  local name="$1" desc="$2" home="$3" exec="$4"
  cat > "$UNITS/$name.service" <<EOF
[Unit]
Description=$desc
Documentation=https://github.com/InvictusNavarchus/batmon

[Service]
Type=simple
Environment="HOME=$home"
ExecStart=$exec
Restart=on-failure
RestartSec=5s
Nice=10

[Install]
WantedBy=default.target
EOF
}

write_unit batmon-diff-rust "batmon differential — Rust implementation" \
  "$ROOT/rust" "$BINARY"
write_unit batmon-diff-ts "batmon differential — TypeScript implementation" \
  "$ROOT/ts" "$BUN run $REPO/legacy/src/index.ts"
write_unit batmon-diff-stats "batmon differential — resource sampler" \
  "$ROOT/stats" "$ROOT/stats/sample-resources.sh $ROOT/stats/resources.csv"

echo "    units → $UNITS/batmon-diff-{rust,ts,stats}.service"

# ── start ─────────────────────────────────────────────────────────────
systemctl --user daemon-reload
# Started together so both baselines begin from the same instant. CPU
# utilisation and per-process deltas are rates, so a staggered start would put
# their sampling windows permanently out of phase.
systemctl --user enable --now batmon-diff-rust.service batmon-diff-ts.service \
  batmon-diff-stats.service >/dev/null 2>&1

sleep 3
echo ""
for unit in batmon-diff-rust batmon-diff-ts batmon-diff-stats; do
  printf "    %-22s %s\n" "$unit" "$(systemctl --user is-active $unit.service)"
done

STARTED="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo "$STARTED" > "$ROOT/started-at"

echo ""
echo "==> Running. Started $STARTED"
echo "    Production batmon.service is untouched and still writing to ~/.local/share/batmon."
echo ""
echo "    Progress:  legacy/tools/differential-report.py"
echo "    Stop:      legacy/tools/differential-stop.sh"
