# Working on batmon

Speak English. Discuss proposed codebase changes and obtain explicit approval
before editing. Once a task is approved, perform its relevant isolated testing
yourself; do not ask the user to create fixtures or run commands you can run.
Keep changes within the approved task.

This laptop also runs the user's installed batmon. Treat its binary, service,
databases, and desktop session as live user state. Development tests should use
temporary directories and the existing test harnesses. Docker is not required.

## Test commands

Run commands from the repository root:

```bash
# Unit, property, and executable integration tests.
cargo test --locked

# Installer/uninstaller tests with temporary paths and mocked network/systemd.
python3 tests/install.py
```

During development, select the relevant tests first:

```bash
cargo test --locked --lib telemetry::battery::tests
cargo test --locked --lib telemetry::thermal::tests
cargo test --locked --lib daemon::tests
cargo test --locked --lib migrations::tests
cargo test --locked --test cli
```

Run the full Rust suite after code changes; run installer tests when changing
installation or release behavior. For documentation-only changes, verify the
documented paths, commands, and examples rather than adding unrelated tests.
Report what ran, its outcome, and any verification that remains unavailable.

Check pre-commit hooks before linting or formatting. This repository configures
Lefthook in `lefthook.yml`; let its commit hooks run formatting and Clippy rather
than invoking manual formatting or fix commands. Do not bypass failing hooks.

## Choose the smallest suitable test harness

| Behavior | Existing harness |
| --- | --- |
| Battery attributes and conversions | `src/telemetry/battery/tests.rs`: `battery()` creates a `TempDir` and `BatteryReader` |
| Sensor discovery and temperature/power readings | `src/telemetry/thermal/tests.rs`: `device()` populates a fake hwmon tree |
| Battery discovery and path selection | `src/paths/tests.rs`: temporary power-supply directories |
| CPU, memory, process, and other procfs parsing | Tests under `src/telemetry/`: fixture files and explicit reader paths |
| Alerts and threshold transitions | Tests under `src/alerts/`: synthetic samples and recording notifiers |
| Sampling, gaps, cycles, and retention | `src/daemon/tests.rs`: `Scripted`, `WithGaps`, `RecordingNotifier`, and in-memory stores |
| SQLite schemas and migrations | `src/db/tests.rs` and `src/migrations/tests.rs`: in-memory or temporary databases |
| CLI arguments, exit codes, persistence, signals | `tests/cli.rs`: `environment()`, `batmon()`, `database_dir()`, and `query()` |
| Installation and removal | `tests/install.py`: temporary installations and mocked external commands |

Prefer scripted ticks to real sleeps for state transitions and retention tests.
Use executable tests when process behavior is what needs verifying. Add regression
coverage for changed behavior, with assertions on observable results; do not add
tests merely to mirror implementation details.

Unit tests needing private items belong in descendant modules. Test modules of
100 lines or more live in a separate file with a `#[cfg(test)] mod tests;`
declaration in the parent. Ensure every new test module is actually declared.

## Create simulated filesystem inputs

Use `tempfile::TempDir` and keep its owner alive until all readers, database
connections, and child processes have finished. Sysfs fixtures are ordinary
directories containing one text file per attribute, usually ending in a newline.
Create and modify these files only under the test's temporary root.

For example, inside `src/telemetry/battery/tests.rs`, use the existing helper:

```rust
let (_fixture, reader) = battery(&[
    ("status", "Discharging"),
    ("capacity", "55"),
    ("energy_now", "33000000"),
    ("energy_full", "58000000"),
    ("energy_full_design", "60000000"),
]);
assert_eq!(reader.charge_pct(), Some(55.0));
assert_eq!(reader.energy().unwrap().now_wh, 33.0);
```

For battery discovery, create `power_supply/BAT0/type` containing `Battery` and
`power_supply/BAT0/scope` containing `System`. Add the required battery attributes
under `BAT0`. To simulate external power, create `power_supply/AC/type` containing
`Mains` and `power_supply/AC/online` containing `1` or `0`.

For hwmon, create `hwmon/hwmon0/name` containing `k10temp` and
`hwmon/hwmon0/temp1_input` containing `48500` to represent 48.5 degrees Celsius.
Follow existing thermal tests for Intel labels, GPU power, and NVMe sensors.

Check the reader's units: energy is generally micro-watt-hours, voltage
microvolts, power microwatts, and hwmon temperatures millidegrees Celsius.
Battery temperature attributes have their own conversion; follow the battery
reader's tests rather than assuming the hwmon scale.

Simulate missing or malformed readings by omitting/removing a fixture attribute
or writing invalid text. Test zero separately from absence. For read-error tests,
use a reproducible failure such as a directory in place of an attribute;
permission bits alone are unreliable when tests run as root.

For sequences, change fixture values between reads or feed samples through
`Scripted`/`WithGaps`. Use these to test charging transitions, hysteresis, missing
readings, and battery identity changes without changing the real hardware state.

## Test the executable without touching the live installation

Use the helpers in `tests/cli.rs`. `environment()` provides a temporary database
home and fake battery. `batmon()` selects Cargo's test-built executable and gives
only that child process the temporary home and battery path. Do not change the
agent shell's `HOME` or run Cargo with a replacement home.

The following is an example test body for that file:

```rust
let (sandbox_home, battery) = environment();
let fixtures = TempDir::new().unwrap();
let supplies = fixtures.path().join("power_supply");
let hwmon = fixtures.path().join("hwmon");
std::fs::create_dir_all(&supplies).unwrap();
std::fs::create_dir_all(&hwmon).unwrap();
std::fs::write(battery.path().join("capacity"), "55\n").unwrap();

let output = batmon(sandbox_home.path(), battery.path())
    .env("BATMON_POWER_SUPPLY_BASE", &supplies)
    .env("BATMON_HWMON_BASE", &hwmon)
    .env("DBUS_SYSTEM_BUS_ADDRESS", format!(
        "unix:path={}/no-system-bus", fixtures.path().display()
    ))
    .env("DBUS_SESSION_BUS_ADDRESS", format!(
        "unix:path={}/no-session-bus", fixtures.path().display()
    ))
    .arg("--oneshot")
    .output()
    .unwrap();

assert!(output.status.success(), "{}",
    String::from_utf8_lossy(&output.stderr));
let debug = database_dir(sandbox_home.path()).join("debug.db");
assert_eq!(query::<i64>(&debug, "SELECT count(*) FROM samples"), 1);
assert_eq!(query::<f64>(&debug, "SELECT charge_pct FROM samples"), 55.0);
```

The nonexistent D-Bus addresses prevent this example from querying the laptop's
UPower or displaying desktop notifications. Expected bus-unavailable warnings
are acceptable here; successful recording and the database assertions are the
checks. For notification delivery tests, use a recording notifier or a private
test bus. Do not send simulated alerts to the user's desktop.

For daemon process tests, use the same isolated command without `--oneshot`.
Give startup/polling a bounded deadline, signal only the child PID you created,
and always terminate and reap it, including on assertion failures. Verify rows
and shutdown behavior in its temporary databases. Do not use broad `pkill` or
`killall` commands.

### Isolation limits

- `BATMON_SYSFS_PATH` overrides the battery directory.
- `BATMON_POWER_SUPPLY_BASE` overrides battery discovery and external-power reads.
- `BATMON_HWMON_BASE` overrides the hwmon tree.
- The executable still reads real `/proc`, CPU sysfs, and DRM sysfs. Those roots
  have no environment overrides. For deterministic tests, construct `Paths` with
  fixture roots in library tests; do not invent unsupported environment options.
- Database and lock paths resolve under `~/.local/share/batmon`. There is no
  `BATMON_DATA_DIR` option, and `XDG_DATA_HOME` does not redirect them.
- `--oneshot` writes samples, applies migrations, and evaluates desktop alerts.
  It bypasses the daemon lock. Never run a development one-shot with the real
  database home just because the installed service is already running.
- This is test-data isolation, not an operating-system security sandbox. These
  fixtures cannot prove real suspend/resume or hardware-driver behavior.

Do not run `install.sh`, `uninstall.sh`, replace `~/.local/bin/batmon`, or stop,
restart, or reconfigure the installed service as part of routine testing. Do not
write to real `/sys` or `/proc`, manipulate the laptop's battery/thermal settings,
or start a development server. Live installation and hardware exercises require
the user's explicit authorization.

Test migrations on synthetic historical schemas first. If a real-data case is
authorized, make a consistent SQLite backup and migrate only that disposable
copy; copying an active `.db` file alone can omit committed WAL contents.

## Finishing an approved task

Update documentation when documented behavior changes. Stage explicit paths,
inspect `git diff --cached --stat` and the staged diff, and commit only the
approved task. Never use sweeping staging commands. Follow the repository's
Conventional Commit style and explain the reason for the change. Include:

```text
Generated with Codex
Co-authored-by: GPT <noreply@openai.com>
```
