# /// script
# requires-python = ">=3.11,<3.12"
# dependencies = ["matplotlib>=3.10,<4", "seaborn>=0.13.2,<0.14", "python-dateutil>=2.9,<5"]
# ///
"""Release validation shared by cargo-release and GitHub Actions."""

import os
from pathlib import Path
import subprocess
import sys
import tempfile
import tomllib


def main():
    root = Path(__file__).resolve().parents[1]
    toolchain = tomllib.loads((root / "rust-toolchain.toml").read_text())["toolchain"]["channel"]
    with tempfile.TemporaryDirectory(prefix="batmon-validation-") as temporary:
        env = dict(os.environ,
                   RUSTUP_TOOLCHAIN=toolchain,
                   DBUS_SYSTEM_BUS_ADDRESS=f"unix:path={temporary}/no-system-bus",
                   DBUS_SESSION_BUS_ADDRESS=f"unix:path={temporary}/no-session-bus",
                   MPLBACKEND="Agg", MPLCONFIGDIR=temporary,
                   PYTHONDONTWRITEBYTECODE="1")
        for command in (
            ["cargo", "test", "--locked"],
            [sys.executable, "-B", "tests/install.py"],
            [sys.executable, "-B", "tests/plot.py"],
        ):
            print("Validating:", " ".join(command), flush=True)
            result = subprocess.run(command, cwd=root, env=env, check=False)
            if result.returncode:
                return result.returncode if result.returncode > 0 else 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
