#!/usr/bin/env python3
"""Compare the TypeScript and Rust daemons after a long differential run.

Reports on the things a short run cannot reach: whether the cycle integral
diverges as it accumulates, whether both implementations saw the same rail
states and thresholds, whether pruning agreed, and whether either grew over the
course of a day.

    legacy/tools/differential-report.py [--root DIR]
"""
from __future__ import annotations

import argparse
import datetime as dt
import sqlite3
import statistics
import sys
from pathlib import Path

# Columns whose value is fixed by hardware and must never differ.
STATIC = [
    "energy_design_wh", "voltage_design_v", "cycle_count", "boot_id",
]
# Columns that move slowly enough that a sub-second sampling offset should
# barely show.
SLOW = ["charge_pct", "health_pct", "energy_full_wh", "status", "power_state",
        "is_charging", "is_present", "load1", "mem_pct", "uptime_s"]
# Columns that genuinely change between two instants.
FAST = ["energy_wh", "power_w", "voltage_v", "cpu_temp_c", "gpu_temp_c",
        "nvme_temp_c", "cpu_pct", "cpu_freq_mhz", "gpu_pct", "gpu_power_w",
        "time_to_empty_s", "time_to_full_s", "battery_temp_c"]

RULE = "─" * 78


def parse(ts: str) -> dt.datetime:
    return dt.datetime.fromisoformat(ts.replace("Z", "+00:00"))


def rows(path: Path, table: str = "samples") -> list[dict]:
    if not path.exists():
        return []
    conn = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    conn.row_factory = sqlite3.Row
    try:
        return [dict(r) for r in conn.execute(f"SELECT * FROM {table} ORDER BY id")]
    finally:
        conn.close()


def header(title: str) -> None:
    print(f"\n{RULE}\n{title}\n{RULE}")


def duration(samples: list[dict]) -> str:
    if len(samples) < 2:
        return "n/a"
    span = parse(samples[-1]["ts"]) - parse(samples[0]["ts"])
    hours, rem = divmod(int(span.total_seconds()), 3600)
    return f"{hours}h {rem // 60}m"


def coverage(title: str, samples: list[dict]) -> None:
    """What the run actually exercised — the reason a long run exists at all."""
    print(f"\n{title}")
    if not samples:
        print("  (no data)")
        return

    states: dict[str, int] = {}
    for s in samples:
        states[s["power_state"] or "NULL"] = states.get(s["power_state"] or "NULL", 0) + 1
    print(f"  rail states     {', '.join(f'{k}={v}' for k, v in sorted(states.items()))}")

    boots = {s["boot_id"] for s in samples if s["boot_id"]}
    print(f"  distinct boots  {len(boots)}"
          f"{'  <- reboot boundary exercised' if len(boots) > 1 else ''}")

    charge = [s["charge_pct"] for s in samples if s["charge_pct"] is not None]
    if charge:
        print(f"  charge range    {min(charge):.0f}% .. {max(charge):.0f}%")

    cycles = [s["estimated_cycle_count"] for s in samples
              if s["estimated_cycle_count"] is not None]
    if cycles:
        print(f"  cycle integral  {cycles[0]:.6f} -> {cycles[-1]:.6f}"
              f"  (accrued {cycles[-1] - cycles[0]:.6f})")

    # Thresholds that would have engaged the alert engine.
    crossings = []
    if any(s["charge_pct"] is not None and s["charge_pct"] >= 80
           and s["is_charging"] for s in samples):
        crossings.append("charge>=80 while charging")
    if any(s["charge_pct"] is not None and s["charge_pct"] <= 20
           and s["power_state"] == "discharging" for s in samples):
        crossings.append("charge<=20 discharging")
    if any(s["cpu_temp_c"] is not None and s["cpu_temp_c"] >= 85
           and s["is_charging"] for s in samples):
        crossings.append("cpu>=85 while charging (heat-soak)")
    if any(s["cpu_temp_c"] is not None and s["cpu_temp_c"] >= 80
           and not s["is_charging"] for s in samples):
        crossings.append("cpu>=80 discharging (anomaly candidate)")
    print(f"  alert territory {', '.join(crossings) if crossings else 'none reached'}")


def join(rust: list[dict], ts: list[dict], tolerance: float = 0.6):
    """Pair each Rust sample with the nearest TypeScript one."""
    if not rust or not ts:
        return []
    ts_times = [parse(s["ts"]) for s in ts]
    pairs, cursor = [], 0
    for r in rust:
        moment = parse(r["ts"])
        while cursor + 1 < len(ts_times) and abs(
                (ts_times[cursor + 1] - moment).total_seconds()) <= abs(
                (ts_times[cursor] - moment).total_seconds()):
            cursor += 1
        offset = abs((ts_times[cursor] - moment).total_seconds())
        if offset <= tolerance:
            pairs.append((r, ts[cursor], offset))
    return pairs


def compare_columns(pairs) -> None:
    header("COLUMN PARITY")
    if not pairs:
        print("no comparable sample pairs")
        return
    print(f"{len(pairs)} pairs, median join offset "
          f"{statistics.median(p[2] for p in pairs) * 1000:.0f} ms\n")
    print(f"{'column':<22} {'class':<8} {'verdict':<12} detail")
    print("-" * 78)

    for column in STATIC + SLOW + FAST:
        kind = ("static" if column in STATIC
                else "slow" if column in SLOW else "fast")
        left = [p[0].get(column) for p in pairs]
        right = [p[1].get(column) for p in pairs]
        both = [(a, b) for a, b in zip(left, right)
                if a is not None and b is not None]

        if not both:
            nulls = sum(a is None for a in left), sum(b is None for b in right)
            state = "both null" if nulls == (len(left), len(right)) else "NULL SKEW"
            print(f"{column:<22} {kind:<8} {state:<12} "
                  f"rust {nulls[0]}/{len(left)} null, ts {nulls[1]}/{len(right)} null")
            continue

        if isinstance(both[0][0], str):
            bad = sum(a != b for a, b in both)
            verdict = "IDENTICAL" if bad == 0 else "DIFFERS"
            print(f"{column:<22} {kind:<8} {verdict:<12} {bad}/{len(both)} mismatched")
            continue

        deltas = [abs(a - b) for a, b in both]
        if max(deltas) == 0:
            print(f"{column:<22} {kind:<8} {'IDENTICAL':<12} all {len(both)} exact")
        else:
            flag = "  <-- REVIEW" if kind == "static" else ""
            print(f"{column:<22} {kind:<8} {'varies':<12} "
                  f"max|d|={max(deltas):.6g} median|d|={statistics.median(deltas):.6g}{flag}")


def compare_integral(pairs) -> None:
    """The question a short run cannot answer: does the integral drift?"""
    header("CYCLE INTEGRAL DIVERGENCE  (the headline check)")
    usable = [(p[0], p[1]) for p in pairs
              if p[0]["estimated_cycle_count"] is not None
              and p[1]["estimated_cycle_count"] is not None]
    if len(usable) < 10:
        print("not enough paired samples")
        return

    start = parse(usable[0][0]["ts"])
    series = [((parse(r["ts"]) - start).total_seconds() / 3600.0,
               r["estimated_cycle_count"] - t["estimated_cycle_count"])
              for r, t in usable]

    final_r = usable[-1][0]["estimated_cycle_count"]
    final_t = usable[-1][1]["estimated_cycle_count"]
    print(f"  rust final       {final_r:.9f}")
    print(f"  typescript final {final_t:.9f}")
    print(f"  absolute gap     {abs(final_r - final_t):.9f} cycles")
    if max(final_r, final_t) > 0:
        print(f"  relative gap     {abs(final_r - final_t) / max(final_r, final_t) * 100:.4f}%")

    # A systematic per-tick bug shows as a gap growing with elapsed time; noise
    # from sampling offset stays bounded.
    print("\n  gap over time (should stay flat, not climb):")
    buckets = max(1, len(series) // 8)
    for i in range(0, len(series), buckets):
        chunk = series[i:i + buckets]
        if chunk:
            print(f"    +{chunk[0][0]:5.2f}h   mean gap {statistics.fmean(g for _, g in chunk):+.9f}")

    hours = series[-1][0]
    if hours > 0.5:
        first = statistics.fmean(g for h, g in series if h < hours * 0.25)
        last = statistics.fmean(g for h, g in series if h > hours * 0.75)
        growth = abs(last) - abs(first)
        verdict = ("BOUNDED — no systematic drift" if abs(growth) < abs(first) + 1e-6
                   else "GROWING — investigate")
        print(f"\n  verdict: {verdict} (|gap| moved {growth:+.9f} between first and last quarter)")


def compare_retention(root: Path) -> None:
    header("RETENTION AND CADENCE")
    for name in ("rust", "ts"):
        debug = rows(root / name / ".local/share/batmon/debug.db")
        hist = rows(root / name / ".local/share/batmon/battery.db")
        if not debug:
            print(f"  {name:<5} no flight-recorder data")
            continue
        span = parse(debug[-1]["ts"]) - parse(debug[0]["ts"])
        window = span.total_seconds() / 3600
        gaps = [(parse(debug[i + 1]["ts"]) - parse(debug[i]["ts"])).total_seconds()
                for i in range(len(debug) - 1)]
        big = [g for g in gaps if g > 5]
        print(f"  {name:<5} debug {len(debug):>6} rows spanning {window:5.2f}h "
              f"(retention is 6h) | history {len(hist):>5} rows "
              f"| median tick {statistics.median(gaps) if gaps else 0:.3f}s "
              f"| gaps>5s: {len(big)}")
        if big:
            print(f"        largest gaps: {', '.join(f'{g:.0f}s' for g in sorted(big)[-3:])}")


def resources(root: Path) -> None:
    header("RESOURCE USE OVER THE RUN")
    csv = root / "stats/resources.csv"
    if not csv.exists():
        print("  no sampler data")
        return
    data: dict[str, list[tuple[str, int, int, int]]] = {"rust": [], "ts": []}
    for line in csv.read_text().splitlines()[1:]:
        parts = line.split(",")
        if len(parts) == 5 and parts[1] in data:
            data[parts[1]].append((parts[0], int(parts[2]), int(parts[3]), int(parts[4])))
    for name, points in data.items():
        if not points:
            continue
        rss = [p[1] for p in points]
        quarter = max(1, len(rss) // 4)
        print(f"  {name:<5} rss first-quarter mean {statistics.fmean(rss[:quarter]) / 1024:6.1f} MB"
              f" | last-quarter mean {statistics.fmean(rss[-quarter:]) / 1024:6.1f} MB"
              f" | peak {max(rss) / 1024:6.1f} MB"
              f" | threads {points[-1][2]:>2} | restarts {points[-1][3]}")
    r, t = data["rust"], data["ts"]
    if r and t:
        print(f"\n  ratio: TypeScript uses "
              f"{statistics.fmean(p[1] for p in t) / statistics.fmean(p[1] for p in r):.1f}x "
              f"the resident memory")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", default=str(Path.home() / ".batmon-differential"))
    args = ap.parse_args()
    root = Path(args.root)

    if not root.exists():
        print(f"no differential data at {root}", file=sys.stderr)
        return 1

    rust = rows(root / "rust/.local/share/batmon/debug.db")
    ts = rows(root / "ts/.local/share/batmon/debug.db")

    started = (root / "started-at").read_text().strip() if (root / "started-at").exists() else "?"
    header("BATMON DIFFERENTIAL REPORT")
    print(f"  started   {started}")
    print(f"  now       {dt.datetime.now(dt.timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')}")
    print(f"  rust      {len(rust):>6} flight rows, spanning {duration(rust)}")
    print(f"  typescript{len(ts):>6} flight rows, spanning {duration(ts)}")

    coverage("WHAT THE RUN EXERCISED (rust side)", rust)

    pairs = join(rust, ts)
    compare_columns(pairs)
    compare_integral(pairs)
    compare_retention(root)
    resources(root)

    print(f"\n{RULE}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
