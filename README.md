# batmon

**Battery health monitor & hardware flight recorder for Linux laptops.**

`batmon` is a zero-dependency, low-overhead background daemon that continuously records power, thermal, and system telemetry to local SQLite databases.

It operates as a high-frequency flight recorder, capturing hardware metrics every second to preserve the exact state of the machine in the event of a crash or kernel panic, while simultaneously maintaining a permanent, downsampled historical log for tracking long-term component wear and battery degradation.

---

## ⚡ Quick Install

```bash
curl -fsSL https://raw.githubusercontent.com/InvictusNavarchus/batmon/master/install.sh | bash
```

The installer automatically detects your architecture (`x86_64` or `aarch64`), downloads the pre-compiled static binary to `~/.local/bin/batmon`, configures and starts the `systemd` user service, and verifies the installation.

---

## 🎯 Architecture: Dual-Tier Monitoring

`batmon` captures telemetry using two distinct tiers:

```text
                      ┌───────────────────────────────────────────────┐
                      │            batmon Daemon (Rust)               │
                      └───────┬───────────────────────────────┬───────┘
                              │ (Every 1 sec)                 │ (Every 60 sec)
                              ▼                               ▼
               ┌──────────────────────────────┐ ┌──────────────────────────────┐
               │    debug.db (Flight Log)     │ │  battery.db (Historical DB)  │
               ├──────────────────────────────┤ ├──────────────────────────────┤
               │ • 1s sample resolution       │ │ • 60s sample resolution      │
               │ • SQLite WAL + sync=NORMAL   │ │ • SQLite WAL + sync=NORMAL   │
               │ • Keeps last 6h of recording │ │ • Permanent wear records     │
               │ • Crash & panic forensics    │ │ • Cycle count & degradation  │
               └──────────────────────────────┘ └──────────────────────────────┘
```

1. **High-Frequency Flight Recorder (`debug.db`):**  
   Records every 1 second directly to SQLite using WAL mode (`PRAGMA synchronous = NORMAL`). Commits are written to the Linux kernel page cache via `write()` without invoking an `fsync()` on each tick, consuming negligible disk I/O and power (<15 mW). Data is immediately queryable across processes. Disk durability operates on a best-effort basis via SQLite's automatic WAL checkpoints (`wal_autocheckpoint = 100` pages) and standard Linux dirty page writeback—preventing database corruption during crashes while trading immediate per-second fsync persistence for drive longevity. Keeps the most recent 6 hours of *recording* (21,600 rows at 1 Hz), pruning by row count rather than wall-clock age: time spent powered off, suspended, or with the battery unreadable does not count against the window, so the run-up to a crash is still there however long the machine stays off afterwards.

2. **Long-Term Historical Telemetry (`battery.db`):**  
   Records downsampled samples every 60 seconds. Tracks long-term battery degradation, design wear capacity, and software-integrated cycle count over months and years.

---

## 📊 What It Logs

| Category | Metric | Source | Description |
| :--- | :--- | :--- | :--- |
| **Electrical & Power** | `power_state` | sysfs / state machine | Rail power status (`charging`, `discharging`, `ac_idle`, `unknown`) |
| | `voltage_v` | sysfs (battery) | Instantaneous battery rail voltage (V) |
| | `power_w` | sysfs (battery) | Discharge / charge rate (Watts) |
| | `charge_pct` | sysfs (battery) | Current state of charge (%) |
| | `energy_wh` | sysfs (battery) | Remaining energy (Wh) |
| | `energy_full_wh` | sysfs (battery) | Current full charge capacity (Wh) |
| | `energy_design_wh` | sysfs (battery) | Factory nominal design capacity (Wh) |
| | `voltage_design_v` | sysfs (battery) | Factory design voltage (V) |
| | `is_charging` | sysfs (battery) | Charge state boolean |
| **Thermal Environment** | `cpu_temp_c` | sysfs (`hwmon`) | CPU package / core temperature (e.g. AMD Tctl / Intel Package id) (°C) |
| | `gpu_temp_c` | sysfs (`hwmon`) | GPU temperature (e.g. AMD edge / Intel package) (°C) |
| | `nvme_temp_c` | sysfs (`hwmon`) | NVMe composite temperature (°C) |
| | `battery_temp_c` | sysfs (battery / `hwmon`) | Battery sensor temperature (if present) |
| **Clock & SoC Power** | `cpu_freq_mhz` | sysfs (`cpufreq`) / `/proc` | Instantaneous CPU clock frequency (MHz) |
| | `gpu_power_w` | sysfs (`hwmon`) | AMD APU / GPU package power (PPT via amdgpu) (Watts) |
| | `gpu_pct` | sysfs (DRM) | GPU compute / shader utilization (%) |
| **System Load & Host** | `cpu_pct` | `/proc/stat` | Global CPU utilization (%) |
| | `mem_pct` | `/proc/meminfo` | Global Memory utilization (%) |
| | `load1` | `/proc/loadavg` | 1-minute system load average |
| | `boot_id` | `/proc/sys/kernel/random/boot_id` | Linux kernel boot session UUID |
| | `uptime_s` | `/proc/uptime` | Monotonic system uptime (seconds) |
| | `top_processes` | `/proc/[pid]/stat` | Top 5 aggregated process groups by 1s CPU delta (JSON) |
| **Health & Wear** | `health_pct` | sysfs | Full charge capacity vs design capacity (%) |
| | `cycle_count` | sysfs | Hardware cycle count (if reported by BMS) |
| | `estimated_cycle_count` | Integrator | Calculated cycle count via energy throughput ($\Delta\text{Wh} / \text{Design}$) |
| **Runtime Estimates** | `time_to_empty_s` | UPower D-Bus | Smoothed discharge runtime estimate (seconds) |
| | `time_to_full_s` | UPower D-Bus | Smoothed charge completion estimate (seconds) |

* Auto-detects `energy_*` (µWh) vs `charge_*` (µAh) battery drivers.
* **Low-Overhead Native Reads:** All CPU, memory, clock, GPU, thermal, and process metrics are gathered directly via Linux kernel VFS interfaces (`/proc` and `/sys`) and standard POSIX process accounting (~12 ms per sample cycle) with **zero child processes**. UPower and desktop notifications are spoken to over D-Bus directly rather than by spawning `busctl` and `notify-send`. Resident memory is ~6.6 MB (median over a 12-hour dual-daemon run spanning a reboot). See empirical evaluations on [Kernel VFS vs. Glances](docs/empirical-glances-vs-native-comparison.md), [Sysfs Hwmon vs. lm-sensors](docs/empirical-lm-sensors-vs-native-hwmon.md), and [TypeScript vs. Rust parity](docs/empirical-typescript-vs-rust-parity.md) for detailed benchmark results.
* **Automatic Migrations:** Database schema updates and column additions are handled seamlessly and automatically on startup using SQLite's native `user_version` tracking with zero manual migration steps required.

---

## 🔍 Post-Mortem Forensics & SQL Recipes

### 1. Inspect the last 30 seconds before a crash
Run after rebooting from the crash. By then the daemon is already recording the new boot, so the newest rows are not the crash; this skips everything from the current boot and shows how the previous one ended.
```bash
sqlite3 ~/.local/share/batmon/debug.db "
SELECT ts, power_w, voltage_v, cpu_freq_mhz, cpu_temp_c, gpu_power_w, cpu_pct, top_processes
FROM samples
WHERE boot_id IS NOT '$(cat /proc/sys/kernel/random/boot_id)'
ORDER BY id DESC
LIMIT 30;"
```

### 2. Check long-term battery degradation & wear
```bash
sqlite3 ~/.local/share/batmon/battery.db "
SELECT ts, charge_pct, health_pct, cycle_count, estimated_cycle_count, energy_full_wh, energy_design_wh
FROM samples
ORDER BY id DESC
LIMIT 10;"
```

### 3. Identify top power-hog process groups
```bash
sqlite3 ~/.local/share/batmon/debug.db "
SELECT ts, power_w, cpu_temp_c, top_processes
FROM samples
WHERE power_w > 30.0
ORDER BY id DESC
LIMIT 5;"
```

---

## 🔔 Desktop Notifications & Alerts

`batmon` features a stateful alerting engine with deadband hysteresis, debouncing, and priority escalation to prevent notification storms from flapping sensors:
* **High Battery Temp Warning:** Alert when battery temp $\ge 45^\circ\text{C}$ (Critical at $50^\circ\text{C}$ with contextual cooling advice; re-arms below $42^\circ\text{C}$ / $47^\circ\text{C}$).
* **Charging While Hot (Heat-Soak):** Alert when charging while CPU $\ge 85^\circ\text{C}$ (re-arms below $80^\circ\text{C}$).
* **Charge Limits:** Reminders to unplug at $\ge 80\%$ (re-arms below $75\%$) and plug in at $\le 20\%$ (Critical at $\le 10\%$ suppresses normal low alert; re-arms above $25\%$).
* **Over-Voltage Charging:** Alert when charging voltage exceeds 15% above design voltage (re-arms at or below 10% above design voltage).
* **Battery Health Degradation:** Warning when full capacity drops below $80\%$ of factory design (re-arms above $82\%$).

Re-fired alerts **replace** their previous notification rather than stacking beside it, so a flapping sensor cannot bury the desktop even if it defeats the deadband.

> **Note:** The current alert rules focus on battery protection, because by the time the voltage or power really drops, the system will be shutting down anyway. The idea is to prevent these issues from happening in the first place, not to detect them after the fact. The flight recorder is there to capture the data in case something does happen.

---

## 🛠️ Requirements

- **Linux** with systemd (Fedora, Ubuntu, Debian, Arch, etc.)
- **[Rust](https://rustup.rs)** toolchain ($\ge 1.87$) (optional) — only required if building from source. The pre-compiled release binary is fully static with SQLite compiled in, linking nothing beyond standard system interfaces.
- **UPower** (optional) — supplies smoothed runtime estimates. Without it, `batmon` falls back to dividing remaining energy by present draw.
- **A notification server** (optional) — any desktop provides one. Without it, alerts are still written to the journal.
- **`sqlite3` CLI** (optional, for querying databases): `sudo dnf install sqlite`

---

## 🚀 Installation Options

### Build from Source

If you prefer compiling locally rather than downloading pre-compiled binaries:

```bash
git clone https://github.com/InvictusNavarchus/batmon.git
cd batmon
./install.sh --build
```

Upgrading from a Bun-based installation is handled automatically: the superseded
TypeScript sources are removed and **your existing databases are kept and continued**.

### Managing the Service

```bash
# Check service status
systemctl --user status batmon.service

# View live logs
journalctl --user -u batmon.service -f

# Run a one-off diagnostic sample
batmon --oneshot

# Raise log verbosity (standard tracing filter syntax)
BATMON_LOG=batmon=debug batmon
```

---

## 🧪 Development & Testing

```bash
# Run the full test suite (unit, property, and end-to-end binary tests)
cargo test

# Lints, denied in the pre-commit hook
cargo clippy --all-targets -- -D warnings

# Formatting
cargo fmt --all
```

### Where tests live

Tests are unit tests in the same crate, because most of them exercise private
items — a sibling module cannot see them, so the test module has to stay a
descendant of the module it tests.

**A test module of 100 lines or more moves into its own file**, declared as a
child module:

```rust
// src/telemetry/thermal.rs
#[cfg(test)]
mod tests;          // -> src/telemetry/thermal/tests.rs
```

`super` still resolves to the parent module, so imports are unaffected and
test paths are unchanged. Below 100 lines the tests stay inline at the bottom
of the file, where colocation is still worth more than the indirection; four
modules are in that category today, and all four are under 200 lines total.

When adding a test file, **check that the `mod` declaration exists**. A test
file that is never declared compiles, runs nothing, and leaves the suite green
at a lower count — the one failure mode here that no assertion catches.

### Blaming through the extraction

The test modules were extracted in `refactor/extract-test-modules`, which moved
4,884 lines without changing any test logic. Plain `git blame` on those files
reports only the move. To see who actually wrote a line:

```bash
git blame -w -C -C -C src/telemetry/thermal/tests.rs
```

`-w` is the part that matters: extraction dedented every line by one level, and
without it the copy detection in `-C` finds nothing. A `.git-blame-ignore-revs`
file does *not* help here — the content landed in new files, so ignoring the
extraction commit leaves blame with nothing earlier to attribute to.

---

## 🗑️ Uninstallation

```bash
./uninstall.sh
```
*(Databases in `~/.local/share/batmon/` are preserved upon uninstall).*

## Limitations

- Battery cell-level data (individual cell voltages, internal impedance, BMS balancing status) is not available through the Linux `power_supply` sysfs interface and cannot be collected.
- VRM rail voltages and transient events below ~1 s are not exposed by the kernel on most laptop hardware. The 1-second flight recorder can catch sustained voltage sag but not microsecond-scale transients.
- The `battery_temp_c` sensor is absent on many laptops. When unavailable, battery thermal protection relies on ambient correlation with CPU/GPU temperatures.
- `mem_pct` and `load1` are recorded for forensic completeness but are rarely primary indicators of hardware failure.