//! Reading the battery's own sysfs attributes.
//!
//! Every attribute here is optional. Which ones a machine exposes depends on the
//! ACPI tables, the driver, and sometimes on whether the pack has completed a
//! full charge since boot — so the reader's job is as much about deciding what a
//! missing or malformed file means as about parsing.

use std::path::{Path, PathBuf};

use crate::parity::js_number;
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
            crate::parity::round_js((self.full_wh / self.design_wh) * 10_000.0) / 100.0
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

    /// A numeric attribute, falling back to zero.
    fn read_num(&self, name: &str) -> f64 {
        self.read_opt(name).unwrap_or(0.0)
    }

    /// A numeric attribute, or [`None`] when absent or unparseable.
    fn read_opt(&self, name: &str) -> Option<f64> {
        self.read_str(name).and_then(|value| js_number(&value))
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

    /// State of charge, percent.
    #[must_use]
    pub fn charge_pct(&self) -> f64 {
        self.read_num("capacity")
    }

    /// Hardware cycle count, if the management system reports one.
    #[must_use]
    pub fn cycle_count(&self) -> Option<i64> {
        self.read_opt("cycle_count").map(|count| count as i64)
    }

    /// Instantaneous rail voltage.
    #[must_use]
    pub fn voltage_v(&self) -> f64 {
        self.read_num("voltage_now") / MICRO
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
    #[must_use]
    pub fn energy(&self) -> Energy {
        if self.has("energy_now") {
            return Energy {
                now_wh: self.read_num("energy_now") / MICRO,
                full_wh: self.read_num("energy_full") / MICRO,
                design_wh: self.read_num("energy_full_design") / MICRO,
            };
        }

        let design_volts = self.voltage_design_v();
        Energy {
            now_wh: (self.read_num("charge_now") / MICRO) * design_volts,
            full_wh: (self.read_num("charge_full") / MICRO) * design_volts,
            design_wh: (self.read_num("charge_full_design") / MICRO) * design_volts,
        }
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
