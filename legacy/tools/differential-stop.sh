#!/usr/bin/env bash
# Stop the differential harness. Data is kept unless --purge is given.
set -euo pipefail

ROOT="$HOME/.batmon-differential"
UNITS="$HOME/.config/systemd/user"

echo "==> stopping batmon differential harness"

for unit in batmon-diff-rust batmon-diff-ts batmon-diff-stats; do
  systemctl --user disable --now "$unit.service" 2>/dev/null || true
  rm -f "$UNITS/$unit.service"
done
systemctl --user daemon-reload
systemctl --user reset-failed 2>/dev/null || true

echo "    units stopped and removed"

if [ "${1:-}" = "--purge" ]; then
  rm -rf "$ROOT"
  echo "    data purged from $ROOT"
else
  echo "    data kept at $ROOT (re-run with --purge to delete)"
fi
