//! System-wide counters from procfs.
//!
//! These parsers had no test coverage at all in the TypeScript daemon, for a
//! structural reason: they hardcoded `/proc`, so there was nothing to point them
//! at. Taking the mount point as a parameter is what makes them testable, and
//! they are among the most parsing-heavy code in the crate.

use std::cell::OnceCell;
use std::path::{Path, PathBuf};

use crate::formats::{js_parse_int, round_to};
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
mod tests;
