//! Reading the battery's own sysfs attributes.
//!
//! Every attribute here is optional. Which ones a machine exposes depends on the
//! ACPI tables, the driver, and sometimes on whether the pack has completed a
//! full charge since boot — so the reader's job is as much about deciding what a
//! missing or malformed file means as about parsing.

use std::path::{Path, PathBuf};

use crate::formats::parse_number;
use crate::units::MICRO;

/// Energy figures in watt-hours, normalised across the two driver conventions.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Energy {
    /// Energy currently in the pack.
    pub now_wh: f64,
    /// Energy the pack holds at full charge today.
    pub full_wh: f64,
    /// Energy the pack held at full charge when it was new.
    pub design_wh: f64,
}

impl Energy {
    /// Present full-charge capacity as a percentage of design capacity.
    ///
    /// Reports a healthy 100 when design capacity is unknown, rather than a
    /// division by zero or an alarming zero: an unreadable attribute is not
    /// evidence of a worn-out battery, and the health alert must not fire on it.
    #[must_use]
    pub fn health_pct(self) -> f64 {
        if self.design_wh > 0.0 {
            crate::formats::round_half_up((self.full_wh / self.design_wh) * 10_000.0) / 100.0
        } else {
            100.0
        }
    }
}

/// Reads one battery's sysfs directory.
#[derive(Debug, Clone)]
pub struct BatteryReader {
    dir: PathBuf,
}

impl BatteryReader {
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The directory being read.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Trimmed contents of an attribute, or [`None`] if it is missing, empty or
    /// unreadable.
    ///
    /// The three are deliberately not distinguished. sysfs attributes disappear
    /// when hardware is unplugged and return `-ENODEV` when it is mid-removal;
    /// no caller here would do anything different with that detail.
    fn read_str(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.dir.join(name))
            .ok()
            .map(|contents| contents.trim().to_owned())
            .filter(|trimmed| !trimmed.is_empty())
    }

    /// A numeric attribute, or [`None`] when absent or unparseable.
    fn read_opt(&self, name: &str) -> Option<f64> {
        self.read_str(name).and_then(|value| parse_number(&value))
    }

    fn has(&self, name: &str) -> bool {
        self.dir.join(name).exists()
    }

    /// Raw kernel status string, or `Unknown`.
    #[must_use]
    pub fn status(&self) -> String {
        self.read_str("status")
            .unwrap_or_else(|| "Unknown".to_owned())
    }

    /// State of charge, percent, or [`None`] when it cannot be determined.
    ///
    /// `capacity` is what the kernel computes and the one to prefer. When it is
    /// unreadable the same figure is derived from the energy pair, which costs
    /// nothing and keeps the daemon useful on a driver that omits it.
    ///
    /// Returning [`None`] rather than zero is the point: zero is a *valid*
    /// state of charge, and the charge ladder acts on it. An unreadable
    /// attribute reported as zero announces a critical low battery on a pack
    /// that may be full.
    #[must_use]
    pub fn charge_pct(&self) -> Option<f64> {
        if let Some(capacity) = self.read_opt("capacity") {
            return Some(capacity);
        }
        let energy = self.energy()?;
        (energy.full_wh > 0.0).then(|| (energy.now_wh / energy.full_wh) * 100.0)
    }

    /// Hardware cycle count, if the management system reports one.
    #[must_use]
    pub fn cycle_count(&self) -> Option<i64> {
        self.read_opt("cycle_count").map(|count| count as i64)
    }

    /// Instantaneous rail voltage, or zero when unreadable.
    ///
    /// Zero is safe to fabricate *here specifically* because the only consumer,
    /// the over-voltage check, treats a non-positive reading as "no information"
    /// and neither fires nor clears on it. Do not copy this pattern to a field
    /// whose consumers act on zero.
    #[must_use]
    pub fn voltage_v(&self) -> f64 {
        self.read_opt("voltage_now").unwrap_or(0.0) / MICRO
    }

    /// Design voltage, falling back to the present reading.
    ///
    /// `voltage_min_design` is the nominal figure and the one to prefer, but
    /// plenty of drivers omit it. The present voltage is a poor substitute — it
    /// makes the over-voltage check compare a value against itself and so never
    /// fire — but it is better than zero, which would disable the check by a
    /// different route while also corrupting charge-based energy conversion.
    #[must_use]
    pub fn voltage_design_v(&self) -> f64 {
        self.read_opt("voltage_min_design")
            .or_else(|| self.read_opt("voltage_now"))
            .unwrap_or(0.0)
            / MICRO
    }

    /// Energy figures, auto-detecting which unit the driver reports.
    ///
    /// Drivers expose either `energy_*` in microwatt-hours or `charge_*` in
    /// microamp-hours, never both, and which one depends on the hardware's fuel
    /// gauge. Charge-based readings are converted through design voltage, which
    /// is an approximation — the real terminal voltage sags under load — but it
    /// is the only conversion available, and it is what the kernel's own
    /// consumers do.
    /// [`None`] when the present or full reading is missing, since neither the
    /// state of charge nor the cycle integral means anything without them.
    ///
    /// `design` is separate: plenty of drivers omit it permanently, so it falls
    /// back to zero, which every consumer already reads as "unknown" — health
    /// reports 100% and the cycle integrator declines to integrate rather than
    /// dividing by it.
    #[must_use]
    pub fn energy(&self) -> Option<Energy> {
        if self.has("energy_now") {
            return Some(Energy {
                now_wh: self.read_opt("energy_now")? / MICRO,
                full_wh: self.read_opt("energy_full")? / MICRO,
                design_wh: self.read_opt("energy_full_design").unwrap_or(0.0) / MICRO,
            });
        }

        let design_volts = self.voltage_design_v();
        Some(Energy {
            now_wh: (self.read_opt("charge_now")? / MICRO) * design_volts,
            full_wh: (self.read_opt("charge_full")? / MICRO) * design_volts,
            design_wh: (self.read_opt("charge_full_design").unwrap_or(0.0) / MICRO) * design_volts,
        })
    }

    /// Charge or discharge rate in watts.
    ///
    /// `power_now` when the driver offers it; otherwise the product of current
    /// and voltage, which is the same quantity the hardware would have computed.
    /// Zero when neither is available — an unknown rate is not a negative one.
    #[must_use]
    pub fn power_w(&self) -> f64 {
        if let Some(watts) = self.read_opt("power_now") {
            return watts / MICRO;
        }

        match (self.read_opt("current_now"), self.read_opt("voltage_now")) {
            (Some(amps), Some(volts)) => (amps * volts) / (MICRO * MICRO),
            _ => 0.0,
        }
    }

    /// Whether a battery is physically installed.
    ///
    /// A missing directory means no battery. A present directory with no
    /// `present` attribute means yes — most laptop drivers simply do not expose
    /// the attribute for a permanently installed pack, and treating its absence
    /// as "no battery" would silence the entire daemon on that hardware.
    #[must_use]
    pub fn is_present(&self) -> bool {
        if !self.dir.exists() {
            return false;
        }
        if self.has("present") {
            return self.read_str("present").is_some_and(|value| value == "1");
        }
        true
    }
}

#[cfg(test)]
mod tests;
