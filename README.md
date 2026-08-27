# batmon

Battery health monitor and high-frequency flight recorder for Linux laptops.
Logs 1-second flight telemetry to `debug.db` (auto-pruned rolling window) and 60-second
historical telemetry to `battery.db` (permanent wear records) via a systemd user service.

Built after a lithium-ion battery swelled from heat damage and became
unchargeable. This tool watches the electrical, process, and thermal signals that
precede that kind of failure.

## What it logs

| Metric | Source |
|---|---|
| Charge %, status, energy, power, voltage | sysfs (`/sys/class/power_supply/BAT0/`) |
| Time to empty / full (smoothed) | UPower via D-Bus (`busctl`) |
| Battery temperature | sysfs / hwmon (if sensor exists) |
| CPU, GPU, NVMe temperature | `lm_sensors` (thermal environment proxy) |
| CPU & Memory usage (global %) | Glances REST API (`/api/4/quicklook`) |
| Top 5 process groups (CPU %, Mem %, count) | Glances REST API (`/api/4/programlist`) |
| Cycle count, estimated cycles, health vs design capacity | sysfs & energy throughput integrator |

Auto-detects `energy_*` (µWh) vs `charge_*` (µAh) batteries.
Gracefully records `NULL` when Glances or specific thermal sensors are offline.

## Requirements

- Linux with systemd (tested on Fedora 44)
- [Bun](https://bun.sh) runtime (includes built-in SQLite)
- `lm_sensors` (optional, for system temps): `sudo dnf install lm_sensors`
- `glances` (optional, for CPU/RAM/process telemetry): `pipx install glances` or package manager
- `sqlite3` CLI (optional, for inspecting the DB): `sudo dnf install sqlite`

## Install

```bash
git clone https://github.com/YOUR_USER/batmon.git
cd batmon
./install.sh