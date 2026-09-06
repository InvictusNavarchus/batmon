//! Tunable constants: what counts as too hot, too low, too often.
//!
//! These are the values a user would most plausibly want to change, so they live
//! in owned structs with a [`Default`] impl rather than as free constants. That
//! costs nothing today and is the entire implementation of a future config file
//! — loading one becomes a deserialize over the same struct.

use std::time::Duration;

/// One or more thresholds are internally inconsistent.
#[derive(Debug, thiserror::Error)]
#[error("invalid threshold configuration:\n{0}")]
pub struct InvalidThresholds(String);

/// Alert firing points and the deadbands that stop them flapping.
///
/// Every threshold is paired with a hysteresis band. A sensor sitting exactly on
/// a limit dithers by fractions of a degree many times a second, and a bare
/// comparison would turn that into a notification storm. The band is the width
/// the reading must travel back through before the alert re-arms.
#[derive(Debug, Clone, PartialEq)]
pub struct Thresholds {
    // ── battery charge ───────────────────────────────────────────────
    /// Unplug reminder, percent.
    pub charge_high_warn: f64,
    /// Plug-in reminder, percent.
    pub charge_low_warn: f64,
    /// Critical low battery, percent.
    pub charge_crit_warn: f64,
    /// Band the charge level must cross back through to re-arm.
    pub charge_hysteresis_pct: f64,

    // ── battery temperature ──────────────────────────────────────────
    /// High battery temperature warning, °C.
    pub temp_warn: f64,
    /// Critical battery temperature, °C.
    pub temp_crit: f64,
    /// Band the battery temperature must fall through to re-arm, °C.
    pub temp_hysteresis_c: f64,

    // ── battery health ───────────────────────────────────────────────
    /// Wear warning, as a percentage of design capacity.
    pub cap_warn: f64,
    /// Band the health figure must recover through to re-arm.
    pub cap_hysteresis_pct: f64,

    // ── heat-soak while charging ─────────────────────────────────────
    /// CPU temperature that makes charging inadvisable, °C.
    pub cpu_hot_charging: f64,
    /// Band the CPU temperature must fall through to re-arm, °C.
    pub cpu_temp_hysteresis_c: f64,
    /// Consecutive qualifying samples before the alert trips.
    pub cpu_heat_debounce_samples: u32,

    // ── thermal anomaly while idle ───────────────────────────────────
    /// CPU temperature that is abnormal *for a low workload*, °C.
    pub cpu_anomaly_temp: f64,
    /// Upper bound on CPU utilisation still considered idle, percent.
    pub cpu_anomaly_max_load_pct: f64,
    /// Upper bound on discharge power still considered idle, watts.
    pub cpu_anomaly_max_power_w: f64,
    /// Band the CPU temperature must fall through to re-arm, °C.
    pub cpu_anomaly_hysteresis_c: f64,
    /// Consecutive qualifying samples before the alert trips.
    pub cpu_anomaly_debounce_samples: u32,

    // ── charging voltage ─────────────────────────────────────────────
    /// Multiple of design voltage that counts as overvoltage.
    pub voltage_over_ratio: f64,
    /// Multiple of design voltage the rail must fall back to, to re-arm.
    pub voltage_clear_ratio: f64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            charge_high_warn: 80.0,
            charge_low_warn: 20.0,
            charge_crit_warn: 10.0,
            charge_hysteresis_pct: 5.0,

            temp_warn: 45.0,
            temp_crit: 50.0,
            temp_hysteresis_c: 3.0,

            cap_warn: 80.0,
            cap_hysteresis_pct: 2.0,

            cpu_hot_charging: 85.0,
            cpu_temp_hysteresis_c: 5.0,
            cpu_heat_debounce_samples: 3,

            cpu_anomaly_temp: 80.0,
            cpu_anomaly_max_load_pct: 20.0,
            cpu_anomaly_max_power_w: 12.0,
            cpu_anomaly_hysteresis_c: 5.0,
            cpu_anomaly_debounce_samples: 3,

            voltage_over_ratio: 1.15,
            voltage_clear_ratio: 1.10,
        }
    }
}

impl Thresholds {
    /// Check every ordering and range invariant, reporting all violations at once.
    ///
    /// Collecting rather than short-circuiting is deliberate: when this is
    /// eventually pointed at a user-written config file, being told about one
    /// mistake per edit round-trip is miserable.
    pub fn validate(&self) -> Result<(), InvalidThresholds> {
        let mut problems = Vec::new();

        // Finiteness first. Every check below is a comparison, and every
        // comparison against NaN is false, so a non-finite threshold would sail
        // through them all while silently disabling the alert it governs — no
        // temperature ever reaches an infinite limit.
        self.check_finite(&mut problems);
        self.check_charge(&mut problems);
        self.check_temperature(&mut problems);
        self.check_health_and_workload(&mut problems);
        self.check_bands(&mut problems);

        if problems.is_empty() {
            Ok(())
        } else {
            Err(InvalidThresholds(problems.join("\n")))
        }
    }

    /// Every threshold that must be a real number, paired with its name.
    fn numeric_fields(&self) -> [(&'static str, f64); 17] {
        [
            ("charge_high_warn", self.charge_high_warn),
            ("charge_low_warn", self.charge_low_warn),
            ("charge_crit_warn", self.charge_crit_warn),
            ("charge_hysteresis_pct", self.charge_hysteresis_pct),
            ("temp_warn", self.temp_warn),
            ("temp_crit", self.temp_crit),
            ("temp_hysteresis_c", self.temp_hysteresis_c),
            ("cap_warn", self.cap_warn),
            ("cap_hysteresis_pct", self.cap_hysteresis_pct),
            ("cpu_hot_charging", self.cpu_hot_charging),
            ("cpu_temp_hysteresis_c", self.cpu_temp_hysteresis_c),
            ("cpu_anomaly_temp", self.cpu_anomaly_temp),
            ("cpu_anomaly_max_load_pct", self.cpu_anomaly_max_load_pct),
            ("cpu_anomaly_max_power_w", self.cpu_anomaly_max_power_w),
            ("cpu_anomaly_hysteresis_c", self.cpu_anomaly_hysteresis_c),
            ("voltage_over_ratio", self.voltage_over_ratio),
            ("voltage_clear_ratio", self.voltage_clear_ratio),
        ]
    }

    fn check_finite(&self, problems: &mut Vec<String>) {
        for (name, value) in self.numeric_fields() {
            if !value.is_finite() {
                problems.push(format!("  - {name} must be a finite number"));
            }
        }
    }

    fn check_charge(&self, problems: &mut Vec<String>) {
        let mut require = |ok: bool, message: &str| require_into(problems, ok, message);

        require(
            self.charge_crit_warn > 0.0,
            "charge_crit_warn must be above 0%",
        );
        require(
            self.charge_low_warn > self.charge_crit_warn,
            "charge_low_warn must be above charge_crit_warn",
        );
        require(
            self.charge_high_warn > self.charge_low_warn,
            "charge_high_warn must be above charge_low_warn",
        );
        require(
            self.charge_high_warn <= 100.0,
            "charge_high_warn must not exceed 100%",
        );
        // Without this the unplug reminder's re-arm band would reach down past
        // the plug-in reminder and the two would trade notifications forever.
        require(
            self.charge_high_warn - self.charge_hysteresis_pct > self.charge_low_warn,
            "charge_high_warn minus charge_hysteresis_pct must stay above charge_low_warn",
        );
    }

    fn check_temperature(&self, problems: &mut Vec<String>) {
        let mut require = |ok: bool, message: &str| require_into(problems, ok, message);

        require(self.temp_warn > 0.0, "temp_warn must be above 0 C");
        require(
            self.temp_crit > self.temp_warn,
            "temp_crit must be above temp_warn",
        );
        require(
            self.cpu_hot_charging > self.temp_crit,
            "cpu_hot_charging must be above temp_crit",
        );
        // Same overlap argument as the charge ladder: the critical re-arm band
        // must not reach down into the warning's firing range.
        require(
            self.temp_crit - self.temp_hysteresis_c > self.temp_warn,
            "temp_crit minus temp_hysteresis_c must stay above temp_warn",
        );
        // Both CPU families clear at threshold minus band. A band wider than its
        // threshold puts that point below absolute zero, where no reading can
        // reach it, so a latch that fires could never re-arm.
        require(
            self.cpu_temp_hysteresis_c < self.cpu_hot_charging,
            "cpu_temp_hysteresis_c must be narrower than cpu_hot_charging",
        );
        require(
            self.cpu_anomaly_hysteresis_c < self.cpu_anomaly_temp,
            "cpu_anomaly_hysteresis_c must be narrower than cpu_anomaly_temp",
        );
    }

    fn check_health_and_workload(&self, problems: &mut Vec<String>) {
        let mut require = |ok: bool, message: &str| require_into(problems, ok, message);

        require(
            self.cap_warn > 0.0 && self.cap_warn <= 100.0,
            "cap_warn must be a percentage above 0",
        );
        // Health is a percentage of design capacity, so a re-arm point above 100
        // is unreachable and the notice would latch permanently after firing.
        require(
            self.cap_warn + self.cap_hysteresis_pct <= 100.0,
            "cap_warn plus cap_hysteresis_pct must not exceed 100%",
        );
        require(
            self.cpu_anomaly_temp > 0.0,
            "cpu_anomaly_temp must be above 0 C",
        );
        require(
            self.cpu_anomaly_max_load_pct > 0.0 && self.cpu_anomaly_max_load_pct <= 100.0,
            "cpu_anomaly_max_load_pct must be a percentage above 0",
        );
        require(
            self.cpu_anomaly_max_power_w > 0.0,
            "cpu_anomaly_max_power_w must be above 0 W",
        );
    }

    fn check_bands(&self, problems: &mut Vec<String>) {
        for (name, band) in [
            ("charge_hysteresis_pct", self.charge_hysteresis_pct),
            ("temp_hysteresis_c", self.temp_hysteresis_c),
            ("cap_hysteresis_pct", self.cap_hysteresis_pct),
            ("cpu_temp_hysteresis_c", self.cpu_temp_hysteresis_c),
            ("cpu_anomaly_hysteresis_c", self.cpu_anomaly_hysteresis_c),
        ] {
            require_into(problems, band > 0.0, &format!("{name} must be above 0"));
        }

        for (name, samples) in [
            ("cpu_heat_debounce_samples", self.cpu_heat_debounce_samples),
            (
                "cpu_anomaly_debounce_samples",
                self.cpu_anomaly_debounce_samples,
            ),
        ] {
            require_into(
                problems,
                samples >= 1,
                &format!("{name} must be at least 1"),
            );
        }

        require_into(
            problems,
            self.voltage_over_ratio > self.voltage_clear_ratio,
            "voltage_over_ratio must be above voltage_clear_ratio",
        );
        require_into(
            problems,
            self.voltage_clear_ratio >= 1.0,
            "voltage_clear_ratio must be at least 1.0 (design voltage)",
        );
    }
}

/// Record a violation if the invariant does not hold.
fn require_into(problems: &mut Vec<String>, holds: bool, message: &str) {
    if !holds {
        problems.push(format!("  - {message}"));
    }
}

/// Sampling cadence and retention.
///
/// The two tiers are expressed as one interval plus tick multiples rather than
/// three independent timers, because a single deadline-driven loop is what keeps
/// the daemon's wakeup count — and therefore its power draw — predictable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule {
    /// Flight recorder period. Every tick writes one row to `debug.db`.
    pub sample_interval: Duration,
    /// Ticks between rows written to the permanent `battery.db`.
    pub historical_interval_ticks: u64,
    /// Ticks between prune sweeps of `debug.db`.
    pub prune_interval_ticks: u64,
    /// Rolling window `debug.db` retains.
    pub debug_retention_hours: i64,
}

impl Default for Schedule {
    fn default() -> Self {
        Self {
            sample_interval: Duration::from_secs(1),
            historical_interval_ticks: 60,
            prune_interval_ticks: 300,
            debug_retention_hours: 6,
        }
    }
}

impl Schedule {
    /// Check that every interval is non-degenerate.
    pub fn validate(&self) -> Result<(), InvalidThresholds> {
        let mut problems = Vec::new();
        if self.sample_interval.is_zero() {
            problems.push("  - sample_interval must be above zero".to_owned());
        }
        if self.historical_interval_ticks == 0 {
            problems.push("  - historical_interval_ticks must be at least 1".to_owned());
        }
        if self.prune_interval_ticks == 0 {
            problems.push("  - prune_interval_ticks must be at least 1".to_owned());
        }
        if self.debug_retention_hours <= 0 {
            problems.push("  - debug_retention_hours must be above zero".to_owned());
        }

        if problems.is_empty() {
            Ok(())
        } else {
            Err(InvalidThresholds(problems.join("\n")))
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::float_cmp)]

    use super::*;

    #[test]
    fn defaults_match_the_documented_specification() {
        let t = Thresholds::default();

        assert_eq!(t.charge_crit_warn, 10.0);
        assert_eq!(t.charge_low_warn, 20.0);
        assert_eq!(t.charge_high_warn, 80.0);
        assert_eq!(t.charge_hysteresis_pct, 5.0);

        assert_eq!(t.temp_warn, 45.0);
        assert_eq!(t.temp_crit, 50.0);
        assert_eq!(t.temp_hysteresis_c, 3.0);

        assert_eq!(t.cap_warn, 80.0);
        assert_eq!(t.cap_hysteresis_pct, 2.0);

        assert_eq!(t.cpu_hot_charging, 85.0);
        assert_eq!(t.cpu_temp_hysteresis_c, 5.0);
        assert_eq!(t.cpu_heat_debounce_samples, 3);

        assert_eq!(t.cpu_anomaly_temp, 80.0);
        assert_eq!(t.cpu_anomaly_max_load_pct, 20.0);
        assert_eq!(t.cpu_anomaly_max_power_w, 12.0);
        assert_eq!(t.cpu_anomaly_hysteresis_c, 5.0);
        assert_eq!(t.cpu_anomaly_debounce_samples, 3);

        assert_eq!(t.voltage_over_ratio, 1.15);
        assert_eq!(t.voltage_clear_ratio, 1.10);
    }

    #[test]
    fn defaults_are_self_consistent() {
        // Replaces roughly thirty hand-written ordering assertions.
        Thresholds::default().validate().unwrap();
        Schedule::default().validate().unwrap();
    }

    #[test]
    fn schedule_defaults_match_the_documented_cadence() {
        let s = Schedule::default();
        assert_eq!(s.sample_interval, Duration::from_secs(1));
        assert_eq!(s.historical_interval_ticks, 60);
        assert_eq!(s.prune_interval_ticks, 300);
        assert_eq!(s.debug_retention_hours, 6);
    }

    #[test]
    fn rejects_an_inverted_charge_ladder() {
        let t = Thresholds {
            charge_low_warn: 5.0, // below crit
            ..Default::default()
        };
        let err = t.validate().unwrap_err().to_string();
        assert!(
            err.contains("charge_low_warn must be above charge_crit_warn"),
            "{err}"
        );
    }

    #[test]
    fn rejects_a_charge_deadband_that_overlaps_the_low_warning() {
        let t = Thresholds {
            charge_hysteresis_pct: 65.0,
            ..Default::default()
        };
        let err = t.validate().unwrap_err().to_string();
        assert!(
            err.contains("charge_hysteresis_pct must stay above"),
            "{err}"
        );
    }

    #[test]
    fn rejects_a_thermal_deadband_that_overlaps_the_warning() {
        let t = Thresholds {
            temp_hysteresis_c: 6.0, // 50 - 6 = 44, below the 45 warn point
            ..Default::default()
        };
        let err = t.validate().unwrap_err().to_string();
        assert!(err.contains("temp_crit minus temp_hysteresis_c"), "{err}");
    }

    #[test]
    fn rejects_inverted_temperature_thresholds() {
        let t = Thresholds {
            temp_crit: 40.0,
            ..Default::default()
        };
        let err = t.validate().unwrap_err().to_string();
        assert!(err.contains("temp_crit must be above temp_warn"), "{err}");
    }

    #[test]
    fn rejects_non_positive_hysteresis_bands() {
        let t = Thresholds {
            cap_hysteresis_pct: 0.0,
            ..Default::default()
        };
        let err = t.validate().unwrap_err().to_string();
        assert!(err.contains("cap_hysteresis_pct must be above 0"), "{err}");
    }

    #[test]
    fn rejects_a_zero_debounce_which_would_defeat_the_filter() {
        let t = Thresholds {
            cpu_heat_debounce_samples: 0,
            ..Default::default()
        };
        let err = t.validate().unwrap_err().to_string();
        assert!(
            err.contains("cpu_heat_debounce_samples must be at least 1"),
            "{err}"
        );
    }

    #[test]
    fn rejects_voltage_ratios_that_would_latch_permanently() {
        let t = Thresholds {
            voltage_clear_ratio: 1.20, // above the over-voltage trip point
            ..Default::default()
        };
        let err = t.validate().unwrap_err().to_string();
        assert!(err.contains("voltage_over_ratio must be above"), "{err}");
    }

    #[test]
    fn rejects_a_clear_ratio_below_design_voltage() {
        let t = Thresholds {
            voltage_over_ratio: 0.9,
            voltage_clear_ratio: 0.8,
            ..Default::default()
        };
        let err = t.validate().unwrap_err().to_string();
        assert!(
            err.contains("voltage_clear_ratio must be at least 1.0"),
            "{err}"
        );
    }

    #[test]
    fn rejects_non_finite_thresholds() {
        // Every ordering check is a comparison, and comparisons against NaN are
        // false — so without this an infinite limit passes validation and
        // silently disables its alert.
        for (label, thresholds) in [
            (
                "infinite",
                Thresholds {
                    cpu_hot_charging: f64::INFINITY,
                    ..Default::default()
                },
            ),
            (
                "nan",
                Thresholds {
                    voltage_over_ratio: f64::NAN,
                    ..Default::default()
                },
            ),
        ] {
            let err = thresholds.validate().unwrap_err().to_string();
            assert!(err.contains("must be a finite number"), "{label}: {err}");
        }
    }

    #[test]
    fn rejects_a_health_rearm_point_above_one_hundred_percent() {
        let t = Thresholds {
            cap_warn: 99.0,
            cap_hysteresis_pct: 5.0,
            ..Default::default()
        };
        let err = t.validate().unwrap_err().to_string();
        assert!(err.contains("must not exceed 100%"), "{err}");
    }

    #[test]
    fn rejects_deadbands_wider_than_the_threshold_they_clear() {
        // 85 - 90 is below absolute zero: the latch could fire and never re-arm
        // for any physically possible CPU temperature.
        let t = Thresholds {
            cpu_temp_hysteresis_c: 90.0,
            ..Default::default()
        };
        let err = t.validate().unwrap_err().to_string();
        assert!(
            err.contains("cpu_temp_hysteresis_c must be narrower"),
            "{err}"
        );

        let t = Thresholds {
            cpu_anomaly_hysteresis_c: 95.0,
            ..Default::default()
        };
        let err = t.validate().unwrap_err().to_string();
        assert!(
            err.contains("cpu_anomaly_hysteresis_c must be narrower"),
            "{err}"
        );
    }

    #[test]
    fn reports_every_violation_rather_than_only_the_first() {
        let t = Thresholds {
            temp_crit: 10.0,
            cap_hysteresis_pct: -1.0,
            cpu_anomaly_debounce_samples: 0,
            ..Default::default()
        };
        let err = t.validate().unwrap_err().to_string();

        assert!(err.contains("temp_crit must be above temp_warn"), "{err}");
        assert!(err.contains("cap_hysteresis_pct must be above 0"), "{err}");
        assert!(
            err.contains("cpu_anomaly_debounce_samples must be at least 1"),
            "{err}"
        );
    }

    #[test]
    fn rejects_degenerate_schedules() {
        let err = Schedule {
            sample_interval: Duration::ZERO,
            historical_interval_ticks: 0,
            prune_interval_ticks: 0,
            debug_retention_hours: 0,
        }
        .validate()
        .unwrap_err()
        .to_string();

        assert!(err.contains("sample_interval"), "{err}");
        assert!(err.contains("historical_interval_ticks"), "{err}");
        assert!(err.contains("prune_interval_ticks"), "{err}");
        assert!(err.contains("debug_retention_hours"), "{err}");
    }
}
