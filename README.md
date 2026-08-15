# batmon

Battery health monitor for Linux laptops. Logs sysfs/UPower telemetry
to SQLite every 60 seconds via a systemd user timer.

Built after a lithium-ion battery swelled from heat damage and became
unchargeable. This tool watches the electrical and thermal signals that
precede that kind of failure.

## What it logs

| Metric | Source |
|---|---|
| Charge %, status, energy, power, voltage | sysfs (`/sys/class/power_supply/BAT0/`) |
| Time to empty / full (smoothed) | UPower via D-Bus (`busctl`) |
| Battery temperature | sysfs / hwmon (if sensor exists) |
| CPU, GPU, NVMe temperature | `lm_sensors` (thermal environment proxy) |
| Cycle count, health vs design capacity | sysfs |

Auto-detects `energy_*` (µWh) vs `charge_*` (µAh) batteries.

## Requirements

- Linux with systemd (tested on Fedora 44)
- [Bun](https://bun.sh) runtime (includes built-in SQLite)
- `lm_sensors` (optional, for system temps): `sudo dnf install lm_sensors`
- `sqlite3` CLI (optional, for inspecting the DB): `sudo dnf install sqlite`

## Install

```bash
git clone https://github.com/YOUR_USER/batmon.git
cd batmon
./install.sh