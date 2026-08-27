# batmon

**Battery health monitor & high-frequency hardware flight recorder for Linux laptops.**

`batmon` is a zero-dependency, low-overhead background monitor designed to diagnose and prevent battery thermal runaway, lithium-ion swelling, and power delivery (VRM / transient voltage sag) failures before they lead to kernel panics or permanent hardware damage.

---

## 🎯 Architecture: Dual-Tier Monitoring

`batmon` captures telemetry using two distinct tiers:

```
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
               │ • Survives hard panics       │ │ • Cycle count & degradation  │
               └──────────────────────────────┘ └──────────────────────────────┘
```

1. **High-Frequency Flight Recorder (`debug.db`):**  
   Records every 1 second directly to SQLite using WAL mode (`PRAGMA synchronous = NORMAL`). Coalesced by the Linux kernel page cache, it consumes negligible power (<15 mW) while ensuring that if a hard lockup, thermal trip, or kernel panic occurs, the crucial seconds and minutes leading up to the failure are safely preserved on disk for post-mortem forensics. Auto-prunes older records on a rolling window (default: 6 hours).

2. **Long-Term Historical Telemetry (`battery.db`):**  
   Records downsampled samples every 60 seconds. Tracks long-term battery degradation, design wear capacity, and software-integrated cycle count over months and years.

---

## 📊 What It Logs

| Category | Metric | Source | Description |
| :--- | :--- | :--- | :--- |
| **Electrical & Power** | `voltage_v` | sysfs (`BAT0`) | Instantaneous battery rail voltage (V) |
| | `power_w` | sysfs (`BAT0`) | Discharge / charge rate (Watts) |
| | `percentage` | sysfs (`BAT0`) | Current state of charge (%) |
| | `energy_wh` | sysfs (`BAT0`) | Remaining energy (Wh) |
| | `is_charging` | sysfs (`BAT0`) | Charge state boolean |
| **Thermal Environment** | `cpu_temp_c` | `lm_sensors` | CPU Package / Tctl temperature (°C) |
| | `gpu_temp_c` | `lm_sensors` | GPU edge temperature (°C) |
| | `nvme_temp_c` | `lm_sensors` | NVMe composite temperature (°C) |
| | `temperature_c` | sysfs / hwmon | Battery sensor temperature (if present) |
| **Clock & SoC Power** | `cpu_freq_mhz` | Glances / sysfs | Instantaneous CPU clock frequency (MHz) |
| | `gpu_power_w` | `lm_sensors` | AMD APU / SoC PPT package power (Watts) |
| | `gpu_pct` | Glances | GPU compute / shader utilization (%) |
| **System Load** | `cpu_pct` | Glances | Global CPU utilization (%) |
| | `mem_pct` | Glances | Global Memory utilization (%) |
| | `load1` | Glances / `/proc` | 1-minute system load average |
| | `top_processes` | Glances REST API | Top 5 aggregated process groups (JSON) |
| **Health & Wear** | `capacity_pct` | sysfs | Full charge capacity vs design capacity (%) |
| | `cycle_count` | sysfs | Hardware cycle count (if reported by BMS) |
| | `estimated_cycle_count` | Integrator | Calculated cycle count via energy throughput ($\Delta\text{Wh} / \text{Design}$) |
| **Runtime Estimates** | `time_to_empty_s` | UPower D-Bus | Smoothed discharge runtime estimate (seconds) |
| | `time_to_full_s` | UPower D-Bus | Smoothed charge completion estimate (seconds) |

* Auto-detects `energy_*` (µWh) vs `charge_*` (µAh) battery drivers.
* Resilient: If Glances is not running, `cpu_freq_mhz` and `load1` seamlessly fall back to native `/sys` and `/proc` reads, while optional metrics record `NULL` without throwing errors.

---

## 🔍 Post-Mortem Forensics & SQL Recipes

### 1. Inspect the last 30 seconds before a crash
```bash
sqlite3 ~/.local/share/batmon/debug.db "
SELECT ts, power_w, voltage_v, cpu_freq_mhz, cpu_temp_c, gpu_power_w, cpu_pct, top_processes
FROM samples_debug
ORDER BY id DESC
LIMIT 30;"
```

### 2. Check long-term battery degradation & wear
```bash
sqlite3 ~/.local/share/batmon/battery.db "
SELECT ts, percentage, capacity_pct, cycle_count, estimated_cycle_count, energy_full_wh
FROM samples
ORDER BY id DESC
LIMIT 10;"
```

### 3. Identify top power-hog process groups
```bash
sqlite3 ~/.local/share/batmon/debug.db "
SELECT ts, power_w, cpu_temp_c, top_processes
FROM samples_debug
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
- **`lm_sensors`** (recommended, for system thermals and APU power):
  ```bash
  sudo dnf install lm_sensors   # Fedora/RHEL
  sudo apt install lm-sensors   # Ubuntu/Debian
  ```
- **`glances`** (optional, for global CPU/RAM ad top process telemetry):
  ```bash
  pipx install glances
  # Start Glances API server:
  glances -w --disable-webui --cached-time 1 -t 1
  ```
- **`sqlite3` CLI** (optional, for querying databases): `sudo dnf install sqlite`

---

## 🚀 Installation

```bash
git clone https://github.com/YOUR_USER/batmon.git
cd batmon
./install.sh
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

### Uninstallation

```bash
./uninstall.sh
```
*(Databases in `~/.local/share/batmon/` are preserved upon uninstall).*