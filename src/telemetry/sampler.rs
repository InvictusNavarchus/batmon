//! Assembling one complete telemetry sample.
//!
//! The [`Sampler`] owns every reader and every piece of state that has to
//! survive between ticks. In the TypeScript daemon that state lived in nine
//! module-level mutables, which is why four `resetForTesting` functions had to
//! be exported from production modules; here a fresh `Sampler` is a fresh state
//! and those escape hatches do not exist.

use std::time::{Duration, Instant};

use crate::formats::{now_iso8601_millis, round_js, round_to};
use crate::paths::Paths;
use crate::telemetry::battery::BatteryReader;
use crate::telemetry::proc::ProcReader;
use crate::telemetry::processes::ProcessReader;
use crate::telemetry::system::{read_cpu_freq_mhz, read_gpu_pct};
use crate::telemetry::thermal::ThermalReader;
use crate::types::{PowerState, Sample};

/// How long a smoothed runtime estimate is reused before being fetched again.
///
/// The estimates come from UPower over D-Bus, which is comparatively expensive
/// and — more importantly — changes slowly by design, since its whole value over
/// instantaneous arithmetic is that it averages across minutes.
const ESTIMATE_TTL: Duration = Duration::from_secs(60);

/// Discharge or charge rate below which a runtime estimate is meaningless.
///
/// Dividing remaining energy by a near-zero rate produces a number in the tens
/// of days, which is worse than admitting we do not know.
const MIN_RATE_FOR_ESTIMATE_W: f64 = 0.5;

/// Smoothed runtime estimates from an external source.
///
/// The seam that lets the sampler be tested without a message bus. Kept behind a
/// trait object rather than a generic parameter deliberately: it is consulted
/// once a minute, so dynamic dispatch is free, and it keeps [`Sampler`] free of
/// a type parameter that would spread into the daemon and every test.
pub trait TimeEstimates {
    /// Seconds until the battery is empty.
    fn time_to_empty_s(&self) -> Option<i64>;
    /// Seconds until the battery is full.
    fn time_to_full_s(&self) -> Option<i64>;
}

/// Estimates from a source that reports nothing, leaving the arithmetic
/// fallback to do the work.
#[derive(Debug, Default)]
pub struct NoTimeEstimates;

impl TimeEstimates for NoTimeEstimates {
    fn time_to_empty_s(&self) -> Option<i64> {
        None
    }
    fn time_to_full_s(&self) -> Option<i64> {
        None
    }
}

/// Anything that can produce a telemetry sample.
///
/// One of the crate's three test seams; it is what lets the daemon loop be
/// driven by a scripted sequence of samples instead of real hardware.
pub trait TelemetrySource {
    fn sample(&mut self) -> Sample;
}

/// Reads every source and assembles a [`Sample`].
pub struct Sampler {
    paths: Paths,
    battery: BatteryReader,
    thermal: ThermalReader,
    proc: ProcReader,
    processes: ProcessReader,
    estimates: Box<dyn TimeEstimates>,

    last_estimate_poll: Option<Instant>,
    last_power_state: Option<PowerState>,
    cached_time_to_empty: Option<i64>,
    cached_time_to_full: Option<i64>,
}

impl std::fmt::Debug for Sampler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sampler")
            .field("battery", &self.battery.dir())
            .field("proc", &self.proc.base())
            .finish_non_exhaustive()
    }
}

impl Sampler {
    /// Build a sampler reading the given locations.
    #[must_use]
    pub fn new(paths: Paths, estimates: Box<dyn TimeEstimates>) -> Self {
        let battery = BatteryReader::new(paths.battery.clone());
        let thermal = ThermalReader::new(paths.battery.clone(), paths.hwmon_base.clone());
        let proc = ProcReader::new(paths.proc_base.clone());
        let processes = ProcessReader::new(paths.proc_base.clone());

        Self {
            paths,
            battery,
            thermal,
            proc,
            processes,
            estimates,
            last_estimate_poll: None,
            last_power_state: None,
            cached_time_to_empty: None,
            cached_time_to_full: None,
        }
    }

    /// A sampler with no external estimate source.
    #[must_use]
    pub fn without_estimates(paths: Paths) -> Self {
        Self::new(paths, Box::new(NoTimeEstimates))
    }

    /// Refresh the cached runtime estimates if they are stale or the rail changed.
    ///
    /// A change of power state invalidates immediately regardless of age: an
    /// estimate of time-to-empty is meaningless the instant a charger is plugged
    /// in, and showing a stale one for up to a minute would be worse than none.
    fn refresh_estimates(&mut self, power_state: PowerState) {
        let expired = self
            .last_estimate_poll
            .is_none_or(|polled| polled.elapsed() >= ESTIMATE_TTL);

        if self.last_power_state != Some(power_state) || expired {
            self.last_estimate_poll = Some(Instant::now());
            self.last_power_state = Some(power_state);
            self.cached_time_to_empty = (power_state == PowerState::Discharging)
                .then(|| self.estimates.time_to_empty_s())
                .flatten();
            self.cached_time_to_full = power_state
                .is_charging()
                .then(|| self.estimates.time_to_full_s())
                .flatten();
        }
    }
}

impl TelemetrySource for Sampler {
    /// Read everything and assemble one sample.
    ///
    /// `estimated_cycle_count` is left at zero: it is a function of the previous
    /// stored sample, which only the store knows, and is filled in on insert.
    fn sample(&mut self) -> Sample {
        let status = self.battery.status();
        let power_state = PowerState::from_status(&status);
        let is_charging = power_state.is_charging();

        let energy = self.battery.energy();
        let power_w = self.battery.power_w();
        let thermals = self.thermal.read();

        self.refresh_estimates(power_state);

        // Where UPower says nothing, fall back to dividing remaining energy by
        // the present rate. That is a far worse estimate — it assumes the
        // current draw continues unchanged — but it is better than a blank
        // column in a forensic record.
        let mut time_to_empty_s = self.cached_time_to_empty;
        let mut time_to_full_s = self.cached_time_to_full;

        if time_to_empty_s.is_none()
            && power_state == PowerState::Discharging
            && power_w > MIN_RATE_FOR_ESTIMATE_W
        {
            time_to_empty_s = Some(round_js(energy.now_wh / power_w * 3_600.0) as i64);
        }
        if time_to_full_s.is_none() && is_charging && power_w > MIN_RATE_FOR_ESTIMATE_W {
            time_to_full_s =
                Some(round_js((energy.full_wh - energy.now_wh) / power_w * 3_600.0) as i64);
        }

        // /proc/stat is read once and its totals shared. The TypeScript parsed
        // it separately for utilisation and for the process scan.
        let cpu_times = self.proc.cpu_times();
        let cpu_pct = cpu_times.and_then(|times| self.proc.cpu_pct(times));
        // Both are required: process memory is a percentage of total, and
        // without a denominator the ranking is omitted rather than invented.
        let top_processes = cpu_times
            .zip(self.proc.mem_total_kb())
            .and_then(|(times, mem_total_kb)| self.processes.read(times.total, mem_total_kb));

        Sample {
            ts: now_iso8601_millis(),
            charge_pct: self.battery.charge_pct(),
            status,
            power_state,
            energy_wh: round_to(energy.now_wh, 3),
            energy_full_wh: round_to(energy.full_wh, 3),
            energy_design_wh: round_to(energy.design_wh, 3),
            power_w: round_to(power_w, 3),
            // Voltages are stored unrounded, unlike energy and power. Preserved
            // from the original rather than normalised, so stored rows match.
            voltage_v: self.battery.voltage_v(),
            voltage_design_v: self.battery.voltage_design_v(),
            cycle_count: self.battery.cycle_count(),
            estimated_cycle_count: 0.0,
            battery_temp_c: thermals.battery_c,
            health_pct: energy.health_pct(),
            is_charging,
            is_present: self.battery.is_present(),
            time_to_empty_s,
            time_to_full_s,
            cpu_temp_c: thermals.cpu_c,
            gpu_temp_c: thermals.gpu_c,
            nvme_temp_c: thermals.nvme_c,
            cpu_pct,
            mem_pct: self.proc.mem_pct(),
            top_processes,
            cpu_freq_mhz: read_cpu_freq_mhz(&self.paths.cpu_base, &self.paths.proc_base),
            gpu_pct: read_gpu_pct(&self.paths.drm_base),
            gpu_power_w: thermals.gpu_power_w,
            load1: self.proc.load1(),
            boot_id: self.proc.boot_id(),
            uptime_s: self.proc.uptime_s(),
        }
    }
}

#[cfg(test)]
mod tests;
