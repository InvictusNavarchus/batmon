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
* **Sample parity is exact where values are stable.** Across ~21,500 joined sample pairs
  spanning six hours, every static and categorical column matched bit-for-bit. Every
  column that diverged is one that genuinely changes between two instants sampled
  250 ms apart.
* **The cycle integral does not drift.** After 21,700 ticks and 1.335 accumulated
  cycles, the two implementations agree to `0.000000000000`.
* **Alerts are identical.** Both fired the same three alerts, in the same order, with
  byte-identical text, on the same real battery.
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

Both daemons were run concurrently against separate database directories, isolated by
`$HOME`, started in the same `systemctl` call so their rate-measurement windows stayed
in phase. The production daemon and its databases were not touched. Samples were joined
on nearest timestamp; the median join offset was 250 ms, which is the irreducible floor
for two independent processes on a one-second cadence.

Two runs are reported. A 180-second smoke run established column parity; a **six-hour
run** covering a full discharge, a charge to 80%, and a second discharge to 18%
established everything that requires time to accumulate.

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

Over ~21,500 joined pairs across six hours:

| Verdict | Columns |
| :--- | :--- |
| **Bit-identical** | `energy_design_wh`, `voltage_design_v`, `cycle_count`, `boot_id`, `health_pct`, `energy_full_wh`, `status`, `power_state`, `is_charging`, `is_present` |
| **Varies within sampling jitter** | `charge_pct` (max Δ 1), `load1`, `mem_pct`, `uptime_s` (max Δ 0.51 s), `energy_wh` (max Δ 0.104 Wh) |
| **Genuinely instantaneous** | `power_w`, `voltage_v`, `cpu_pct`, `cpu_freq_mhz`, `gpu_pct`, `gpu_power_w`, all temperatures, `top_processes` |
| **Externally sourced** | `time_to_empty_s`, `time_to_full_s` — UPower's own smoothed estimates, read verbatim by both |
| **No data on this hardware** | `battery_temp_c` — no pack sensor |

### 3.3 The cycle integral

The check a point-in-time comparison structurally cannot make. The count is accumulated
one tick at a time, so a per-tick divergence too small to see in one sample compounds.

```
rust final       1.335224935
typescript final 1.335224935
final gap        0.000000000000 cycles
```

Expressed in the only meaningful unit — one tick of discharge accrues ~6.0e-4 cycles —
the mean gap never exceeded **0.11 ticks** and the worst single excursion was **2.31
ticks**, which is what a 250 ms sampling offset produces. Several windows sat at
`2.220e-16`, one double-precision ulp. Critically the gap **oscillates and returns to
zero rather than accumulating**: if a per-tick divergence ε existed, the gap after
21,700 ticks would be ~21,700ε.

Both daemons were also restarted mid-run and each resumed at exactly `1.335224935`,
verifying the adopt-last-stored-sample path on real data.

### 3.4 Rail states and alerts

| | Result |
| :--- | :--- |
| Charging pairs | 9,714 — **0** `power_state` and **0** `is_charging` mismatches |
| Discharging pairs | 11,832 |
| `ac_idle` | **0 — not reached** |
| Rail transitions | 3 in each implementation, same order |
| Charge range | 18% → 74% → 80% → 20% |

Both fired exactly three alerts, in the same order, with byte-identical bodies:

```
Low Battery: 20% remaining – plug in charger
Battery Charge Target Reached: Level reached 80% – unplug charger to preserve health
Low Battery: 20% remaining – plug in charger
```

Three alerts across six hours spanning a full charge cycle is the hysteresis engine
working: no storms, and the 80% unplug reminder — previously exercised only by synthetic
tests — fired correctly on real hardware.

### 3.5 Cadence and retention

Both crossed the six-hour retention boundary and pruned correctly; their retained
windows differed by 114 s, which is the offset between their five-minute prune cycles.

The tick cadence difference is the deadline-scheduling claim, measured:

| | median tick | rows in 6.02 h |
| :--- | :--- | :--- |
| Rust (sleeps toward a deadline) | 1.0000 s | 21,570 |
| TypeScript (`setInterval`) | 1.0050 s | 21,457 |

**The TypeScript daemon recorded 113 fewer samples from the same window**, about
7.5 minutes of lost coverage per day. Thirteen tick overruns were logged by the Rust
daemon, all inside a single 90-second window — one system event, 0.06% of ticks — and
both implementations show gaps at the same moments.

### 3.6 Resource cost

| Metric | TypeScript (Bun) | Rust | Change |
| :--- | :--- | :--- | :--- |
| Resident memory | 23.9–50.9 MB | 5.7–8.9 MB | **4.2× less** (no growth over 6 h in either) |
| Threads | 17 | 6 | 2.8× fewer |
| Median sample cost | 20.3 ms | 12.4 ms | **1.6× faster** |
| Subprocess spawns | 2/min (`busctl`) | 0 | eliminated |
| Binary / runtime | Bun runtime + sources | 5.4 MB static binary | — |
| Samples recorded in 6 h | 21,457 | **21,570** | 113 more |

### 3.7 Where the speedup actually came from

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

The six-hour run covered a full discharge, a charge, and a second discharge. What it did
**not** reach is not a matter of running longer — each needs a specific condition:

* **`ac_idle`** — the battery never sat at Full or at a vendor charge limit. This is a
  three-line branch in the charge ladder with four unit tests.
* **A reboot boundary** — one boot session only, so the cycle integrator's carry-forward
  was exercised only by its unit and property tests. This is the gap that matters most,
  because getting carry-forward wrong silently corrupts the long-term number with no
  alert.
* **Heat-soak and thermal anomaly** — the CPU never reached 85 °C while charging or
  80 °C while idle. Both are covered by the shared debounce latch's own tests.
* **`battery_temp_c`** — null in all rows. This hardware has no pack sensor, so the
  battery thermal family cannot be differentially tested here at all, ever.

One artefact worth recording: three notifications failed with
`ExcessNotificationGeneration`. That is the notification server rate-limiting because
*three* daemons — production plus both differential instances — alerted on the same
battery simultaneously. The daemon logged it and continued, which is the designed
degradation, but it is an artefact of the experiment rather than a property of the port.
