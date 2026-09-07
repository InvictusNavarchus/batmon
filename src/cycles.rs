//! Software cycle counting by energy throughput.
//!
//! Most battery management systems either do not report a cycle count or report
//! one that only moves in whole numbers, long after the wear has happened. This
//! integrates the real thing: every watt-hour that leaves the pack while
//! discharging is a fraction of a design capacity, and those fractions accumulate.

use crate::types::{PowerState, Sample};

/// Cycle count for `curr`, carried forward from `prev`.
///
/// Pure and total: no I/O, no clock, no state. The result is always the previous
/// count plus a non-negative increment, so the series can only ever rise.
///
/// Three guards keep the integral honest, and each exists because of a real way
/// the number would otherwise drift upward:
///
/// - **Only while discharging.** Energy readings dip while plugged in — float
///   charge cycles, a vendor charge limit releasing, plain sensor noise. Counting
///   those would accrue wear the battery never suffered.
/// - **Never across a reboot.** A machine powered off for a week comes back with
///   less energy than it had, and none of that loss went through a load. A
///   changed `boot_id`, or an uptime that went backwards, means the gap is not
///   ours to integrate.
/// - **Never more than one cycle in a tick.** A delta larger than the entire
///   design capacity is a driver glitch or a battery swap, not a discharge.
///
/// When a guard trips, the carried count is returned unchanged. A guard means
/// "this interval cannot be integrated", never "the history is void".
///
/// With no previous sample the result is `0.0`, which is what a fresh database
/// looks like. Every other path returns the carried count or more -- including
/// zero, when that is what was carried. The series is cumulative wear, so it
/// must never fall.
#[must_use]
pub fn compute_estimated_cycles(curr: &Sample, prev: Option<&Sample>) -> f64 {
    let Some(prev) = prev else {
        return 0.0;
    };

    let carried = prev.estimated_cycle_count;

    // An unreadable design capacity means the increment cannot be computed, not
    // that the wear never happened. Returning zero here would silently reset a
    // number that represents months of accumulated history.
    if curr.energy_design_wh <= 0.0 {
        return carried;
    }

    if crossed_boot_boundary(curr, prev) {
        return carried;
    }

    if curr.power_state != PowerState::Discharging {
        return carried;
    }

    let delta_wh = prev.energy_wh - curr.energy_wh;
    if delta_wh > 0.0 && delta_wh <= curr.energy_design_wh {
        return carried + delta_wh / curr.energy_design_wh;
    }

    carried
}

/// Whether the machine rebooted between the two samples.
///
/// Two independent signals, either of which is sufficient. `boot_id` is
/// authoritative but absent on kernels that do not expose it; a decreasing
/// uptime catches the same event without it. Both are skipped when either side
/// is missing rather than guessed at, because a false positive here silently
/// discards real discharge.
fn crossed_boot_boundary(curr: &Sample, prev: &Sample) -> bool {
    let boot_id_changed = match (&prev.boot_id, &curr.boot_id) {
        (Some(before), Some(after)) => before != after,
        _ => false,
    };

    let uptime_went_backwards = match (prev.uptime_s, curr.uptime_s) {
        (Some(before), Some(after)) => after < before,
        _ => false,
    };

    boot_id_changed || uptime_went_backwards
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod properties;
