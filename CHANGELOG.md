# Changelog

Notable changes to batmon, newest first. Each version's section is published
verbatim as its GitHub release notes, so write entries for someone upgrading,
not for someone reading the diff — lead with anything that changes behaviour.
How to write entries and cut a release: [docs/releasing.md](docs/releasing.md).

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Sections up to v0.8.2 were rewritten into this format from the original
release notes; v0.3.0 and earlier had none.

<!-- next-header -->

## [Unreleased] - ReleaseDate

## [0.8.2] - 2026-09-10

### Changed

- **Behaviour change:** `debug.db` now keeps the last 6 hours of *recording*
  rather than the last 6 hours of wall-clock time. Downtime (power-off,
  suspend, unreadable battery) no longer ages data out, so the run-up to a
  crash survives a long power-off — but rows, including `top_processes`, can
  now be older than 6 hours. (#15)

## [0.8.1] - 2026-09-07

### Fixed

- Unreadable or empty sysfs readings are treated as unknown instead of `0` or
  `false` (#14). Before this:
  - an unreadable `capacity` or `energy_full` could raise a false
    "CRITICAL: Battery Low" or "Battery health at 0.0%" alert;
  - an unreadable `energy_full_design` could reset the accumulated cycle count
    to zero, and an unreadable `energy_now` could add phantom cycles;
  - an empty hwmon file read as 0 °C and could clear an active thermal alert;
  - a transient unreadable `present` could clear active alert latches.
- Flight-recorder pruning could stop while the battery was unreadable or
  absent; it now runs on every tick.
- Unreadable sysfs values were logged every second; they are now logged once
  per episode.
- A reinserted battery could inherit a stale UPower time-to-empty estimate;
  estimates are dropped when readings stop.

## [0.8.0] - 2026-09-07

### Added

- One-line install: `install.sh` detects `x86_64` or `aarch64` and downloads a
  precompiled static musl binary, so installing no longer needs a Rust
  toolchain. Each release ships its binaries with SHA-256 checksums.
- Only one daemon runs at a time. A second instance finds the lock
  (`batmon.lock`), reports the PID of the running one, and exits with an
  error. (#12)

### Changed

- `install.sh` stops a running `batmon` service before replacing the binary,
  so an upgrade cannot race the daemon for the database.

## [0.7.0] - 2026-09-07

batmon is now a native Rust binary, replacing the TypeScript/Bun
implementation. The goal is the observer principle: measuring the system must
not meaningfully perturb it. Existing databases carry over unchanged.

**Upgrading:** re-run `./install.sh` (building needs a Rust toolchain). It
installs the binary to `~/.local/bin/batmon`, removes the old TypeScript
sources from `~/.local/share/batmon`, keeps both databases, and updates
`batmon.service`. Bun and Node are no longer needed.

### Changed

- Median memory drops from 28.4 MB to 6.6 MB, and threads from ~15 to 6.
- Ticks are deadline-scheduled, removing ~7.5 minutes a day of cumulative
  drift.
- hwmon sensor paths are cached after the first probe, with an automatic
  rescan if a device drops or a driver loads late (e.g. `k10temp`).
- Notifications replace the previous bubble of their kind instead of stacking.
- UPower reads time out after 2 s and fall back to internal arithmetic, so a
  stalled UPower no longer blocks sampling.
- If the notification daemon is not up yet at boot, alerts go to the journal
  and the connection is retried.

### Removed

- The Bun/Node runtime dependency, and the `busctl` and `notify-send`
  subprocess calls (~1,440 forks a day). batmon talks to D-Bus directly.

### Fixed

- Removing or swapping the battery could count the missing capacity as
  discharge; the cycle count now carries forward.
- NaN or infinite sensor readings could raise a false 0% critical alert or slip
  past threshold checks.

## [0.6.0] - 2026-09-06

**Upgrading:** databases migrate automatically on first start. If you run the
systemd service:

```bash
git pull
bun run install-service
systemctl --user restart batmon
```

### Added

- A thermal anomaly alert fires when the CPU stays above 75 °C while load and
  CPU usage are low — an early warning for a failing fan, dried thermal paste
  or a clogged heatsink.
- Every sample records the kernel `boot_id` and monotonic uptime, so history
  can be told apart across reboots.
- On Btrfs, `install.sh` marks the database directory No-CoW (`chattr +C`) to
  avoid write amplification, fragmentation and erratic WAL checkpoint latency,
  and warns if the filesystem refuses.
- A standalone binary build (`npm run build`, via `bun build --compile`) that
  runs without a system-wide Bun.
- `BATMON_POWER_SUPPLY_BASE` overrides the power-supply sysfs path.

### Changed

- **Behaviour change:** low and critical battery alerts fire only while
  discharging, never while plugged in. Power state is now `charging`,
  `discharging` or `unknown` instead of a boolean, with statuses such as
  `Not charging` and `Full` mapped explicitly.
- Alerts need 3 consecutive samples past a threshold before firing, filtering
  out momentary spikes and electrical noise.
- CPU temperature alerts no longer depend on whether the battery is charging.
- Cycle deltas are computed against the previous 1-second sample instead of
  the 60-second history, capturing short bursts.
- UPower queries are rate-limited so they cannot stall the 1-second loop.
- Flight-recorder pruning uses an indexed timestamp cutoff instead of a full
  table scan.

### Fixed

- The cycle counter could freeze at low power draw (below ~10.8 W on a 60 Wh
  battery), because intermediate deltas were rounded to 4 decimal places. Full
  precision is now kept.
- A reboot or clock adjustment could create a false cycle spike; a change of
  `boot_id` or a drop in uptime now resets the reference sample.
- An AC-only desktop or a disconnected battery could be logged as a phantom
  battery; presence now requires the sysfs battery directory to exist.
- One-shot `batmon` reported zero CPU and process activity; it now samples
  twice, 500 ms apart.
- Power-state rows written by older versions could be misread; they are mapped
  through the new three-state model.
- A migration could fail when the table it renames to already existed.

## [0.5.0] - 2026-08-31

**Upgrading:** `lm-sensors` is no longer needed.

### Added

- The battery is discovered under `/sys/class/power_supply` instead of being
  assumed to be `BAT0`, preferring internal batteries over peripherals such as
  wireless mice. Works with `BAT1`, `macsmc-battery`, `bat.0@aux:1` and other
  vendor names.

### Changed

- **Behaviour change:** alerts use hysteresis. Once fired, an alert re-arms
  only after the reading moves back past a deadband (charge %, battery and CPU
  temperature, battery health), ending notification storms around a
  threshold. Critical alerts suppress lower-severity duplicates.
- Sensors are read from `/sys/class/hwmon` directly (AMD, Intel, ARM, NVMe)
  instead of by running `sensors` — about 7.2× faster, with no subprocess. See
  the [benchmark](docs/empirical-lm-sensors-vs-native-hwmon.md).
- Overheat advice differs between charging and discharging.

### Removed

- The `lm-sensors` dependency.

### Fixed

- The alert check now runs every second, as intended.
- The high-charge alert could re-arm on a `Full` status or a brief AC toggle;
  it now re-arms only after discharging below 75 %.
- UPower lookups failed for batteries with `-`, `.`, `:` or `@` in their
  names; the D-Bus path is now sanitised the way UPower does it.
- Firmware reporting a zero or negative design voltage could cause a permanent
  over-voltage false alarm.
- On multi-GPU systems, GPU temperature and power could come from different
  devices; they are now always read from the same one.
- A GPU drawing 0 W (asleep in D3cold) and temperatures down to −50 °C were
  rejected as invalid readings.

## [0.4.0] - 2026-08-28

System metrics are now collected straight from the kernel instead of through
Glances.

**Upgrading:** Glances is no longer required; remove any Glances service or
install steps from your setup.

### Changed

- CPU, memory, CPU frequency, GPU load and top processes are read from
  `/proc` and sysfs with no subprocesses. Sampling latency drops from ~48 ms to
  ~13 ms. See the
  [benchmark](docs/empirical-glances-vs-native-comparison.md).
- Top-process CPU is measured tick over tick instead of from `ps`'s lifetime
  average, so short spikes in long-running processes show up in the flight
  recorder.

### Removed

- The Glances dependency.

### Fixed

- Battery temperature, health, charging over-voltage and CPU heat-soak alerts
  repeated every 60 s while the condition held; they now fire once, when the
  threshold is crossed.

<!-- next-url -->
[Unreleased]: https://github.com/InvictusNavarchus/batmon/compare/v0.8.2...HEAD
[0.8.2]: https://github.com/InvictusNavarchus/batmon/compare/v0.8.1...v0.8.2
[0.8.1]: https://github.com/InvictusNavarchus/batmon/compare/v0.8.0...v0.8.1
[0.8.0]: https://github.com/InvictusNavarchus/batmon/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/InvictusNavarchus/batmon/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/InvictusNavarchus/batmon/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/InvictusNavarchus/batmon/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/InvictusNavarchus/batmon/compare/v0.3.0...v0.4.0
