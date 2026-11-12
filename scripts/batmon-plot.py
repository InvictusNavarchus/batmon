#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["matplotlib>=3.10,<4", "seaborn>=0.13.2,<0.14", "python-dateutil>=2.9,<5"]
# ///
"""Plot recorded telemetry without changing batmon's databases or service."""

import argparse
from contextlib import closing
from datetime import datetime, timedelta, timezone
import math
from pathlib import Path
import re
import sqlite3
import sys


PANELS = (
    ("Temperature", "°C", (
        ("cpu_temp_c", "CPU"), ("gpu_temp_c", "GPU"),
        ("nvme_temp_c", "NVMe"), ("battery_temp_c", "Battery"),
    )),
    ("Power", "W", (("power_w", "Battery"), ("gpu_power_w", "GPU"))),
    ("Utilization", "%", (
        ("cpu_pct", "CPU"), ("gpu_pct", "GPU"), ("mem_pct", "Memory"),
    )),
)
METRICS = tuple(column for _, _, fields in PANELS for column, _ in fields)


def duration(value):
    """Accept a positive duration in hours or days, including fractional values."""
    match = re.fullmatch(r"(\d+(?:\.\d+)?)([hd])", value)
    if match:
        try:
            seconds = float(match[1]) * (3600 if match[2] == "h" else 86400)
            result = timedelta(seconds=seconds)
            if result > timedelta(0):
                return result
        except OverflowError:
            pass
    raise argparse.ArgumentTypeError("use a positive duration such as 12h, 1.5h, or 7d")


def iso_timestamp(value):
    return value.astimezone(timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")


def read_samples(path, start, end):
    """Fetch only plot columns; close the read snapshot before doing any rendering."""
    uri = path.expanduser().resolve().as_uri() + "?mode=ro"
    with closing(sqlite3.connect(uri, uri=True, timeout=5)) as connection:
        connection.row_factory = sqlite3.Row
        columns = {row[1] for row in connection.execute("PRAGMA table_info(samples)")}
        if not {"id", "ts"} <= columns:
            raise ValueError("database must contain a samples table with id and ts columns")
        selected = ["ts", *[c for c in ("boot_id", "uptime_s", *METRICS) if c in columns]]
        # Names come only from our allowlist. Values remain bound parameters.
        rows = connection.execute(
            f"SELECT {', '.join(selected)} FROM samples WHERE ts >= ? AND ts <= ? ORDER BY id",
            (iso_timestamp(start), iso_timestamp(end)),
        ).fetchall()
    samples = []
    for row in rows:
        sample = dict(row)
        timestamp = datetime.fromisoformat(sample["ts"])
        if timestamp.tzinfo is None:
            raise ValueError("sample timestamps must include a UTC offset")
        sample["ts"] = timestamp.astimezone(timezone.utc)
        samples.append(sample)
    return samples


def series(samples, column, interval):
    """Preserve missing readings and break across downtime, reboots, and clock steps."""
    times, values = [], []
    previous = None
    for sample in samples:
        timestamp = sample["ts"]
        if previous is not None:
            elapsed = (timestamp - previous["ts"]).total_seconds()
            boot_changed = (
                sample.get("boot_id") != previous.get("boot_id")
                and (sample.get("boot_id") is not None or previous.get("boot_id") is not None)
            )
            uptime, old_uptime = sample.get("uptime_s"), previous.get("uptime_s")
            clock_changed = (
                uptime is not None and old_uptime is not None
                and abs(elapsed - (uptime - old_uptime)) > interval * 3
            )
            if elapsed <= 0 or elapsed > interval * 3 or boot_changed or clock_changed:
                times.append(timestamp)
                values.append(math.nan)
        value = sample.get(column)
        times.append(timestamp)
        values.append(float(value) if value is not None else math.nan)
        previous = sample
    return times, values


def create_figure(samples, start, end, source, database, show=False, timezone_name="local"):
    from dateutil.tz import gettz
    import matplotlib

    display_timezone = gettz() if timezone_name == "local" else gettz(timezone_name)
    if not timezone_name or display_timezone is None:
        raise ValueError(f"unknown timezone {timezone_name!r}; use local, UTC, or an IANA name such as Asia/Jakarta")
    if not show:
        matplotlib.use("Agg")
    import matplotlib.dates as dates
    import matplotlib.pyplot as plt
    import seaborn as sns

    sns.set_theme(style="whitegrid", context="notebook", palette="colorblind", rc={
        "axes.spines.top": False, "axes.spines.right": False,
        "grid.alpha": 0.25, "axes.titleweight": "bold",
    })
    figure, axes = plt.subplots(3, 1, figsize=(12, 9), sharex=True)
    colors = dict(zip(("CPU", "GPU", "NVMe", "Battery", "Memory"),
                      sns.color_palette("colorblind", 5)))
    interval = 1 if source == "debug" else 60
    for axis, (title, unit, fields) in zip(axes, PANELS):
        for column, label in fields:
            times, values = series(samples, column, interval)
            if any(math.isfinite(value) for value in values):
                axis.plot(times, values, label=label, color=colors[label], linewidth=1.3,
                          marker=".", markersize=2.5)
        axis.set_title(title, loc="left", fontsize=12)
        axis.set_ylabel(unit)
        if axis.lines:
            axis.legend(loc="upper left", bbox_to_anchor=(1.01, 1), frameon=False)
        else:
            axis.text(0.5, 0.5, "No readings available", ha="center", va="center",
                      transform=axis.transAxes, color="0.45")
        if unit == "%":
            axis.set_ylim(0, 100)
    locator = dates.AutoDateLocator(minticks=4, maxticks=9, tz=display_timezone)
    axes[-1].xaxis.set_major_locator(locator)
    axes[-1].xaxis.set_major_formatter(dates.ConciseDateFormatter(locator, tz=display_timezone))
    axes[-1].set_xlim(start, end)
    axes[-1].set_xlabel(f"Time ({timezone_name})")
    figure.suptitle("batmon · recorded telemetry", x=0.09, ha="left", fontsize=18, weight="bold")
    local_start = start.astimezone(display_timezone)
    local_end = end.astimezone(display_timezone)
    figure.text(0.09, 0.935,
                f"{local_start:%Y-%m-%d %H:%M %z} → {local_end:%Y-%m-%d %H:%M %z}  ·  "
                f"{len(samples):,} samples  ·  {database.name}", fontsize=10, color="0.4")
    resolution = "1-second flight recorder" if source == "debug" else "1-minute historical snapshots"
    figure.text(0.09, 0.025,
                f"{resolution} · no smoothing · gaps indicate missing readings or recording discontinuities",
                fontsize=9, color="0.4")
    figure.subplots_adjust(left=0.09, right=0.84, top=0.88, bottom=0.1, hspace=0.4)
    return figure


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--since", type=duration, default="24h", metavar="12h|7d",
                        help="wall-clock window ending now (default: 24h)")
    parser.add_argument("--source", choices=("history", "debug"), default="history",
                        help="minute history or second-resolution flight recorder (default: history)")
    parser.add_argument("--database", type=Path,
                        help="override the database path; --source still sets the expected cadence")
    parser.add_argument("--output", type=Path, default=Path("report.png"),
                        help="PNG output path (default: report.png)")
    parser.add_argument("--show", action="store_true", help="also open an interactive plot window")
    parser.add_argument("--timezone", default="local", metavar="ZONE",
                        help="display timezone: local, UTC, or an IANA name (default: local)")
    args = parser.parse_args(argv)
    if args.output.suffix.lower() != ".png":
        parser.error("--output must end in .png")
    database = args.database or Path.home() / ".local/share/batmon" / (
        "debug.db" if args.source == "debug" else "battery.db"
    )
    end = datetime.now(timezone.utc)
    try:
        start = end - args.since
        samples = read_samples(database, start, end)
        if not samples:
            raise ValueError("no samples in the requested window; try a longer --since or another --source")
        if not any(sample.get(column) is not None for sample in samples for column in METRICS):
            raise ValueError("no temperature, power, or utilization readings in the requested window")
        figure = create_figure(samples, start, end, args.source, database,
                               show=args.show, timezone_name=args.timezone)
        import matplotlib.pyplot as plt

        try:
            figure.savefig(args.output.expanduser(), format="png", dpi=160)
            if args.show:
                plt.show()
        finally:
            plt.close(figure)
    except ImportError as error:
        print(f"batmon-plot: {error}. Run with uv run --script {sys.argv[0]}", file=sys.stderr)
        return 1
    except (OSError, sqlite3.Error, ValueError, OverflowError) as error:
        print(f"batmon-plot: {error}", file=sys.stderr)
        return 1
    print(f"Saved {args.output} ({len(samples):,} samples from {database})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
