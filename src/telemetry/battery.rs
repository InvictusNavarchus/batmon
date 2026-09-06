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
mod tests {
    #![allow(clippy::float_cmp)]

    use super::*;
    use tempfile::TempDir;

    /// A battery directory populated with the given attributes.
    fn battery(attributes: &[(&str, &str)]) -> (TempDir, BatteryReader) {
        let tmp = TempDir::new().unwrap();
        for (name, value) in attributes {
            std::fs::write(tmp.path().join(name), format!("{value}\n")).unwrap();
        }
        let reader = BatteryReader::new(tmp.path());
        (tmp, reader)
    }

    #[test]
    fn reads_an_energy_reporting_driver() {
        let (_tmp, reader) = battery(&[
            ("energy_now", "48600000"),
            ("energy_full", "53000000"),
            ("energy_full_design", "58328000"),
        ]);

        let energy = reader.energy();
        assert_eq!(energy.now_wh, 48.6);
        assert_eq!(energy.full_wh, 53.0);
        assert_eq!(energy.design_wh, 58.328);
    }

    #[test]
    fn reads_a_charge_reporting_driver_through_design_voltage() {
        let (_tmp, reader) = battery(&[
            ("charge_now", "4000000"),
            ("charge_full", "5000000"),
            ("charge_full_design", "5500000"),
            ("voltage_min_design", "11550000"),
        ]);

        let energy = reader.energy();
        assert!((energy.now_wh - 46.2).abs() < 1e-9);
        assert!((energy.full_wh - 57.75).abs() < 1e-9);
        assert!((energy.design_wh - 63.525).abs() < 1e-9);
    }

    #[test]
    fn a_charge_driver_without_design_voltage_falls_back_to_the_present_one() {
        let (_tmp, reader) = battery(&[("charge_now", "4000000"), ("voltage_now", "12000000")]);

        assert!((reader.energy().now_wh - 48.0).abs() < 1e-9);
    }

    #[test]
    fn energy_attributes_win_over_charge_attributes_when_both_exist() {
        let (_tmp, reader) = battery(&[
            ("energy_now", "48600000"),
            ("charge_now", "4000000"),
            ("voltage_min_design", "11550000"),
        ]);

        assert_eq!(reader.energy().now_wh, 48.6);
    }

    #[test]
    fn health_is_full_capacity_against_design_capacity() {
        let energy = Energy {
            now_wh: 30.0,
            full_wh: 48.6,
            design_wh: 53.0,
        };
        assert_eq!(energy.health_pct(), 91.7);
    }

    #[test]
    fn health_reports_healthy_when_design_capacity_is_unknown() {
        // An unreadable attribute is not evidence of a worn battery, and the
        // health alert must not fire on it.
        let energy = Energy {
            now_wh: 30.0,
            full_wh: 48.6,
            design_wh: 0.0,
        };
        assert_eq!(energy.health_pct(), 100.0);
    }

    #[test]
    fn power_prefers_the_direct_reading() {
        let (_tmp, reader) = battery(&[
            ("power_now", "22806000"),
            ("current_now", "1000000"),
            ("voltage_now", "12000000"),
        ]);

        assert_eq!(reader.power_w(), 22.806);
    }

    #[test]
    fn power_falls_back_to_current_times_voltage() {
        let (_tmp, reader) = battery(&[("current_now", "1500000"), ("voltage_now", "12000000")]);

        assert_eq!(reader.power_w(), 18.0);
    }

    #[test]
    fn power_is_zero_when_neither_source_is_available() {
        let (_tmp, reader) = battery(&[("capacity", "80")]);
        assert_eq!(reader.power_w(), 0.0);
    }

    #[test]
    fn power_is_zero_when_only_one_half_of_the_fallback_exists() {
        let (_tmp, reader) = battery(&[("current_now", "1500000")]);
        assert_eq!(reader.power_w(), 0.0);
    }

    #[test]
    fn status_defaults_to_unknown_when_the_attribute_is_missing() {
        let (_tmp, reader) = battery(&[("capacity", "80")]);
        assert_eq!(reader.status(), "Unknown");
    }

    #[test]
    fn attributes_are_trimmed_of_the_trailing_newline() {
        let (_tmp, reader) = battery(&[("status", "Discharging"), ("capacity", "94")]);

        assert_eq!(reader.status(), "Discharging");
        assert_eq!(reader.charge_pct(), 94.0);
    }

    #[test]
    fn a_whitespace_only_attribute_reads_as_absent() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("capacity"), "   \n").unwrap();
        std::fs::write(tmp.path().join("status"), "\n").unwrap();
        let reader = BatteryReader::new(tmp.path());

        assert_eq!(reader.charge_pct(), 0.0);
        assert_eq!(reader.status(), "Unknown");
    }

    #[test]
    fn a_malformed_numeric_attribute_falls_back_rather_than_failing() {
        let (_tmp, reader) = battery(&[("capacity", "not-a-number"), ("cycle_count", "??")]);

        assert_eq!(reader.charge_pct(), 0.0);
        assert_eq!(reader.cycle_count(), None);
    }

    #[test]
    fn a_missing_battery_directory_reports_not_present() {
        let reader = BatteryReader::new("/nonexistent/batmon/BAT0");

        assert!(!reader.is_present());
        assert_eq!(reader.charge_pct(), 0.0);
        assert_eq!(reader.status(), "Unknown");
    }

    #[test]
    fn a_directory_without_a_present_attribute_counts_as_present() {
        // Most laptop drivers omit it for a permanently installed pack, and
        // reading its absence as "no battery" would silence the whole daemon.
        let (_tmp, reader) = battery(&[("capacity", "80")]);
        assert!(reader.is_present());
    }

    #[test]
    fn a_present_attribute_is_obeyed_in_both_directions() {
        let (_tmp, installed) = battery(&[("present", "1")]);
        assert!(installed.is_present());

        let (_tmp, removed) = battery(&[("present", "0")]);
        assert!(!removed.is_present());
    }

    #[test]
    fn voltage_design_prefers_the_nominal_figure() {
        let (_tmp, reader) = battery(&[
            ("voltage_min_design", "11550000"),
            ("voltage_now", "12524000"),
        ]);

        assert_eq!(reader.voltage_design_v(), 11.55);
        assert_eq!(reader.voltage_v(), 12.524);
    }

    #[test]
    fn voltage_design_falls_back_to_the_present_reading() {
        let (_tmp, reader) = battery(&[("voltage_now", "12524000")]);
        assert_eq!(reader.voltage_design_v(), 12.524);
    }

    #[test]
    fn voltage_design_is_zero_when_nothing_is_reported() {
        let (_tmp, reader) = battery(&[("capacity", "80")]);
        assert_eq!(reader.voltage_design_v(), 0.0);
    }

    #[test]
    fn cycle_count_is_read_when_the_management_system_reports_one() {
        let (_tmp, reader) = battery(&[("cycle_count", "120")]);
        assert_eq!(reader.cycle_count(), Some(120));
    }
}
