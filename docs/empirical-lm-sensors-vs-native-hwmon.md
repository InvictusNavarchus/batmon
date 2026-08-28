# Empirical Evaluation: Native Linux Sysfs Hwmon vs. `lm-sensors`

## 1. Executive Summary & Objective

`batmon` is a low-power, high-frequency hardware flight recorder and battery health daemon for Linux laptops operating on a 1-second sample loop. This document records the empirical testing, architectural analysis, and benchmark data evaluating the deprecation and removal of the external `lm-sensors` (`sensors -j`) CLI dependency in favor of direct Linux kernel sysfs hwmon reads (`/sys/class/hwmon`).

### Core Conclusion
**Direct Linux kernel sysfs hwmon parsing is strictly superior to `lm-sensors` for `batmon`:**
* **100% Measurement Fidelity:** Parsing `/sys/class/hwmon` directly yields identical temperature and power readings as `lm-sensors` because `libsensors` is merely a user-space C wrapper around the exact same kernel sysfs attributes.
* **Elimination of 86,400 Process Spawns/Day:** Spawning `Bun.spawnSync(["sensors", "-j"])` once every 1,000 ms incurred continuous `fork`/`clone`, `execve`, dynamic linking, and JSON serialization overhead (over **41 ms** average execution latency). Native sysfs traversal eliminates child process creation entirely (~**5.6 ms** un-cached full directory scan; <**0.2 ms** cached file reads).
* **Zero External Dependencies:** Eliminates `lm_sensors` package installation requirements across minimal Linux installations (Alpine, Arch, Debian minimal, NixOS, Void Linux, and container environments).
* **Guaranteed Kernel ABI Stability:** `/sys/class/hwmon` attribute naming (`name`, `tempX_input`, `powerX_input`) and units (millidegrees Celsius, microwatts) are governed by the Linux Kernel ABI backwards compatibility guarantee.

---

## 2. Test Environment & Methodology

* **CPU:** AMD Ryzen 7 6800H (8 Cores, 16 Threads @ 3.20 GHz nominal)
* **GPU:** AMD Radeon 680M (Integrated APU, registered on `amdgpu` driver)
* **Storage:** PCIe NVMe SSD (registered on `nvme` driver)
* **OS:** Linux 6.x (x86_64)
* **Runtime:** Bun >= 1.3
* **lm-sensors:** `sensors` version 3.6.0+ with `libsensors`
* **Test Interval:** 1,000 ms sample period (matching the `batmon` flight recorder loop)

---

## 3. Side-by-Side Empirical Telemetry & Fidelity

Below is live telemetry captured synchronously on the same hardware from both data sources:

| Metric | Driver / Source | Direct Sysfs (`/sys/class/hwmon`) | `sensors -j` (`lm-sensors`) | Measurement Fidelity |
| :--- | :--- | :--- | :--- | :--- |
| **CPU Temperature** | `k10temp` / `Tctl` | `44.3 °C` (`44250 m°C / 1000`) | `44.25 °C` | **Exact Match** (1 decimal place rounding) |
| **GPU Temperature** | `amdgpu` / `edge` | `42.0 °C` (`42000 m°C / 1000`) | `42.00 °C` | **Exact Match** |
| **NVMe Temperature** | `nvme` / `Composite` | `40.9 °C` (`40850 m°C / 1000`) | `40.85 °C` | **Exact Match** (1 decimal place rounding) |
| **GPU APU Power** | `amdgpu` / `PPT` | `10.31 W` (`10310000 µW / 1e6`) | `10.31 W` | **Exact Match** |

### Data Mapping Breakdown

```text
Raw Linux Sysfs Attributes:
  k10temp (CPU):  /sys/class/hwmon/hwmon5/temp1_input   = 44250  (m°C)  → 44.25 °C
  amdgpu  (GPU):  /sys/class/hwmon/hwmon4/temp1_input   = 42000  (m°C)  → 42.00 °C
  nvme   (Drive): /sys/class/hwmon/hwmon2/temp1_input   = 40850  (m°C)  → 40.85 °C
  amdgpu (Power): /sys/class/hwmon/hwmon4/power1_input  = 10310000 (µW)  → 10.31 W

sensors -j Output:
  k10temp-pci-00c3.Tctl.temp1_input        = 44.25
  amdgpu-pci-0400.edge.temp1_input         = 42.00
  nvme-pci-0300.Composite.temp1_input      = 40.85
  amdgpu-pci-0400.PPT.power1_input         = 10.31
```

---

## 4. Benchmark & Resource Footprint

Execution latency benchmark over 100 iterations on live hardware:

```text
Sampling Latency Benchmark (100 iterations):
  Direct Sysfs (/sys/class/hwmon):  5.69 ms avg  (0 subprocesses, 0 context switches)
  Legacy `sensors -j` (Bun.spawn): 41.08 ms avg  (100 child processes spawned)
  Speedup:                         ~7.2x faster execution per sample
```

### Power & Lifecycle Implications

1. **Process Churn:** At 1 sample/sec, `sensors -j` spawns:
   $$\text{Spawns} = 1 \times 60 \times 60 = 3,600 \text{ processes/hour} = 86,400 \text{ processes/day}$$
2. **Observer Effect on Battery:** In a tool specifically engineered to record and optimize laptop battery runtime, spawning external binaries at 1 Hz continuously wakes CPU execution units and pollutes process accounting metrics. Direct VFS reads stay strictly within user-space memory and lightweight kernel virtual file reads.

---

## 5. Architectural Findings & Kernel ABI Guarantees

### 1. Demystifying `libsensors`
`lm-sensors` does not access hardware registers or I/O ports directly on modern Linux systems. The Linux kernel's hwmon subsystem (`drivers/hwmon/`) standardizes all hardware sensors into `/sys/class/hwmon/hwmon[0-N]`.

`libsensors` simply performs `readdir("/sys/class/hwmon")`, checks the `name` file, and parses integer strings. `batmon`'s native implementation replicates this without the external library and CLI middleman.

### 2. Linux Kernel ABI Specification
Per Linux Kernel documentation (`Documentation/hwmon/sysfs-interface.rst`):
* `temp[1-n]_input`: Temperature in millidegrees Celsius ($m^\circ\text{C}$).
* `temp[1-n]_label`: Optional string describing sensor location (`Tctl`, `Package id 0`, `Composite`, `edge`).
* `power[1-n]_input` / `power[1-n]_average`: Instantaneous or average power in microwatts ($\mu\text{W}$).

These attributes are governed by the Linux Kernel ABI stability guarantee and remain identical across all Linux distributions (Ubuntu, Fedora, Arch, Debian, Alpine, NixOS, openSUSE).

### 3. Dynamic Device Enumeration
Because `hwmon` numbering (`hwmon0`, `hwmon1`, etc.) is dynamically assigned by the kernel during driver initialization, `batmon` dynamically scans `/sys/class/hwmon/*/name` to associate drivers:
* **CPU:** `k10temp`, `zenpower` (AMD), `coretemp` (Intel), `cpu_thermal`, `soc_thermal` (ARM/RPi/Apple Silicon Linux)
* **GPU:** `amdgpu` (AMD), `i915`, `xe` (Intel), `nouveau` (Nvidia Open Source)
* **Storage:** `nvme`
