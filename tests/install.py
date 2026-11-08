"""Installer regressions using temporary paths and mocked network/systemd."""

import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest


REPO = Path(__file__).resolve().parents[1]
MOCK = r'''#!/usr/bin/env python3
import json, os, pathlib, shutil, sys
name, args = pathlib.Path(sys.argv[0]).name, sys.argv[1:]
with open(os.environ["COMMAND_LOG"], "a") as log:
    log.write(json.dumps([name, *args]) + "\n")
if name == "uname": print("Linux" if args == ["-s"] else os.environ.get("MOCK_ARCH", "x86_64"))
elif name == "stat": print("ext4")
elif name == "cargo":
    if os.environ.get("MOCK_BUILD_FAIL"): sys.exit(1)
    target = pathlib.Path(os.environ["SOURCE_TARGET"])
    if args[0] == "metadata": print(json.dumps({"target_directory": str(target)}))
    else:
        binary = target / "release/batmon"
        binary.parent.mkdir(parents=True, exist_ok=True)
        binary.write_text("#!/bin/sh\nexit 0\n")
        binary.chmod(0o755)
elif name == "curl":
    if "-w" in args:
        print("https://github.com/InvictusNavarchus/batmon/releases/tag/v0.8.5", end="")
    else:
        url = next(a for a in args if a.startswith("https://"))
        source = pathlib.Path(os.environ["RELEASE_DIR"]) / url.rsplit("/", 1)[1]
        if not source.is_file(): sys.exit(22)
        shutil.copyfile(source, args[args.index("-o") + 1])
'''


class Installation(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="batmon-install-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.source = self.root / "checkout"
        self.source.mkdir()
        for name in ("install.sh", "uninstall.sh"):
            # Redirect the user's home in script copies, without overriding HOME.
            text = (REPO / name).read_text().replace("$HOME", "${TEST_USER_HOME}")
            (self.source / name).write_text(text)
        self.home = self.root / "user"
        self.home.mkdir()
        self.bin = self.root / "bin"
        self.bin.mkdir()
        for name in ("uname", "curl", "systemctl", "stat", "cargo"):
            command = self.bin / name
            command.write_text(MOCK)
            command.chmod(0o755)
        self.log = self.root / "commands.jsonl"
        self.releases = self.root / "releases"
        self.releases.mkdir()
        for target in ("x86_64", "aarch64"):
            archive = self.releases / f"batmon-{target}-unknown-linux-musl.tar.gz"
            binary = b"#!/bin/sh\nexit 0\n"
            with tarfile.open(archive, "w:gz") as bundle:
                info = tarfile.TarInfo("batmon")
                info.size, info.mode = len(binary), 0o755
                bundle.addfile(info, io.BytesIO(binary))
            checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
            archive.with_name(archive.name + ".sha256").write_text(f"{checksum}  {archive.name}\n")
        self.env = dict(os.environ, PATH=str(self.bin) + ":" + os.environ["PATH"],
                        TEST_USER_HOME=str(self.home), COMMAND_LOG=str(self.log), RELEASE_DIR=str(self.releases),
                        SOURCE_TARGET=str(self.source / "target"))
        for key in ("BIN_DIR", "BATMON_VERSION", "BATMON_FROM_RELEASE"):
            self.env.pop(key, None)

    def commands(self):
        return [json.loads(line) for line in self.log.read_text().splitlines()] if self.log.exists() else []

    def run_script(self, *args, env=None, ok=True, script="install.sh"):
        result = subprocess.run(["bash", str(self.source / script), *args], cwd=self.root,
                                env=env or self.env, text=True, capture_output=True)
        self.assertEqual(result.returncode == 0, ok, result.stdout + result.stderr)
        return result

    def test_verified_download_installs_each_supported_architecture(self):
        for arch in ("x86_64", "aarch64"):
            self.run_script(env=dict(self.env, MOCK_ARCH=arch))
            self.assertTrue((self.home / ".local/bin/batmon").is_file())
        downloads = [arg for c in self.commands() if c[0] == "curl" for arg in c if arg.startswith("https://") and "/download/" in arg]
        self.assertTrue(downloads)
        self.assertTrue(all("/download/v0.8.5/" in url for url in downloads))

    def test_pinned_version_does_not_resolve_latest(self):
        self.run_script(env=dict(self.env, BATMON_VERSION="v0.8.5"))
        self.assertFalse(any(c[0] == "curl" and "-w" in c for c in self.commands()))

    def test_corruption_leaves_binary_and_service_untouched(self):
        binary = self.home / ".local/bin/batmon"
        binary.parent.mkdir(parents=True)
        binary.write_text("old binary")
        archive = self.releases / "batmon-x86_64-unknown-linux-musl.tar.gz"
        archive.write_bytes(archive.read_bytes() + b"corruption")
        self.run_script(ok=False)
        self.assertEqual(binary.read_text(), "old binary")
        self.assertFalse(any(c[0] == "systemctl" for c in self.commands()))

    def test_missing_checksum_leaves_installation_untouched(self):
        (self.releases / "batmon-x86_64-unknown-linux-musl.tar.gz.sha256").unlink()
        self.run_script(ok=False)
        self.assertFalse((self.home / ".local/bin/batmon").exists())
        self.assertFalse(any(c[0] == "systemctl" for c in self.commands()))

    def test_invalid_tag_is_rejected_before_download(self):
        self.run_script(env=dict(self.env, BATMON_VERSION="../master"), ok=False)
        self.assertFalse(any(c[0] in ("curl", "systemctl") for c in self.commands()))

    def test_checkout_defaults_to_release_download_even_with_cargo_installed(self):
        (self.source / "Cargo.toml").write_text((REPO / "Cargo.toml").read_text())
        self.run_script()
        self.assertTrue(any(c[0] == "curl" for c in self.commands()))
        self.assertFalse(any(c[0] == "cargo" for c in self.commands()))

    def test_explicit_source_build_is_locked_and_does_not_download(self):
        (self.source / "Cargo.toml").write_text((REPO / "Cargo.toml").read_text())
        self.run_script("--build")
        self.assertTrue(any(c[:2] == ["cargo", "build"] and "--locked" in c for c in self.commands()))
        self.assertFalse(any(c[0] == "curl" for c in self.commands()))

    def test_unknown_options_and_build_without_checkout_fail_before_changes(self):
        self.run_script("--unknown", ok=False)
        self.run_script("--build", ok=False)
        self.assertFalse(any(c[0] in ("curl", "cargo", "systemctl") for c in self.commands()))

    def test_failed_source_build_does_not_fall_back_to_a_release(self):
        (self.source / "Cargo.toml").write_text((REPO / "Cargo.toml").read_text())
        self.run_script("--build", env=dict(self.env, MOCK_BUILD_FAIL="1"), ok=False)
        self.assertFalse(any(c[0] in ("curl", "systemctl") for c in self.commands()))

    def test_custom_directory_is_used_by_service_and_remembered_for_updates(self):
        custom = self.root / 'custom bin %$"'
        self.run_script(env=dict(self.env, BIN_DIR=str(custom)))
        unit = (self.home / ".config/systemd/user/batmon.service").read_text()
        escaped = str(custom / "batmon").replace('\\', '\\\\').replace('"', '\\"').replace('%', '%%')
        self.assertIn('ExecStart=:"' + escaped + '"', unit)
        self.run_script()
        self.assertTrue((custom / "batmon").exists())
        self.assertFalse((self.home / ".local/bin/batmon").exists())
        self.assertEqual((self.home / ".local/share/batmon/bin-dir").read_text().strip(), str(custom))

    def test_installed_uninstaller_works_after_checkout_is_deleted_and_preserves_data(self):
        custom = self.root / "custom bin"
        self.run_script(env=dict(self.env, BIN_DIR=str(custom)))
        data = self.home / ".local/share/batmon"
        (data / "battery.db").write_text("history")
        shutil.rmtree(self.source)
        self.run_script(script=str(custom / "batmon-uninstall"))
        self.assertFalse((custom / "batmon").exists())
        self.assertFalse((custom / "batmon-uninstall").exists())
        self.assertFalse((data / "bin-dir").exists())
        self.assertFalse((self.home / ".config/systemd/user/batmon.service").exists())
        self.assertEqual((data / "battery.db").read_text(), "history")

    def test_changing_directory_removes_the_previous_install_after_success(self):
        first, second = self.root / "first", self.root / "second"
        self.run_script(env=dict(self.env, BIN_DIR=str(first)))
        self.run_script(env=dict(self.env, BIN_DIR=str(second)))
        self.assertFalse((first / "batmon").exists())
        self.assertFalse((first / "batmon-uninstall").exists())
        self.assertTrue((second / "batmon").exists())

    def test_control_characters_in_binary_directory_are_rejected(self):
        self.run_script(env=dict(self.env, BIN_DIR="bad\npath"), ok=False)
        self.assertEqual(self.commands(), [])


if __name__ == "__main__":
    unittest.main()
