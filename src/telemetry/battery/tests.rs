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

    let energy = reader.energy().expect("a complete energy driver reads");
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

    let energy = reader.energy().expect("a complete charge driver reads");
    assert!((energy.now_wh - 46.2).abs() < 1e-9);
    assert!((energy.full_wh - 57.75).abs() < 1e-9);
    assert!((energy.design_wh - 63.525).abs() < 1e-9);
}

#[test]
fn a_charge_driver_without_design_voltage_falls_back_to_the_present_one() {
    let (_tmp, reader) = battery(&[
        ("charge_now", "4000000"),
        ("charge_full", "4000000"),
        ("voltage_now", "12000000"),
    ]);

    let energy = reader
        .energy()
        .expect("charge_now and charge_full are present");
    assert!((energy.now_wh - 48.0).abs() < 1e-9);
}

#[test]
fn energy_attributes_win_over_charge_attributes_when_both_exist() {
    let (_tmp, reader) = battery(&[
        ("energy_now", "48600000"),
        ("energy_full", "53000000"),
        ("charge_now", "4000000"),
        ("voltage_min_design", "11550000"),
    ]);

    let energy = reader.energy().expect("the energy pair is present");
    assert_eq!(energy.now_wh, 48.6);
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
    assert_eq!(reader.charge_pct(), Some(94.0));
}

#[test]
fn a_whitespace_only_attribute_reads_as_absent() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("capacity"), "   \n").unwrap();
    std::fs::write(tmp.path().join("status"), "\n").unwrap();
    let reader = BatteryReader::new(tmp.path());

    assert_eq!(reader.charge_pct(), None);
    assert_eq!(reader.status(), "Unknown");
}

#[test]
fn a_malformed_numeric_attribute_reads_as_absent_rather_than_zero() {
    let (_tmp, reader) = battery(&[("capacity", "not-a-number"), ("cycle_count", "??")]);

    // None, not 0.0: zero is a real state of charge and the charge ladder
    // announces a critical low battery on it.
    assert_eq!(reader.charge_pct(), None);
    assert_eq!(reader.cycle_count(), None);
}

#[test]
fn a_missing_battery_directory_reports_not_present() {
    let reader = BatteryReader::new("/nonexistent/batmon/BAT0");

    assert_eq!(reader.is_present(), Some(false));
    assert_eq!(reader.charge_pct(), None);
    assert_eq!(reader.status(), "Unknown");
}

#[test]
fn a_directory_without_a_present_attribute_counts_as_present() {
    // Most laptop drivers omit it for a permanently installed pack, and
    // reading its absence as "no battery" would silence the whole daemon.
    let (_tmp, reader) = battery(&[("capacity", "80")]);
    assert_eq!(reader.is_present(), Some(true));
}

#[test]
fn a_present_attribute_is_obeyed_in_both_directions() {
    let (_tmp, installed) = battery(&[("present", "1")]);
    assert_eq!(installed.is_present(), Some(true));

    let (_tmp, removed) = battery(&[("present", "0")]);
    assert_eq!(removed.is_present(), Some(false));
}

#[test]
fn an_unreadable_present_attribute_is_unknown_rather_than_absent() {
    // Absence clears every alert latch, so it must be a reading rather than a
    // failure to read: an empty `present` file says nothing about the pack.
    let (_tmp, reader) = battery(&[("present", ""), ("capacity", "80")]);

    assert_eq!(reader.is_present(), None);
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

#[test]
fn an_unreadable_capacity_is_derived_from_the_energy_pair() {
    // Some drivers omit `capacity`. Deriving it costs nothing and keeps the
    // daemon useful there, rather than discarding an otherwise good sample.
    let (_tmp, reader) = battery(&[("energy_now", "29000000"), ("energy_full", "58000000")]);

    assert_eq!(reader.charge_pct(), Some(50.0));
}

#[test]
fn an_incomplete_energy_pair_reads_as_absent() {
    // energy_now alone cannot yield health or a cycle increment, and reporting
    // the missing half as zero fires both the health and charge alerts.
    let (_tmp, reader) = battery(&[("energy_now", "29000000")]);

    assert_eq!(reader.energy(), None);
    assert_eq!(reader.charge_pct(), None);
}
