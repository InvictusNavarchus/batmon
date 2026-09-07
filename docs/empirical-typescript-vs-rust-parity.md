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
* **The cycle integral does not drift.** After 1.335 accumulated cycles at the six-hour
  checkpoint and 2.065 at the end of the run, the two implementations agree to
  `0.000000000000` at both.
* **The integral survives a reboot.** The run was carried through a real shutdown and
  restart. Each implementation resumed at exactly the value it stored before power was
  cut, and both recorded the same two `boot_id`s with zero mismatches.
* **Alerts are identical.** Both fired the same four alerts, in the same order, with
  byte-identical text, within one tick of each other, on the same real battery.
* **4.3× less resident memory.** Median 6.6 MB against 28.4 MB across 1,430 paired
  observations spanning the whole run, with 6 threads against 15.
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

A 180-second smoke run established column parity. Everything that requires time to
accumulate comes from a **single long run**, started `2026-09-06T09:29:50Z` and examined
at two checkpoints: at **six hours**, covering a full discharge, a charge to 80% and a
second discharge to 18%; and at the **end of the run**, `2026-09-07T02:47:22Z`, after
the machine had been shut down and rebooted. Sections 3.1–3.7 report the six-hour
checkpoint; section 3.8 reports what the remainder of the run added.

The end state spans 17.3 h of wall time but **11.9 h of sampling** — 9.4 h in the first
boot session and 2.5 h in the second, with the machine off for 5 h 26 m in between.

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
daemon, all inside a single 114-second window — one system event, 0.06% of ticks — and
both implementations show gaps at the same moments.

### 3.6 Resource cost

All memory and thread figures below come from the six-hour run, sampled every
30 seconds by a third unit — 897 observations per implementation. Medians are
quoted rather than instantaneous readings, and neither implementation's
first-quarter and last-quarter means differ meaningfully, so neither grows over
the course of a day.

| Metric | TypeScript (Bun) | Rust | Change |
| :--- | :--- | :--- | :--- |
| Resident memory (median) | 26.7 MB | 6.3 MB | **4.3× less** |
| Resident memory (peak) | 50.9 MB | 8.9 MB | 5.7× less |
| Threads (median) | 15 | 6 | 2.5× fewer |
| Median sample cost | 20.3 ms | 12.4 ms | **1.6× faster** |
| Subprocess spawns | 2/min (`busctl`) | 0 | eliminated |
| Binary / runtime | Bun runtime + sources | 5.2 MB binary, SQLite compiled in, links only libc/libm/libgcc | — |
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

### 3.8 The reboot boundary

The six-hour checkpoint named one gap as the one that mattered most: the cycle
integrator's carry-forward had been exercised only by unit and property tests, because
the run had covered a single boot session. Getting carry-forward wrong silently
corrupts the long-term number with no alert, so the run was left in place and carried
through a real shutdown and reboot.

Two boot sessions were recorded, `7653dc99…` and `be846abc…`, identically by both
implementations. Comparing the last row of the first session against the first row of
the second:

| | last row before shutdown | first row after reboot |
| :--- | :--- | :--- |
| Rust | `1.32829858729941` | `1.32829858729941` |
| TypeScript | `1.32473254697572` | `1.32473254697572` |

**Both resumed at exactly the value they had stored**, and the cycle integral finished
the run where the six-hour checkpoint left it — identical:

```
rust final       2.065114525
typescript final 2.065114525
final gap        0.000000000000 cycles
```

The 0.0036 offset between the two columns above is not divergence. Their last
pre-shutdown rows are 23 s apart, and 23 s of discharge accrues 0.009 cycles, so the
offset is well *inside* what the sampling gap alone explains. Over the
post-reboot window the mean gap never exceeded 0.12 ticks of accrual and the worst
excursion was 2.52 ticks, with the first- and last-quarter means at 2.3e-05 and 3.8e-05
— bounded, not accumulating.

`ac_idle` — the other gap named at six hours — was also reached. The battery charged to
100% and sat at Full for 97 rows, and both implementations made the transition between
the same pair of minutes:

```
rust  17:12:48 Charging/charging 99%  ->  17:13:48 Full/ac_idle 100%
ts    17:12:07 Charging/charging 99%  ->  17:13:07 Full/ac_idle 100%
```

Over the full run the rail-state census was `discharging` 356/352, `charging` 265/264,
`ac_idle` 97/98 (rust/TypeScript), the ±1 differences being which side of a transition
each implementation's downsample instant fell on.

Four alerts fired in total, in the same order, with byte-identical bodies, within one
tick (local time, UTC+07):

| Alert | Rust | TypeScript |
| :--- | :--- | :--- |
| Low Battery: 20% remaining | 18:08:50 | 18:08:49 |
| Battery Charge Target Reached: 80% | 19:01:43 | 19:01:42 |
| Low Battery: 20% remaining | 21:55:39 | 21:55:39 |
| Battery Charge Target Reached: 80% | 22:41:57 | 22:41:57 |

Neither implementation panicked, logged an error, or was restarted by systemd after a
failure — `NRestarts` was 0 for both across all 1,430 observations. The daemons were
stopped and started cleanly twice: once to redeploy the harness mid-run, and once by the
system shutdown. **Both times the Rust daemon logged a clean `batmon stopped`**, so
SIGTERM at system poweroff is handled rather than the process being killed. The Rust
daemon logged 14 tick overruns in 11.9 h of sampling, 13 of them inside the single
114-second window under load already noted in §3.5.

Resource use over the whole run, 1,430 paired observations sampled every 30 s:

| Metric | TypeScript (Bun) | Rust | Change |
| :--- | :--- | :--- | :--- |
| Resident memory (median) | 28.4 MB | 6.6 MB | **4.3× less** |
| Resident memory (peak) | 57.8 MB | 9.5 MB | 6.1× less |
| Resident memory (range) | 13.6 – 57.8 MB | 4.6 – 9.5 MB | — |
| Threads (median) | 15 | 6 | 2.5× fewer |

Neither implementation leaks. The TypeScript figure looks like growth inside any single
window — it rose 32 MB to 43 MB across the post-reboot session — but per hour across the
whole run it is a garbage-collection sawtooth that trends *down*: 43.5, 33.7, 28.2,
24.8, 23.0, 23.9, 29.4, 28.3, 25.5, 23.4 MB. The Rust figure plateaus, moving 0.02 MB
over the final hour.

---

## 4. Limitations

The run covered three discharge legs, two charges past the 80% target, a charge all the
way to 100%, a spell at Full, and a reboot — an 18%–100% span in all.
Two of the four gaps named at the six-hour checkpoint were closed by letting it run;
the rest are not a matter of running longer, because each needs a specific condition
this hardware or this experiment cannot produce.

**Closed by the full run:**

* ~~**A reboot boundary**~~ — closed. Both implementations carried the cycle integral
  across a real shutdown exactly, and agreed on `boot_id` (§3.8).
* ~~**`ac_idle`**~~ — closed. The battery reached Full and both classified the
  transition in the same minute (§3.8).

**Still open:**

* **Heat-soak and thermal anomaly** — the CPU never reached 85 °C while charging or
  80 °C while idle. Both are covered by the shared debounce latch's own tests, but
  neither has been observed differentially.
* **`battery_temp_c`** — null in every row of both runs. This hardware has no pack
  sensor, so the battery thermal family cannot be differentially tested here at all,
  ever.
* **`time_to_full_s`** — null in every row. UPower never produced a full-time estimate
  on this hardware, so the column is read verbatim by both and compared only as "both
  null".
* **One machine.** Everything here validates a single battery, kernel, driver set and
  hwmon topology. It says nothing about a different sensor layout, a charge-reporting
  rather than energy-reporting battery, or a second battery.

**A methodological limit worth recording**, because it will bite anyone repeating this:
the flight recorder prunes to a six-hour window, so a long unattended run **cannot
preserve fine-grained evidence of its own beginning**. By the end of this run the
charging portion had been pruned, and the 1-second-cadence comparison at a 253 ms join
offset covered the post-reboot discharge only. Charging parity at that resolution rests
on the six-hour checkpoint's 9,714 charging pairs (§3.4); across the full run it rests
on the one-minute history store, where the only two apparent mismatches sit exactly on
rail transitions at a 13 s join offset and resolve to identical behaviour on inspection.
Read the flight recorder at checkpoints, or raise its retention, rather than reading it
once at the end.

One artefact worth recording: every alert notification failed with
`ExcessNotificationGeneration`. That is the notification server rate-limiting, because
two daemons alerted on the same battery within a second of each other. It is an artefact
of the experiment, not a property of the port. It appears one-sided only because the
TypeScript implementation spawns `notify-send` with `stderr: "ignore"` and discards the
exit status, so it hits the same limit silently; the Rust daemon logs the failure and
continues, which is the designed degradation.
