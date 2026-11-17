#!/bin/sh
# Shared local/CI release gate; no tagging, publication, or live installation.
set -eu
cd "$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
for tool in cargo rustup uv; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "Release validation requires $tool" >&2
        exit 1
    }
done
# Honor the repository toolchain, not an inherited shell override.
unset RUSTUP_TOOLCHAIN
if [ "$(uv --version | cut -d ' ' -f 2)" != "0.13.1" ]; then
    echo 'Release validation requires uv 0.13.1 (matching CI)' >&2
    exit 1
fi
test -f scripts/validate.py.lock || {
    echo 'Missing scripts/validate.py.lock; restore the committed lockfile' >&2
    exit 1
}
exec uv run --managed-python --python 3.11 --locked --script scripts/validate.py
