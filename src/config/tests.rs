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
