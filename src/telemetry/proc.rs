//! System-wide counters from procfs.
//!
//! These are among the most parsing-heavy code in the crate, so they take the
//! mount point as a parameter rather than hardcoding `/proc`. That is the whole
//! reason they can be tested: a parser with the path baked in has nothing to be
//! pointed at.

use std::cell::OnceCell;
use std::path::{Path, PathBuf};

use crate::formats::{parse_leading_int, round_to};
use crate::units::clamp_percent;

/// Aggregate CPU time counters from the `cpu` line of `/proc/stat`.
///
/// Monotonic since boot and measured in `USER_HZ` ticks, so only differences
/// between two readings mean anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuTimes {
    /// Idle plus I/O wait. Waiting on a disk is not doing work.
    pub idle: u64,
    /// Pure I/O wait ticks.
    pub iowait: u64,
    /// Every counted state, idle included.
    pub total: u64,
}

/// Calculated CPU and I/O wait utilisation percentages since previous tick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CpuRates {
    pub cpu_pct: f64,
    pub iowait_pct: f64,
}

/// Snapshot of `/proc/stat`: CPU times and blocked processes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcStat {
    pub times: CpuTimes,
    pub procs_blocked: Option<i64>,
}

/// Linux Pressure Stall Information (PSI) for I/O.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct IoPressure {
    /// Percentage of time some tasks were stalled on I/O (10-second rolling average).
    pub some_avg10: Option<f64>,
    /// Percentage of time ALL non-idle tasks were stalled on I/O (10-second rolling average).
    pub full_avg10: Option<f64>,
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

    /// Parse aggregate CPU counters and blocked processes from `/proc/stat`.
    #[must_use]
    pub fn proc_stat(&self) -> Option<ProcStat> {
        let stat = std::fs::read_to_string(self.base.join("stat")).ok()?;
        parse_proc_stat(&stat)
    }

    /// Parse the aggregate `cpu` line of `/proc/stat`.
    ///
    /// Exposed separately from [`ProcReader::cpu_rates`] because the per-process
    /// scan needs the same totals, and reading and parsing the file once per
    /// tick instead of twice is free.
    #[must_use]
    pub fn cpu_times(&self) -> Option<CpuTimes> {
        self.proc_stat().map(|stat| stat.times)
    }

    /// Utilisation rates (CPU and I/O wait) since the previous tick, as percentages.
    ///
    /// Returns [`None`] on the first call: utilisation is a rate, and one
    /// reading of a monotonic counter is not a rate.
    pub fn cpu_rates(&mut self, current: CpuTimes) -> Option<CpuRates> {
        let previous = self.previous_cpu.replace(current)?;

        // A counter that did not advance, or appeared to go backwards, means
        // no measurable work rather than no measurement. Reporting zero matches
        // the original and is the honest answer for a zero-length interval.
        let Some(total_delta) = current.total.checked_sub(previous.total) else {
            return Some(CpuRates {
                cpu_pct: 0.0,
                iowait_pct: 0.0,
            });
        };
        if total_delta == 0 {
            return Some(CpuRates {
                cpu_pct: 0.0,
                iowait_pct: 0.0,
            });
        }
        let idle_delta = current.idle.saturating_sub(previous.idle);
        let iowait_delta = current.iowait.saturating_sub(previous.iowait);

        let busy = 1.0 - (idle_delta as f64 / total_delta as f64);
        let iowait = iowait_delta as f64 / total_delta as f64;

        Some(CpuRates {
            cpu_pct: round_to(clamp_percent(busy * 100.0), 1),
            iowait_pct: round_to(clamp_percent(iowait * 100.0), 1),
        })
    }

    /// Utilisation since the previous tick, as a percentage.
    ///
    /// Returns [`None`] on the first call: utilisation is a rate, and one
    /// reading of a monotonic counter is not a rate. That is why the daemon's
    /// one-shot mode samples twice.
    pub fn cpu_pct(&mut self, current: CpuTimes) -> Option<f64> {
        self.cpu_rates(current).map(|rates| rates.cpu_pct)
    }

    /// Read memory utilisation percentage and uncommitted dirty pages from `/proc/meminfo`.
    #[must_use]
    pub fn memory_stats(&self) -> (Option<f64>, Option<i64>) {
        let Some(meminfo) = std::fs::read_to_string(self.base.join("meminfo")).ok() else {
            return (None, None);
        };
        let mem_pct = calculate_mem_pct(&meminfo);
        let dirty_kb = meminfo_field(&meminfo, "Dirty").and_then(|kb| i64::try_from(kb).ok());
        (mem_pct, dirty_kb)
    }

    /// Memory in use, as a percentage of total.
    ///
    /// "In use" means unavailable, not merely allocated: the kernel's
    /// `MemAvailable` estimate already discounts reclaimable page cache, so this
    /// does not report a machine with a warm cache as being out of memory.
    #[must_use]
    pub fn mem_pct(&self) -> Option<f64> {
        self.memory_stats().0
    }

    /// Uncommitted dirty memory in kilobytes waiting to be written to disk.
    #[must_use]
    pub fn dirty_kb(&self) -> Option<i64> {
        self.memory_stats().1
    }

    /// Linux Pressure Stall Information (PSI) for I/O.
    ///
    /// Reads `/proc/pressure/io`. Returns default (None) if the kernel was built
    /// without `CONFIG_PSI` or the file is otherwise unavailable.
    #[must_use]
    pub fn io_pressure(&self) -> IoPressure {
        std::fs::read_to_string(self.base.join("pressure/io"))
            .map(|contents| parse_io_pressure(&contents))
            .unwrap_or_default()
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
        iowait,
        total: user + nice + system + idle + iowait + irq + softirq + steal,
    })
}

/// Parse CPU times and blocked processes from the complete `/proc/stat` output.
fn parse_proc_stat(stat: &str) -> Option<ProcStat> {
    let times = parse_cpu_times(stat)?;
    let procs_blocked = parse_procs_blocked(stat);
    Some(ProcStat {
        times,
        procs_blocked,
    })
}

/// Parse the `procs_blocked` line from `/proc/stat`.
fn parse_procs_blocked(stat: &str) -> Option<i64> {
    for line in stat.lines() {
        if let Some(rest) = line.strip_prefix("procs_blocked ") {
            return rest.trim().parse::<i64>().ok();
        }
    }
    None
}

/// Calculate memory utilisation percentage from `/proc/meminfo`.
fn calculate_mem_pct(meminfo: &str) -> Option<f64> {
    let total = meminfo_field(meminfo, "MemTotal").filter(|kb| *kb > 0)?;

    // Pre-3.14 kernels have no MemAvailable; the sum is the historical
    // approximation of it, and is what free(1) used to report.
    let available = meminfo_field(meminfo, "MemAvailable").unwrap_or_else(|| {
        meminfo_field(meminfo, "MemFree").unwrap_or(0)
            + meminfo_field(meminfo, "Buffers").unwrap_or(0)
            + meminfo_field(meminfo, "Cached").unwrap_or(0)
    });

    let used = (total.saturating_sub(available)) as f64 / total as f64 * 100.0;
    Some(round_to(clamp_percent(used), 1))
}

/// Parse the `avg10=` value from a line in `/proc/pressure/io`.
fn parse_psi_avg10(line: &str) -> Option<f64> {
    for part in line.split_ascii_whitespace() {
        if let Some(val_str) = part.strip_prefix("avg10=") {
            let val = val_str.parse::<f64>().ok()?;
            if val.is_finite() {
                return Some(round_to(clamp_percent(val), 2));
            }
        }
    }
    None
}

/// Parse `/proc/pressure/io` for both `some` and `full` stall averages.
fn parse_io_pressure(contents: &str) -> IoPressure {
    let mut some_avg10 = None;
    let mut full_avg10 = None;

    for line in contents.lines() {
        if line.starts_with("some ") {
            some_avg10 = parse_psi_avg10(line);
        } else if line.starts_with("full ") {
            full_avg10 = parse_psi_avg10(line);
        }
    }

    IoPressure {
        some_avg10,
        full_avg10,
    }
}

/// Value of a named `/proc/meminfo` field, in kilobytes.
fn meminfo_field(meminfo: &str, name: &str) -> Option<u64> {
    for line in meminfo.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if key.trim() == name {
            // The value carries a trailing unit, so a plain parse would reject
            // it, whereas the leading-integer parse stops at the unit.
            return parse_leading_int(value).and_then(|kb| u64::try_from(kb).ok());
        }
    }
    None
}

#[cfg(test)]
mod tests;
