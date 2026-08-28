# batmon

**Battery health monitor & high-frequency hardware flight recorder for Linux laptops.**

`batmon` is a zero-dependency, low-overhead background monitor designed to diagnose and prevent battery thermal runaway, lithium-ion swelling, and power delivery (VRM / transient voltage sag) failures before they lead to kernel panics or permanent hardware damage.

---

## 🎯 Architecture: Dual-Tier Monitoring

`batmon` captures telemetry using two distinct tiers:

```text
                      ┌───────────────────────────────────────────────┐
                      │             batmon Daemon (Bun)               │
                      └───────┬───────────────────────────────┬───────┘
                              │ (Every 1 sec)                 │ (Every 60 sec)
                              ▼                               ▼
               ┌──────────────────────────────┐ ┌──────────────────────────────┐
               │    debug.db (Flight Log)     │ │  battery.db (Historical DB)  │
               ├──────────────────────────────┤ ├──────────────────────────────┤
               │ • 1s sample resolution       │ │ • 60s sample resolution      │
               │ • SQLite WAL + sync=NORMAL   │ │ • SQLite WAL + sync=NORMAL   │
               │ • Auto-pruned (last 6 hours) │ │ • Permanent wear records     │
               │ • Crash & panic forensics    │ │ • Cycle count & degradation  │
               └──────────────────────────────┘ └──────────────────────────────┘
```

1. **High-Frequency Flight Recorder (`debug.db`):**  
   Records every 1 second directly to SQLite using WAL mode (`PRAGMA synchronous = NORMAL`). Coalesced by the Linux kernel page cache, it consumes negligible power (<15 mW) while ensuring that during hard lockups, thermal throttling, or kernel panics, the crucial minutes leading up to the failure are safely preserved on disk for post-mortem forensics (with at most ~2–5s uncommitted in kernel page cache during sudden hard power cuts). Auto-prunes older records on a rolling window (default: 6 hours).

2. **Long-Term Historical Telemetry (`battery.db`):**  
   Records downsampled samples every 60 seconds. Tracks long-term battery degradation, design wear capacity, and software-integrated cycle count over months and years.

---

## 📊 What It Logs

| Category | Metric | Source | Description |
| :--- | :--- | :--- | :--- |
| **Electrical & Power** | `voltage_v` | sysfs (`BAT0`) | Instantaneous battery rail voltage (V) |
| | `power_w` | sysfs (`BAT0`) | Discharge / charge rate (Watts) |
| | `charge_pct` | sysfs (`BAT0`) | Current state of charge (%) |
| | `energy_wh` | sysfs (`BAT0`) | Remaining energy (Wh) |
| | `energy_full_wh` | sysfs (`BAT0`) | Current full charge capacity (Wh) |
| | `energy_design_wh` | sysfs (`BAT0`) | Factory nominal design capacity (Wh) |
| | `voltage_design_v` | sysfs (`BAT0`) | Factory design voltage (V) |
| | `is_charging` | sysfs (`BAT0`) | Charge state boolean |
| **Thermal Environment** | `cpu_temp_c` | sysfs (`hwmon`) | CPU package / core temperature (e.g. AMD Tctl / Intel Package id) (°C) |
| | `gpu_temp_c` | sysfs (`hwmon`) | GPU temperature (e.g. AMD edge / Intel package) (°C) |
| | `nvme_temp_c` | sysfs (`hwmon`) | NVMe composite temperature (°C) |
| | `battery_temp_c` | sysfs (`BAT0` / `hwmon`) | Battery sensor temperature (if present) |
| **Clock & SoC Power** | `cpu_freq_mhz` | sysfs (`cpufreq`) / `/proc` | Instantaneous CPU clock frequency (MHz) |
| | `gpu_power_w` | sysfs (`hwmon`) | AMD APU / GPU package power (PPT via amdgpu) (Watts) |
| | `gpu_pct` | sysfs (DRM) | GPU compute / shader utilization (%) |
| **System Load** | `cpu_pct` | `/proc/stat` | Global CPU utilization (%) |
| | `mem_pct` | `/proc/meminfo` | Global Memory utilization (%) |
| | `load1` | `/proc/loadavg` | 1-minute system load average |
| | `top_processes` | `/proc/[pid]/stat` | Top 5 aggregated process groups by 1s CPU delta (JSON) |
| **Health & Wear** | `health_pct` | sysfs | Full charge capacity vs design capacity (%) |
| | `cycle_count` | sysfs | Hardware cycle count (if reported by BMS) |
| | `estimated_cycle_count` | Integrator | Calculated cycle count via energy throughput ($\Delta\text{Wh} / \text{Design}$) |
| **Runtime Estimates** | `time_to_empty_s` | UPower D-Bus | Smoothed discharge runtime estimate (seconds) |
| | `time_to_full_s` | UPower D-Bus | Smoothed charge completion estimate (seconds) |

* Auto-detects `energy_*` (µWh) vs `charge_*` (µAh) battery drivers.
* **Low-Overhead Native Reads:** All CPU, memory, clock, GPU, thermal, and process metrics are gathered directly via Linux kernel VFS interfaces (`/proc` and `/sys`) and standard POSIX process accounting (~5–8 ms execution per sample cycle) with zero child processes or external daemons. See empirical evaluations on [Kernel VFS vs. Glances](docs/empirical-glances-vs-native-comparison.md) and [Sysfs Hwmon vs. lm-sensors](docs/empirical-lm-sensors-vs-native-hwmon.md) for detailed benchmark results.
* **Automatic Migrations:** Database schema updates and column additions are handled seamlessly and automatically on startup using SQLite's native `user_version` tracking with zero manual migration steps required.

---

## 🔍 Post-Mortem Forensics & SQL Recipes

### 1. Inspect the last 30 seconds before a crash
```bash
sqlite3 ~/.local/share/batmon/debug.db "
SELECT ts, power_w, voltage_v, cpu_freq_mhz, cpu_temp_c, gpu_power_w, cpu_pct, top_processes
FROM samples
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

`batmon` evaluates thresholds on each historical cycle and dispatches desktop notifications via `notify-send`:
* **High Battery Temp Warning:** Alert when battery temp $\ge 45^\circ\text{C}$ (Critical at $50^\circ\text{C}$).
* **Charging While Hot:** Alert when charging while CPU $\ge 85^\circ\text{C}$ to prevent battery swelling.
* **Charge Limits:** Reminders to unplug at $\ge 80\%$ and plug in at $\le 20\%$ ($\le 10\%$ critical).
* **Battery Health Degradation:** Warning when full capacity drops below $80\%$ of factory design.

---

## 🛠️ Requirements

- **Linux** with systemd (Fedora, Ubuntu, Debian, Arch, etc.)
- **[Bun](https://bun.sh)** runtime ($\ge 1.3$)
- **`libnotify` / `notify-send`** (optional, for desktop notifications):
  ```bash
  sudo dnf install libnotify     # Fedora/RHEL
  sudo apt install libnotify-bin # Ubuntu/Debian
  ```
- **`sqlite3` CLI** (optional, for querying databases): `sudo dnf install sqlite`

---

## 🚀 Installation

```bash
git clone https://github.com/InvictusNavarchus/batmon.git
cd batmon
./install.sh
# or using bun:
bun run install-service
```

The installer will:
1. Copy the application to `~/.local/share/batmon/src/`.
2. Configure and start a `systemd` user service (`batmon.service`).
3. Run an initial test verification.

### Managing the Service

```bash
# Check service status
systemctl --user status batmon.service

# View live logs
journalctl --user -u batmon.service -f

# Run a one-off diagnostic sample
bun run src/index.ts --oneshot
```

---

## 🧪 Development & Testing

Run unit tests and typechecks using Bun:

```bash
# Run test suite
bun test

# Run typechecker
bun run typecheck
```

---

## 🗑️ Uninstallation

```bash
./uninstall.sh
# or using bun:
bun run uninstall-service
```
*(Databases in `~/.local/share/batmon/` are preserved upon uninstall).*