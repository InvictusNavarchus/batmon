# Empirical Evaluation: TypeScript vs. Rust Implementation Parity

## 1. Executive Summary & Objective

`batmon` was reimplemented in Rust. This document records the parity evidence gathered
before the TypeScript implementation was removed, and — just as importantly — states
precisely what that evidence does **not** cover.

### Core Conclusion

**The Rust implementation is behaviourally equivalent on every measurement that can be
compared, and materially cheaper to run:**

* **Schema parity is exact.** Both migration ladders transform seven distinct starting
  states into byte-identical schemas, compared on column names, declared types,
  `NOT NULL`, defaults, primary-key ordinals, indexes and `user_version`.
* **Sample parity is exact where values are stable.** Across 185 joined sample pairs,
  14 of 30 columns matched bit-for-bit. Every column that diverged is one that genuinely
  changes between two instants sampled 185 ms apart.
* **5.9× less resident memory.** 7.6 MB against 45.3 MB, with 6 threads against 17.
* **~40% cheaper per sample.** 12.4 ms against 20.3 ms median.
* **Two subprocess spawns per minute eliminated**, roughly 1,440 process creations a day.

---

## 2. Test Environment & Methodology

* **CPU:** AMD Ryzen (8 cores / 16 threads), `k10temp`
* **GPU:** AMD integrated APU, `amdgpu`, reporting package power
* **Battery:** `BAT0`, energy-reporting driver, 58.3 Wh design capacity, no pack
  temperature sensor
* **OS:** Linux 7.1.13 (Fedora, systemd user session)
* **Toolchains:** Bun 1.4.2, rustc 1.96.0 (release profile, thin LTO)

Both daemons were run concurrently for 180 seconds against separate database
directories, isolated by `$HOME`. The production daemon and its databases were not
touched. Samples were joined on nearest timestamp; the median join offset was 185 ms,
which is the irreducible floor for two independent processes on a one-second cadence.

---

## 3. Results

### 3.1 Schema parity

Seven starting states were migrated through both ladders and the resulting schemas
compared in full: a fresh database, a database resuming from version 2, the version 5
column standardisation, both branches of the `samples_debug` rename (empty destination
and populated destination), and a database already at the head of the ladder.

**All seven produced identical schemas.** A name-only comparison would have been
insufficient — a divergent type affinity is exactly how this class of bug escapes review
— so declared types, nullability, defaults and primary-key ordinals were compared too.

Separately, the Rust ladder was run against copies of the live field databases
(19,176 historical and 21,711 flight-recorder rows). Both opened at their existing
versions, the ladder was a no-op, and the schema hash was unchanged before and after.

### 3.2 Column-by-column sample parity

| Verdict | Columns |
| :--- | :--- |
| **Bit-identical** (185/185 pairs) | `charge_pct`, `status`, `power_state`, `energy_full_wh`, `energy_design_wh`, `power_w`, `voltage_v`, `voltage_design_v`, `cycle_count`, `health_pct`, `is_charging`, `is_present`, `load1`, `boot_id` |
| **Varies within sampling jitter** | `energy_wh` (max Δ 0.023 Wh, 0.05%), `estimated_cycle_count` (max Δ 0.0004), `uptime_s` (max Δ 0.32 s), `mem_pct` (max Δ 0.9), `cpu_temp_c` (max Δ 0.6 °C), `nvme_temp_c` (max Δ 1 °C) |
| **Genuinely instantaneous** | `cpu_pct`, `cpu_freq_mhz`, `gpu_pct`, `gpu_power_w`, `gpu_temp_c`, `top_processes` |
| **Externally sourced** | `time_to_empty_s` — UPower's own smoothed estimate, which both implementations read verbatim and which updates on its own schedule |
| **No data on this hardware** | `battery_temp_c` (no pack sensor), `time_to_full_s` (never charged during the window) |

`power_w` and `voltage_v` matching exactly across all 185 pairs is not a coincidence:
the kernel refreshes fuel-gauge attributes more slowly than 1 Hz, so consecutive reads
return the same driver-cached value.

`top_processes` differed in 183 of 185 pairs. This is expected and not a defect: the
ranking is computed from a one-second CPU delta measured over a different second, and
ties among idle processes resolve by `/proc` enumeration order in both implementations.
The **format** was verified separately as byte-identical, including that whole numbers
render as `6` rather than `6.0`.

### 3.3 Resource cost

| Metric | TypeScript (Bun) | Rust | Change |
| :--- | :--- | :--- | :--- |
| Resident memory | 45.3 MB | 7.6 MB | **5.9× less** |
| Threads | 17 | 6 | 2.8× fewer |
| Median sample cost | 20.3 ms | 12.4 ms | **1.6× faster** |
| Subprocess spawns | 2/min (`busctl`) | 0 | eliminated |
| Binary / runtime | Bun runtime + sources | 5.4 MB static binary | — |
| Tick interval deviation | not measured | ≤ 2 ms | drift-free |

### 3.4 Where the speedup actually came from

Decomposing the per-sample cost corrected an expectation set during planning, which
had predicted the process scan would fall from ~13 ms to 2–4 ms:

| Stage | Cost | Note |
| :--- | :--- | :--- |
| Listing `/proc` | 0.35 ms | 490 processes |
| Listing + reading every `stat` | 7.28 ms | **kernel-side syscall floor** |
| Full scan including parse, group, rank, render | 8.08 ms | ~0.8 ms of user-space work |

**Ninety percent of the process scan is `open`/`read`/`close` that no implementation can
avoid.** The rewrite removed roughly 3 ms of per-process string allocation above that
floor, not the floor itself. The larger single win was elsewhere: caching hwmon sensor
paths, since numbering is fixed at driver initialisation, removed a **3.9 ms rescan from
every tick**.

---

## 4. Limitations

The differential run was **180 seconds, not 24 hours**. It ran entirely on battery, so
the following were exercised only by unit and property tests and have **never met real
hardware**:

* The charging branch of the charge ladder (the 80% unplug reminder)
* Heat-soak, which only evaluates while charging
* Over-voltage, which only evaluates while charging
* The rail transition that invalidates the cached UPower estimate
* The 6-hour prune boundary
* Suspend/resume, and the deadline-resynchronisation path it triggers
* A reboot boundary in the cycle integrator's carry-forward

`estimated_cycle_count` deserves particular note. It is an integral accumulated one tick
at a time — the field history runs from 0 to 46.83 — so a per-tick divergence too small
to see in a single comparison compounds. A 180-second window cannot detect that; a
multi-day run comparing the accumulated totals can.

`battery_temp_c` is null in all 19,361 historical rows on this hardware. The battery
thermal alert family cannot be differentially tested here at all, and its synthetic tests
are the only coverage it will ever have on this machine.

**Recommendation:** run both daemons against separate database directories for at least
one full charge/discharge cycle, including a suspend and a reboot, before treating the
port as fully validated. The `$HOME` redirection used here makes that safe to do
alongside the production daemon.
