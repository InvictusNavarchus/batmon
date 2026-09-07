//! System-wide counters from procfs.
//!
//! These parsers had no test coverage at all in the TypeScript daemon, for a
//! structural reason: they hardcoded `/proc`, so there was nothing to point them
//! at. Taking the mount point as a parameter is what makes them testable, and
//! they are among the most parsing-heavy code in the crate.

use std::cell::OnceCell;
use std::path::{Path, PathBuf};

use crate::parity::{js_parse_int, round_to};
use crate::units::clamp_percent;

/// Aggregate CPU time counters from the `cpu` line of `/proc/stat`.
///
/// Monotonic since boot and measured in `USER_HZ` ticks, so only differences
/// between two readings mean anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuTimes {
    /// Idle plus I/O wait. Waiting on a disk is not doing work.
    pub idle: u64,
    /// Every counted state, idle included.
    pub total: u64,
}

/// Reads the process filesystem, remembering what it needs between ticks.
#[derive(Debug)]
pub struct ProcReader {
    base: PathBuf,
    previous_cpu: Option<CpuTimes>,
    /// Resolved once — the boot id cannot change without the process restarting.
    boot_id: OnceCell<Option<String>>,
    /// Resolved once — physical memory does not change on a running machine.
    mem_total_kb: OnceCell<u64>,
}

impl ProcReader {
    #[must_use]
    pub fn new(base: impl Into<PathBuf>) -> Self {
        Self {
            base: base.into(),
            previous_cpu: None,
            boot_id: OnceCell::new(),
            mem_total_kb: OnceCell::new(),
        }
    }

    /// The procfs mount point being read.
    #[must_use]
    pub fn base(&self) -> &Path {
        &self.base
    }

    /// Parse the aggregate `cpu` line of `/proc/stat`.
    ///
    /// Exposed separately from [`ProcReader::cpu_pct`] because the per-process
    /// scan needs the same totals. The TypeScript read and parsed this file
    /// twice per tick; reading it once and sharing the result is free.
    #[must_use]
    pub fn cpu_times(&self) -> Option<CpuTimes> {
        let stat = std::fs::read_to_string(self.base.join("stat")).ok()?;
        parse_cpu_times(&stat)
    }

    /// Utilisation since the previous tick, as a percentage.
    ///
    /// Returns [`None`] on the first call: utilisation is a rate, and one
    /// reading of a monotonic counter is not a rate. That is why the daemon's
    /// one-shot mode samples twice.
    pub fn cpu_pct(&mut self, current: CpuTimes) -> Option<f64> {
        let previous = self.previous_cpu.replace(current)?;

        // A counter that did not advance, or appeared to go backwards, means
        // no measurable work rather than no measurement. Reporting zero matches
        // the original and is the honest answer for a zero-length interval.
        let Some(total_delta) = current.total.checked_sub(previous.total) else {
            return Some(0.0);
        };
        if total_delta == 0 {
            return Some(0.0);
        }
        let idle_delta = current.idle.saturating_sub(previous.idle);

        let busy = 1.0 - (idle_delta as f64 / total_delta as f64);
        Some(round_to(clamp_percent(busy * 100.0), 1))
    }

    /// Memory in use, as a percentage of total.
    ///
    /// "In use" means unavailable, not merely allocated: the kernel's
    /// `MemAvailable` estimate already discounts reclaimable page cache, so this
    /// does not report a machine with a warm cache as being out of memory.
    #[must_use]
    pub fn mem_pct(&self) -> Option<f64> {
        let meminfo = std::fs::read_to_string(self.base.join("meminfo")).ok()?;
        let field = |name: &str| meminfo_field(&meminfo, name);

        let total = field("MemTotal").filter(|kb| *kb > 0)?;

        // Pre-3.14 kernels have no MemAvailable; the sum is the historical
        // approximation of it, and is what free(1) used to report.
        let available = field("MemAvailable").unwrap_or_else(|| {
            field("MemFree").unwrap_or(0)
                + field("Buffers").unwrap_or(0)
                + field("Cached").unwrap_or(0)
        });

        let used = (total.saturating_sub(available)) as f64 / total as f64 * 100.0;
        Some(round_to(clamp_percent(used), 1))
    }

    /// Total physical memory in kilobytes, resolved once.
    ///
    /// Only a successful read is cached. A sentinel would be cached forever,
    /// and every later `top_processes` row would report memory as a percentage
    /// of that sentinel — wrong in a way that looks like data, long after
    /// `/proc/meminfo` had recovered.
    ///
    /// [`None`] means the denominator is unknown, and the caller should omit
    /// the metrics that need it rather than invent one.
    pub fn mem_total_kb(&self) -> Option<u64> {
        if let Some(cached) = self.mem_total_kb.get() {
            return Some(*cached);
        }

        let total = std::fs::read_to_string(self.base.join("meminfo"))
            .ok()
            .and_then(|meminfo| meminfo_field(&meminfo, "MemTotal"))
            .filter(|kb| *kb > 0)?;

        let _ = self.mem_total_kb.set(total);
        Some(total)
    }

    /// One-minute load average.
    #[must_use]
    pub fn load1(&self) -> Option<f64> {
        let loadavg = std::fs::read_to_string(self.base.join("loadavg")).ok()?;
        let first = loadavg.split_ascii_whitespace().next()?;
        first
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite())
            .map(|value| round_to(value, 2))
    }

    /// Seconds since boot, unrounded.
    #[must_use]
    pub fn uptime_s(&self) -> Option<f64> {
        let uptime = std::fs::read_to_string(self.base.join("uptime")).ok()?;
        let first = uptime.split_ascii_whitespace().next()?;
        first.parse::<f64>().ok().filter(|value| value.is_finite())
    }

    /// The kernel's boot session UUID, resolved once.
    ///
    /// Used to tell a gap in the record apart from a reboot, which is what stops
    /// the cycle integrator from counting energy lost while the machine was off.
    pub fn boot_id(&self) -> Option<String> {
        self.boot_id
            .get_or_init(|| {
                std::fs::read_to_string(self.base.join("sys/kernel/random/boot_id"))
                    .ok()
                    .map(|contents| contents.trim().to_owned())
                    .filter(|contents| !contents.is_empty())
            })
            .clone()
    }
}

/// Parse the aggregate `cpu` line, which is always first in `/proc/stat`.
fn parse_cpu_times(stat: &str) -> Option<CpuTimes> {
    let line = stat.lines().next()?;
    if !line.starts_with("cpu ") {
        return None;
    }

    // user nice system idle iowait irq softirq steal [guest guest_nice]
    // Guest time is already counted inside user, so including it would
    // double-count; the kernel's own tools stop at steal for the same reason.
    let mut fields = line.split_ascii_whitespace().skip(1);
    let mut counters = [0u64; 8];
    for counter in &mut counters {
        // A short line is normal on old kernels, which simply omit the later
        // columns. Missing counters stay zero rather than failing the parse.
        let Some(field) = fields.next() else { break };
        *counter = field.parse().ok()?;
    }

    let [user, nice, system, idle, iowait, irq, softirq, steal] = counters;

    Some(CpuTimes {
        idle: idle + iowait,
        total: user + nice + system + idle + iowait + irq + softirq + steal,
    })
}

/// Value of a named `/proc/meminfo` field, in kilobytes.
fn meminfo_field(meminfo: &str, name: &str) -> Option<u64> {
    for line in meminfo.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if key.trim() == name {
            // The value carries a trailing unit, so a plain parse would reject
            // it; parseInt semantics stop at the first non-digit.
            return js_parse_int(value).and_then(|kb| u64::try_from(kb).ok());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::float_cmp)]

    use super::*;
    use tempfile::TempDir;

    /// A fake procfs containing the given files.
    fn procfs(files: &[(&str, &str)]) -> (TempDir, ProcReader) {
        let tmp = TempDir::new().unwrap();
        for (name, contents) in files {
            let path = tmp.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }
        let reader = ProcReader::new(tmp.path());
        (tmp, reader)
    }

    fn stat_line(user: u64, nice: u64, system: u64, idle: u64, iowait: u64) -> String {
        format!(
            "cpu  {user} {nice} {system} {idle} {iowait} 0 0 0 0 0\ncpu0 1 2 3 4 5 0 0 0 0 0\nintr 12345\n"
        )
    }

    #[test]
    fn cpu_times_sum_the_aggregate_line() {
        let (_tmp, reader) = procfs(&[("stat", &stat_line(100, 10, 50, 800, 40))]);

        let times = reader.cpu_times().unwrap();
        assert_eq!(times.idle, 840, "idle counts iowait");
        assert_eq!(times.total, 1_000);
    }

    #[test]
    fn cpu_utilisation_needs_two_readings() {
        let (_tmp, mut reader) = procfs(&[("stat", &stat_line(0, 0, 0, 0, 0))]);
        let first = reader.cpu_times().unwrap();

        assert_eq!(
            reader.cpu_pct(first),
            None,
            "one reading of a counter is not a rate"
        );
    }

    #[test]
    fn cpu_utilisation_is_the_non_idle_share_of_the_interval() {
        let (tmp, mut reader) = procfs(&[("stat", &stat_line(100, 0, 100, 800, 0))]);
        let first = reader.cpu_times().unwrap();
        assert_eq!(reader.cpu_pct(first), None);

        // 250 more ticks, of which 100 were idle: 60% busy.
        std::fs::write(tmp.path().join("stat"), stat_line(200, 0, 150, 900, 0)).unwrap();
        let second = reader.cpu_times().unwrap();

        assert_eq!(reader.cpu_pct(second), Some(60.0));
    }

    #[test]
    fn a_fully_idle_interval_reports_zero() {
        let (tmp, mut reader) = procfs(&[("stat", &stat_line(100, 0, 100, 800, 0))]);
        let first = reader.cpu_times().unwrap();
        let _ = reader.cpu_pct(first);

        std::fs::write(tmp.path().join("stat"), stat_line(100, 0, 100, 900, 0)).unwrap();
        let second = reader.cpu_times().unwrap();

        assert_eq!(reader.cpu_pct(second), Some(0.0));
    }

    #[test]
    fn a_zero_length_interval_reports_zero_rather_than_dividing() {
        let (_tmp, mut reader) = procfs(&[("stat", &stat_line(100, 0, 100, 800, 0))]);
        let times = reader.cpu_times().unwrap();
        let _ = reader.cpu_pct(times);

        assert_eq!(reader.cpu_pct(times), Some(0.0));
    }

    #[test]
    fn counters_that_appear_to_go_backwards_report_zero() {
        let (tmp, mut reader) = procfs(&[("stat", &stat_line(500, 0, 500, 5_000, 0))]);
        let first = reader.cpu_times().unwrap();
        let _ = reader.cpu_pct(first);

        std::fs::write(tmp.path().join("stat"), stat_line(1, 0, 1, 1, 0)).unwrap();
        let second = reader.cpu_times().unwrap();

        assert_eq!(reader.cpu_pct(second), Some(0.0));
    }

    #[test]
    fn a_stat_file_without_the_aggregate_line_is_rejected() {
        let (_tmp, reader) = procfs(&[("stat", "cpu0 1 2 3 4 5 0 0 0\nintr 1\n")]);
        assert_eq!(reader.cpu_times(), None);
    }

    #[test]
    fn a_short_stat_line_from_an_old_kernel_still_parses() {
        // Kernels before 2.6.11 omit steal and everything after it.
        let (_tmp, reader) = procfs(&[("stat", "cpu  100 10 50 800\n")]);

        let times = reader.cpu_times().unwrap();
        assert_eq!(times.idle, 800);
        assert_eq!(times.total, 960);
    }

    #[test]
    fn a_missing_stat_file_yields_nothing() {
        let (_tmp, reader) = procfs(&[("meminfo", "MemTotal: 100 kB\n")]);
        assert_eq!(reader.cpu_times(), None);
    }

    #[test]
    fn memory_use_is_computed_against_the_kernels_availability_estimate() {
        let (_tmp, reader) = procfs(&[(
            "meminfo",
            "MemTotal:       16268140 kB\nMemFree:          500000 kB\nMemAvailable:    6268140 kB\nBuffers:          200000 kB\nCached:          8000000 kB\n",
        )]);

        // 10,000,000 of 16,268,140 kB unavailable.
        assert_eq!(reader.mem_pct(), Some(61.5));
    }

    #[test]
    fn memory_use_falls_back_to_the_historical_approximation() {
        // Pre-3.14 kernels have no MemAvailable.
        let (_tmp, reader) = procfs(&[(
            "meminfo",
            "MemTotal:       1000000 kB\nMemFree:         100000 kB\nBuffers:          50000 kB\nCached:          250000 kB\n",
        )]);

        // 400,000 kB available, so 60% in use.
        assert_eq!(reader.mem_pct(), Some(60.0));
    }

    #[test]
    fn a_warm_page_cache_is_not_reported_as_memory_pressure() {
        let (_tmp, reader) = procfs(&[(
            "meminfo",
            "MemTotal:       1000000 kB\nMemFree:           10000 kB\nMemAvailable:     900000 kB\nCached:           890000 kB\n",
        )]);

        assert_eq!(reader.mem_pct(), Some(10.0));
    }

    #[test]
    fn memory_reporting_needs_a_usable_total() {
        let (_tmp, missing) = procfs(&[("meminfo", "MemFree: 100 kB\n")]);
        assert_eq!(missing.mem_pct(), None);

        let (_tmp, zero) = procfs(&[("meminfo", "MemTotal: 0 kB\n")]);
        assert_eq!(zero.mem_pct(), None);
    }

    #[test]
    fn meminfo_lines_without_a_colon_are_skipped_not_fatal() {
        let (_tmp, reader) = procfs(&[(
            "meminfo",
            "garbage line\nMemTotal:       1000000 kB\nMemAvailable:    400000 kB\n\n",
        )]);

        assert_eq!(reader.mem_pct(), Some(60.0));
    }

    #[test]
    fn total_memory_is_resolved_once_and_cached() {
        let (tmp, reader) = procfs(&[("meminfo", "MemTotal:       1000000 kB\n")]);
        assert_eq!(reader.mem_total_kb(), Some(1_000_000));

        std::fs::remove_file(tmp.path().join("meminfo")).unwrap();
        assert_eq!(
            reader.mem_total_kb(),
            Some(1_000_000),
            "value was not cached"
        );
    }

    #[test]
    fn an_unreadable_total_memory_is_not_cached_and_recovers() {
        // Caching a sentinel would make every later top_processes row report
        // memory against it, wrong in a way that looks like data.
        let (tmp, reader) = procfs(&[("stat", "cpu  1 1 1 1\n")]);
        assert_eq!(reader.mem_total_kb(), None);

        std::fs::write(tmp.path().join("meminfo"), "MemTotal: 1000000 kB\n").unwrap();

        assert_eq!(
            reader.mem_total_kb(),
            Some(1_000_000),
            "a failed read was cached and blocked recovery"
        );
    }

    #[test]
    fn load_average_takes_the_one_minute_figure() {
        let (_tmp, reader) = procfs(&[("loadavg", "1.19 0.94 0.83 2/1543 12345\n")]);
        assert_eq!(reader.load1(), Some(1.19));
    }

    #[test]
    fn load_average_is_rounded_to_two_places() {
        let (_tmp, reader) = procfs(&[("loadavg", "1.198765 0.94 0.83 2/1543 1\n")]);
        assert_eq!(reader.load1(), Some(1.2));
    }

    #[test]
    fn a_missing_or_malformed_loadavg_yields_nothing() {
        let (_tmp, missing) = procfs(&[("stat", "cpu  1 1 1 1\n")]);
        assert_eq!(missing.load1(), None);

        let (_tmp, malformed) = procfs(&[("loadavg", "nonsense\n")]);
        assert_eq!(malformed.load1(), None);
    }

    #[test]
    fn uptime_is_read_unrounded() {
        let (_tmp, reader) = procfs(&[("uptime", "59289.86 412345.67\n")]);
        assert_eq!(reader.uptime_s(), Some(59_289.86));
    }

    #[test]
    fn a_missing_uptime_yields_nothing() {
        let (_tmp, reader) = procfs(&[("stat", "cpu  1 1 1 1\n")]);
        assert_eq!(reader.uptime_s(), None);
    }

    #[test]
    fn the_boot_id_is_read_and_cached() {
        let (tmp, reader) = procfs(&[(
            "sys/kernel/random/boot_id",
            "7d0e0b50-a9e4-4880-8366-14fe16e77f9b\n",
        )]);

        assert_eq!(
            reader.boot_id().as_deref(),
            Some("7d0e0b50-a9e4-4880-8366-14fe16e77f9b")
        );

        std::fs::remove_file(tmp.path().join("sys/kernel/random/boot_id")).unwrap();
        assert!(reader.boot_id().is_some(), "value was not cached");
    }

    #[test]
    fn a_missing_boot_id_is_remembered_as_absent() {
        let (_tmp, reader) = procfs(&[("stat", "cpu  1 1 1 1\n")]);

        assert_eq!(reader.boot_id(), None);
        assert_eq!(reader.boot_id(), None);
    }

    #[test]
    fn a_fresh_reader_carries_no_state_from_another() {
        // What the four *ForTesting reset helpers existed to fake.
        let (_tmp, mut first) = procfs(&[("stat", &stat_line(100, 0, 100, 800, 0))]);
        let times = first.cpu_times().unwrap();
        assert_eq!(first.cpu_pct(times), None);
        assert_eq!(first.cpu_pct(times), Some(0.0));

        let mut second = ProcReader::new(first.base());
        assert_eq!(
            second.cpu_pct(times),
            None,
            "a new reader must start without history"
        );
    }
}
