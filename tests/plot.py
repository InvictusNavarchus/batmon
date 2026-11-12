"""Plotting regressions with synthetic SQLite databases and a headless renderer.

Run: uv run --with 'matplotlib>=3.10,<4' --with 'seaborn>=0.13.2,<0.14' python -B tests/plot.py
"""

from contextlib import closing, redirect_stderr, redirect_stdout
from datetime import datetime, timedelta, timezone
import importlib.util
import io
import math
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[1] / "scripts/batmon-plot.py"
sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("batmon_plot", SCRIPT)
plot = importlib.util.module_from_spec(spec)
spec.loader.exec_module(plot)


class Plotting(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="batmon-plot-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.database = self.root / "recordings #1.db"
        self.end = datetime.now(timezone.utc)
        self.start = self.end - timedelta(hours=1)
        self.env = dict(os.environ, MPLCONFIGDIR=str(self.root / "matplotlib"), MPLBACKEND="Agg")
        config = patch.dict(os.environ, self.env)
        config.start()
        self.addCleanup(config.stop)

    def database_with(self, readings, columns=("cpu_temp_c", "power_w", "cpu_pct")):
        with closing(sqlite3.connect(self.database)) as connection, connection:
            connection.execute(
                "CREATE TABLE samples (id INTEGER PRIMARY KEY, ts TEXT, boot_id TEXT, "
                "uptime_s REAL, " + ", ".join(f"{column} REAL" for column in columns) + ")"
            )
            for offset, values in readings:
                connection.execute(
                    "INSERT INTO samples (ts, boot_id, uptime_s, " + ", ".join(columns)
                    + ") VALUES (" + ", ".join("?" for _ in range(3 + len(columns))) + ")",
                    (plot.iso_timestamp(self.end + timedelta(seconds=offset)), "boot-a", 10000 + offset, *values),
                )

    def command(self, *args):
        return subprocess.run(
            [sys.executable, "-B", str(SCRIPT), *args], env=self.env, cwd=self.root,
            capture_output=True, text=True, timeout=60,
        )

    def test_query_filters_wall_clock_window_and_preserves_recording_order(self):
        self.database_with([
            (-7200, (40, 2, 5)), (-300, (55, 0, 10)),
            (-360, (91, 12, 80)), (60, (48, 3, 20)),
        ])
        before = self.database.read_bytes()
        samples = plot.read_samples(self.database, self.start, self.end)
        self.assertEqual([s["cpu_temp_c"] for s in samples], [55, 91])
        self.assertEqual(samples[0]["power_w"], 0)
        self.assertEqual(samples[0]["ts"].tzinfo, timezone.utc)
        self.assertEqual(self.database.read_bytes(), before)

    def test_missing_database_is_not_created(self):
        result = self.command("--database", str(self.database))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unable to open database", result.stderr)
        self.assertFalse(self.database.exists())
        self.assertFalse((self.root / "report.png").exists())

    def test_reads_committed_wal_rows_while_writer_stays_open(self):
        self.database_with([(-120, (55, 2, 10))])
        with closing(sqlite3.connect(self.database)) as writer:
            writer.execute("PRAGMA journal_mode=WAL")
            writer.execute("PRAGMA wal_autocheckpoint=0")
            writer.execute(
                "INSERT INTO samples (ts, cpu_temp_c) VALUES (?, ?)",
                (plot.iso_timestamp(self.end - timedelta(seconds=60)), 91),
            )
            writer.commit()
            self.assertGreater(Path(str(self.database) + "-wal").stat().st_size, 0)
            samples = plot.read_samples(self.database, self.start, self.end)
            self.assertEqual([s["cpu_temp_c"] for s in samples], [55, 91])
            writer.execute("INSERT INTO samples (ts, cpu_temp_c) VALUES (?, ?)",
                           (plot.iso_timestamp(self.end), 60))
            writer.commit()
            self.assertEqual(len(plot.read_samples(self.database, self.start, self.end)), 3)

    def test_cli_renders_png_with_partial_schema_without_display(self):
        self.database_with([(-300, (55,)), (-240, (91,)), (-180, (None,)),
                            (-120, (0,)), (-60, (60,))], columns=("cpu_temp_c",))
        output = self.root / "temperatures.png"
        result = self.command("--database", str(self.database), "--since", "1.5h", "--output", str(output))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("5 samples", result.stdout)
        self.assertEqual(output.read_bytes()[:8], b"\x89PNG\r\n\x1a\n")
        from matplotlib.image import imread

        pixels = imread(output)
        self.assertGreater(pixels.shape[0], 1000)
        self.assertGreater(float(pixels.max() - pixels.min()), 0.5)

    def test_plot_preserves_peak_zero_and_missing_reading(self):
        self.database_with([(-300, (55, 0, 10)), (-240, (91, 12, 80)),
                            (-180, (None, None, None)), (-120, (0, 0, 0))])
        samples = plot.read_samples(self.database, self.start, self.end)
        figure = plot.create_figure(samples, self.start, self.end, "history", self.database,
                                    timezone_name="UTC")
        import matplotlib.pyplot as plt

        self.addCleanup(plt.close, figure)
        temperatures = figure.axes[0].lines[0].get_ydata()
        self.assertEqual(list(temperatures[:2]), [55, 91])
        self.assertTrue(math.isnan(temperatures[2]))
        self.assertEqual(temperatures[3], 0)
        self.assertEqual(list(figure.axes[1].lines[0].get_ydata())[:2], [0, 12])
        self.assertEqual(figure.axes[-1].get_xlabel(), "Time (UTC)")

    def test_display_timezone_changes_labels_without_changing_recorded_times(self):
        import matplotlib.dates as dates
        import matplotlib.pyplot as plt

        fall = datetime(2026, 11, 1, 5, 30, tzinfo=timezone.utc)
        spring = datetime(2026, 3, 8, 6, 30, tzinfo=timezone.utc)
        cases = (
            ("UTC", fall, "2026-11-01 05:30 +0000", "2026-11-01 06:30 +0000"),
            ("Asia/Jakarta", fall, "2026-11-01 12:30 +0700", "2026-11-01 13:30 +0700"),
            ("America/New_York", fall, "2026-11-01 01:30 -0400", "2026-11-01 01:30 -0500"),
            ("America/New_York", spring, "2026-03-08 01:30 -0500", "2026-03-08 03:30 -0400"),
        )
        for name, start, expected_start, expected_end in cases:
            with self.subTest(timezone=name, start=start):
                end = start + timedelta(hours=1)
                samples = [{"ts": start, "cpu_temp_c": 55}, {"ts": end, "cpu_temp_c": 60}]
                figure = plot.create_figure(samples, start, end, "history", self.database,
                                            timezone_name=name)
                self.addCleanup(plt.close, figure)
                axis = figure.axes[-1]
                self.assertEqual(axis.get_xlabel(), f"Time ({name})")
                self.assertIn(expected_start + " → " + expected_end, figure.texts[1].get_text())
                # Exercise the actual tick formatter, including the repeated DST hour.
                formatter = axis.xaxis.get_major_formatter()
                formatter.formats = ["%Y-%m-%d %H:%M %z"] * 6
                formatter.zero_formats = formatter.formats
                labels = formatter.format_ticks(dates.date2num([start, end]))
                self.assertEqual(labels, [expected_start, expected_end])
                self.assertAlmostEqual(axis.get_xlim()[1] - axis.get_xlim()[0], 1 / 24)
                line = figure.axes[0].lines[0]
                recorded_times = [timestamp for timestamp, value in zip(line.get_xdata(), line.get_ydata())
                                  if math.isfinite(value)]
                self.assertEqual(recorded_times, [start, end])
                self.assertEqual([s["ts"].tzinfo for s in samples], [timezone.utc, timezone.utc])

    def test_local_timezone_uses_tz_override_and_retains_dst_rules(self):
        import matplotlib.dates as dates
        import matplotlib.pyplot as plt

        start = datetime(2026, 11, 1, 5, 30, tzinfo=timezone.utc)
        end = start + timedelta(hours=1)
        with patch.dict(os.environ, TZ="America/New_York"):
            figure = plot.create_figure([], start, end, "history", self.database)
        self.addCleanup(plt.close, figure)
        axis = figure.axes[-1]
        self.assertEqual(axis.get_xlabel(), "Time (local)")
        self.assertIn("2026-11-01 01:30 -0400 → 2026-11-01 01:30 -0500",
                      figure.texts[1].get_text())
        formatter = axis.xaxis.get_major_formatter()
        formatter.formats = ["%H:%M %z"] * 6
        formatter.zero_formats = formatter.formats
        self.assertEqual(formatter.format_ticks(dates.date2num([start, end])),
                         ["01:30 -0400", "01:30 -0500"])

    def test_cli_accepts_local_utc_named_zones_and_rejects_unknown_zone(self):
        self.database_with([(-60, (55, 2, 10))])
        for name in ("local", "UTC", "Asia/Jakarta"):
            with self.subTest(timezone=name):
                output = self.root / (name.replace("/", "-") + ".png")
                result = self.command("--database", str(self.database), "--timezone", name,
                                      "--output", str(output))
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertTrue(output.is_file())
        result = self.command("--database", str(self.database), "--timezone", "Invalid/Timezone")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unknown timezone", result.stderr)
        self.assertNotIn("Traceback", result.stderr)
        self.assertFalse((self.root / "report.png").exists())

    def test_line_breaks_at_downtime_reboot_and_clock_changes(self):
        def sample(seconds, boot="a", uptime=None):
            return {"ts": self.start + timedelta(seconds=seconds), "cpu_temp_c": 50,
                    "boot_id": boot, "uptime_s": uptime}

        cases = (
            [sample(0), sample(60), sample(3600)],
            [sample(0), sample(60), sample(120, boot="b")],
            [sample(0), sample(60), sample(30)],
            [sample(0), sample(60), sample(60)],
            [sample(0, uptime=1000), sample(60, uptime=1060), sample(120, uptime=5000)],
            [sample(0), sample(1), sample(10)],
        )
        for index, samples in enumerate(cases):
            with self.subTest(index=index):
                _, values = plot.series(samples, "cpu_temp_c", 1 if index == 5 else 60)
                self.assertEqual(values[:2], [50, 50])
                self.assertTrue(math.isnan(values[2]))
                self.assertEqual(values[3], 50)

    def test_empty_window_and_all_missing_metrics_fail_without_output(self):
        self.database_with([(-7200, (55, 2, 10)), (-60, (None, None, None))])
        for since, message in (("0.001h", "no samples"), ("1h", "no temperature")):
            with self.subTest(since=since):
                result = self.command("--database", str(self.database), "--since", since)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(message, result.stderr)
                self.assertFalse((self.root / "report.png").exists())

    def test_source_selects_default_database_under_user_home(self):
        self.database_with([(-60, (55, 2, 10))])
        directory = self.root / ".local/share/batmon"
        directory.mkdir(parents=True)
        original = self.database.read_bytes()
        for source, name in (("history", "battery.db"), ("debug", "debug.db")):
            with self.subTest(source=source), patch.object(Path, "home", return_value=self.root):
                database = directory / name
                database.write_bytes(original)
                output = self.root / f"{source}.png"
                with redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()):
                    status = plot.main(["--source", source, "--output", str(output)])
                self.assertEqual(status, 0)
                self.assertTrue(output.is_file())
                self.assertEqual(database.read_bytes(), original)

    def test_bad_arguments_fail_before_creating_output(self):
        for since in ("0h", "-1h", "1w", "nan", "infh", "9" * 400 + "d"):
            with self.subTest(since=since):
                result = self.command("--since=" + since)
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertIn("positive duration", result.stderr)
        result = self.command("--output", "report.svg")
        self.assertEqual(result.returncode, 2)
        self.assertFalse((self.root / "report.svg").exists())

    def test_schema_errors_are_clear_and_database_is_unchanged(self):
        with closing(sqlite3.connect(self.database)) as connection, connection:
            connection.execute("CREATE TABLE unrelated (value REAL)")
        before = self.database.read_bytes()
        result = self.command("--database", str(self.database))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("samples table", result.stderr)
        self.assertNotIn("Traceback", result.stderr)
        self.assertEqual(self.database.read_bytes(), before)


if __name__ == "__main__":
    unittest.main()
