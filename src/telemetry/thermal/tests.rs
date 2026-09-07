#![allow(clippy::float_cmp)]

use super::*;
use tempfile::TempDir;

/// Create one hwmon device directory with the given files.
fn device(base: &Path, name: &str, files: &[(&str, &str)]) {
    let dir = base.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    for (file, value) in files {
        std::fs::write(dir.join(file), format!("{value}\n")).unwrap();
    }
}

/// A reader over a fresh hwmon tree and an empty battery directory.
fn reader(hwmon: &Path) -> ThermalReader {
    ThermalReader::new(hwmon.join("__no_battery__"), hwmon)
}

#[test]
fn parses_an_amd_machine_with_gpu_power_and_nvme() {
    let tmp = TempDir::new().unwrap();
    device(
        tmp.path(),
        "hwmon0",
        &[("name", "k10temp"), ("temp1_input", "48500")],
    );
    device(
        tmp.path(),
        "hwmon1",
        &[
            ("name", "amdgpu"),
            ("temp1_input", "44000"),
            ("power1_input", "9120000"),
        ],
    );
    device(
        tmp.path(),
        "hwmon2",
        &[("name", "nvme"), ("temp1_input", "41850")],
    );

    let thermals = reader(tmp.path()).read();

    assert_eq!(thermals.cpu_c, Some(48.5));
    assert_eq!(thermals.gpu_c, Some(44.0));
    assert_eq!(thermals.nvme_c, Some(41.9));
    assert_eq!(thermals.gpu_power_w, Some(9.12));
}

#[test]
fn prefers_the_intel_package_sensor_over_individual_cores() {
    // temp1 is Core 0; only the package figure describes what the battery
    // underneath actually experiences.
    let tmp = TempDir::new().unwrap();
    device(
        tmp.path(),
        "hwmon0",
        &[
            ("name", "coretemp"),
            ("temp1_label", "Core 0"),
            ("temp1_input", "95000"),
            ("temp2_label", "Package id 0"),
            ("temp2_input", "62350"),
        ],
    );
    device(
        tmp.path(),
        "hwmon1",
        &[("name", "i915"), ("temp1_input", "45000")],
    );

    let thermals = reader(tmp.path()).read();

    assert_eq!(thermals.cpu_c, Some(62.4));
    assert_eq!(thermals.gpu_c, Some(45.0));
    assert_eq!(thermals.gpu_power_w, None, "only amdgpu reports power");
}

#[test]
fn falls_back_to_the_first_sensor_when_no_package_label_exists() {
    let tmp = TempDir::new().unwrap();
    device(
        tmp.path(),
        "hwmon0",
        &[
            ("name", "coretemp"),
            ("temp1_label", "Core 0"),
            ("temp1_input", "57000"),
        ],
    );

    assert_eq!(reader(tmp.path()).read().cpu_c, Some(57.0));
}

#[test]
fn parses_an_arm_machine_reporting_average_power() {
    let tmp = TempDir::new().unwrap();
    device(
        tmp.path(),
        "hwmon0",
        &[("name", "soc_thermal"), ("temp1_input", "51200")],
    );
    device(
        tmp.path(),
        "hwmon1",
        &[
            ("name", "amdgpu"),
            ("temp1_input", "39000"),
            ("power1_average", "5500000"),
        ],
    );

    let thermals = reader(tmp.path()).read();

    assert_eq!(thermals.cpu_c, Some(51.2));
    assert_eq!(thermals.gpu_power_w, Some(5.5));
}

#[test]
fn a_missing_hwmon_tree_yields_no_readings() {
    let mut reader = ThermalReader::new("/nonexistent/bat", "/nonexistent/hwmon");
    assert_eq!(reader.read(), Thermals::default());
}

#[test]
fn unrecognised_drivers_are_ignored() {
    let tmp = TempDir::new().unwrap();
    device(
        tmp.path(),
        "hwmon0",
        &[("name", "acpitz"), ("temp1_input", "40000")],
    );
    device(
        tmp.path(),
        "hwmon1",
        &[("name", "iwlwifi_1"), ("temp1_input", "50000")],
    );

    assert_eq!(reader(tmp.path()).read(), Thermals::default());
}

#[test]
fn unparseable_sensor_values_are_skipped_rather_than_read_as_zero() {
    let tmp = TempDir::new().unwrap();
    device(
        tmp.path(),
        "hwmon0",
        &[("name", "k10temp"), ("temp1_input", "garbage")],
    );

    assert_eq!(reader(tmp.path()).read().cpu_c, None);
}

#[test]
fn an_empty_sensor_file_reads_as_zero_degrees_unlike_a_battery_attribute() {
    // Faithful to the original, and the two readers genuinely differ here.
    // The hwmon path passes trimmed contents straight to Number(), and
    // Number("") is 0 — finite, and above the plausibility floor. The
    // battery reader discards empty attributes before parsing, so the same
    // file there reads as absent. Verified against the TypeScript, which
    // reports nvme_c: 0 for exactly this input.
    let tmp = TempDir::new().unwrap();
    device(
        tmp.path(),
        "hwmon0",
        &[("name", "nvme"), ("temp1_input", "")],
    );

    assert_eq!(reader(tmp.path()).read().nvme_c, Some(0.0));
}

#[test]
fn zero_watts_and_sub_zero_temperatures_are_real_readings() {
    // A GPU in D3cold genuinely draws 0 W, and -50 °C is the documented
    // floor rather than an error sentinel.
    let tmp = TempDir::new().unwrap();
    device(
        tmp.path(),
        "hwmon0",
        &[
            ("name", "amdgpu"),
            ("temp1_input", "0"),
            ("power1_input", "0"),
        ],
    );
    device(
        tmp.path(),
        "hwmon1",
        &[("name", "k10temp"), ("temp1_input", "-50000")],
    );

    let thermals = reader(tmp.path()).read();

    assert_eq!(thermals.cpu_c, Some(-50.0));
    assert_eq!(thermals.gpu_c, Some(0.0));
    assert_eq!(thermals.gpu_power_w, Some(0.0));
}

#[test]
fn kernel_error_sentinels_are_rejected() {
    let tmp = TempDir::new().unwrap();
    device(
        tmp.path(),
        "hwmon0",
        &[("name", "k10temp"), ("temp1_input", "-273150")],
    );

    assert_eq!(reader(tmp.path()).read().cpu_c, None);
}

#[test]
fn gpu_temperature_and_power_always_describe_the_same_device() {
    // An integrated GPU listed first, a discrete one with power second.
    // Reporting the iGPU's 45 °C beside the dGPU's 25 W would describe a
    // machine that does not exist.
    let tmp = TempDir::new().unwrap();
    device(
        tmp.path(),
        "hwmon0",
        &[("name", "i915"), ("temp1_input", "45000")],
    );
    device(
        tmp.path(),
        "hwmon1",
        &[
            ("name", "amdgpu"),
            ("temp1_input", "58500"),
            ("power1_input", "25000000"),
        ],
    );

    let thermals = reader(tmp.path()).read();

    assert_eq!(thermals.gpu_c, Some(58.5));
    assert_eq!(thermals.gpu_power_w, Some(25.0));
}

#[test]
fn a_suspended_gpu_cannot_donate_power_to_another_gpus_temperature() {
    // The concrete trigger: a second GPU in runtime suspend errors on
    // temp1_input while power1_input still reads. Adopting its power
    // alongside the first device's temperature would report a machine that
    // does not exist.
    let tmp = TempDir::new().unwrap();
    device(
        tmp.path(),
        "hwmon0",
        &[("name", "amdgpu"), ("temp1_input", "42000")],
    );
    device(
        tmp.path(),
        "hwmon1",
        &[("name", "amdgpu"), ("power1_input", "90000000")],
    );

    let thermals = reader(tmp.path()).read();

    assert_eq!(thermals.gpu_c, Some(42.0));
    assert_eq!(
        thermals.gpu_power_w, None,
        "power was adopted from a device whose temperature was not"
    );
}

#[test]
fn a_later_complete_gpu_upgrades_a_partial_first_one() {
    // The first device reports only power; a later one reports both, so the
    // pair should move wholesale rather than staying split.
    let tmp = TempDir::new().unwrap();
    device(
        tmp.path(),
        "hwmon0",
        &[("name", "amdgpu"), ("power1_input", "8000000")],
    );
    device(
        tmp.path(),
        "hwmon1",
        &[
            ("name", "amdgpu"),
            ("temp1_input", "58500"),
            ("power1_input", "25000000"),
        ],
    );

    let thermals = reader(tmp.path()).read();

    assert_eq!(thermals.gpu_c, Some(58.5));
    assert_eq!(thermals.gpu_power_w, Some(25.0));
}

#[test]
fn two_power_reporting_gpus_do_not_get_mixed() {
    let tmp = TempDir::new().unwrap();
    device(
        tmp.path(),
        "hwmon0",
        &[
            ("name", "amdgpu"),
            ("temp1_input", "42000"),
            ("power1_input", "8000000"),
        ],
    );
    device(
        tmp.path(),
        "hwmon1",
        &[
            ("name", "amdgpu"),
            ("temp1_input", "82000"),
            ("power1_input", "90000000"),
        ],
    );

    let thermals = reader(tmp.path()).read();

    assert_eq!(thermals.gpu_c, Some(42.0));
    assert_eq!(thermals.gpu_power_w, Some(8.0));
}

#[test]
fn the_first_cpu_driver_that_reads_successfully_wins() {
    let tmp = TempDir::new().unwrap();
    device(
        tmp.path(),
        "hwmon0",
        &[("name", "k10temp"), ("temp1_input", "bad")],
    );
    device(
        tmp.path(),
        "hwmon1",
        &[("name", "zenpower"), ("temp1_input", "55000")],
    );

    assert_eq!(reader(tmp.path()).read().cpu_c, Some(55.0));
}

#[test]
fn battery_temperature_comes_from_the_pack_attribute_in_tenths() {
    let tmp = TempDir::new().unwrap();
    let battery = tmp.path().join("BAT0");
    std::fs::create_dir_all(&battery).unwrap();
    std::fs::write(battery.join("temp"), "305\n").unwrap();

    let mut reader = ThermalReader::new(&battery, tmp.path().join("hwmon"));

    assert_eq!(reader.read().battery_c, Some(30.5));
}

#[test]
fn battery_temperature_falls_back_to_an_hwmon_device_in_millidegrees() {
    let tmp = TempDir::new().unwrap();
    let battery = tmp.path().join("BAT0");
    std::fs::create_dir_all(battery.join("hwmon3")).unwrap();
    std::fs::write(battery.join("hwmon3").join("temp1_input"), "31250\n").unwrap();

    let mut reader = ThermalReader::new(&battery, tmp.path().join("hwmon"));

    // Deliberately unrounded, unlike every system temperature.
    assert_eq!(reader.read().battery_c, Some(31.25));
}

#[test]
fn an_empty_battery_attribute_falls_through_to_hwmon() {
    // An empty `temp` file previously parsed as 0 °C, which is plausible
    // enough to look like a reading and masked the hwmon sensor that had
    // the real answer. The TypeScript discarded empty attributes first.
    let tmp = TempDir::new().unwrap();
    let battery = tmp.path().join("BAT0");
    std::fs::create_dir_all(battery.join("hwmon3")).unwrap();
    std::fs::write(battery.join("temp"), "\n").unwrap();
    std::fs::write(battery.join("hwmon3").join("temp1_input"), "31000\n").unwrap();

    let mut reader = ThermalReader::new(&battery, tmp.path().join("hwmon"));

    assert_eq!(reader.read().battery_c, Some(31.0));
}

#[test]
fn an_implausible_battery_attribute_falls_through_to_hwmon() {
    let tmp = TempDir::new().unwrap();
    let battery = tmp.path().join("BAT0");
    std::fs::create_dir_all(battery.join("hwmon3")).unwrap();
    std::fs::write(battery.join("temp"), "-2731\n").unwrap();
    std::fs::write(battery.join("hwmon3").join("temp1_input"), "29000\n").unwrap();

    let mut reader = ThermalReader::new(&battery, tmp.path().join("hwmon"));

    assert_eq!(reader.read().battery_c, Some(29.0));
}

#[test]
fn a_battery_sensor_that_stops_reading_triggers_a_rescan() {
    // The staleness check originally covered only the system sensors, so a
    // battery hwmon path that went stale was never re-resolved and
    // battery_c stayed None for the life of the process — silencing the
    // whole battery-thermal alert family on hardware that has a sensor.
    let tmp = TempDir::new().unwrap();
    let battery = tmp.path().join("BAT0");
    std::fs::create_dir_all(battery.join("hwmon3")).unwrap();
    std::fs::write(battery.join("hwmon3").join("temp1_input"), "31000\n").unwrap();

    let mut reader = ThermalReader::new(&battery, tmp.path().join("hwmon"));
    assert_eq!(reader.read().battery_c, Some(31.0));

    // The driver re-registers its hwmon child at a different index.
    std::fs::remove_dir_all(battery.join("hwmon3")).unwrap();
    assert_eq!(reader.read().battery_c, None);

    std::fs::create_dir_all(battery.join("hwmon5")).unwrap();
    std::fs::write(battery.join("hwmon5").join("temp1_input"), "33500\n").unwrap();
    assert_eq!(
        reader.read().battery_c,
        Some(33.5),
        "the reader did not rescan for the battery sensor"
    );
}

#[test]
fn a_battery_with_no_sensor_reports_nothing() {
    let tmp = TempDir::new().unwrap();
    let battery = tmp.path().join("BAT0");
    std::fs::create_dir_all(&battery).unwrap();

    let mut reader = ThermalReader::new(&battery, tmp.path().join("hwmon"));

    assert_eq!(reader.read().battery_c, None);
}

#[test]
fn sensor_paths_are_resolved_once_and_reused() {
    let tmp = TempDir::new().unwrap();
    device(
        tmp.path(),
        "hwmon0",
        &[("name", "k10temp"), ("temp1_input", "48000")],
    );

    let mut reader = reader(tmp.path());
    assert_eq!(reader.read().cpu_c, Some(48.0));

    // Renaming the device would defeat a rescan; the cached path still reads.
    std::fs::write(tmp.path().join("hwmon0").join("name"), "renamed\n").unwrap();
    std::fs::write(tmp.path().join("hwmon0").join("temp1_input"), "52000\n").unwrap();

    assert_eq!(
        reader.read().cpu_c,
        Some(52.0),
        "cached path was not reused"
    );
}

#[test]
fn a_driver_that_loads_after_the_first_scan_is_eventually_found() {
    // A daemon started early in boot scans before k10temp is loaded. The
    // absent CPU sensor has no path to fail, so staleness cannot detect it
    // and without the slow retry cpu_c would stay None for the life of the
    // process — silencing the heat-soak and thermal-anomaly families.
    let tmp = TempDir::new().unwrap();
    std::fs::create_dir_all(tmp.path()).unwrap();
    let mut reader = reader(tmp.path());

    assert_eq!(reader.read().cpu_c, None, "nothing is loaded yet");

    device(
        tmp.path(),
        "hwmon0",
        &[("name", "k10temp"), ("temp1_input", "48000")],
    );

    // Not on the very next read — the whole point is that rescanning is
    // rare, since it is the ~4 ms cost the caching removed.
    assert_eq!(reader.read().cpu_c, None, "should not rescan every read");

    for _ in 0..RESCAN_INTERVAL {
        reader.read();
    }
    assert_eq!(
        reader.read().cpu_c,
        Some(48.0),
        "a late-loading driver was never discovered"
    );
}

#[test]
fn a_complete_sensor_set_is_never_rescanned() {
    // The counterpart: a machine whose sensors were all found must keep
    // paying nothing, however long it runs.
    let tmp = TempDir::new().unwrap();
    let battery = tmp.path().join("BAT0");
    std::fs::create_dir_all(battery.join("hwmon9")).unwrap();
    std::fs::write(battery.join("hwmon9").join("temp1_input"), "30000\n").unwrap();
    let hwmon = tmp.path().join("hwmon");
    device(
        &hwmon,
        "hwmon0",
        &[("name", "k10temp"), ("temp1_input", "48000")],
    );
    device(
        &hwmon,
        "hwmon1",
        &[
            ("name", "amdgpu"),
            ("temp1_input", "44000"),
            ("power1_input", "9120000"),
        ],
    );
    device(
        &hwmon,
        "hwmon2",
        &[("name", "nvme"), ("temp1_input", "41000")],
    );

    let mut reader = ThermalReader::new(&battery, &hwmon);
    assert_eq!(reader.read().cpu_c, Some(48.0));

    // Renaming every device would defeat any rescan; the cached paths must
    // still be the ones being read.
    for entry in std::fs::read_dir(&hwmon).unwrap() {
        let dir = entry.unwrap().path();
        std::fs::write(dir.join("name"), "renamed\n").unwrap();
    }
    for _ in 0..(RESCAN_INTERVAL * 2) {
        reader.read();
    }

    assert_eq!(
        reader.read().cpu_c,
        Some(48.0),
        "a complete set should never have been rescanned"
    );
}

#[test]
fn a_sensor_that_stops_reading_triggers_a_rescan() {
    let tmp = TempDir::new().unwrap();
    device(
        tmp.path(),
        "hwmon0",
        &[("name", "k10temp"), ("temp1_input", "48000")],
    );

    let mut reader = reader(tmp.path());
    assert_eq!(reader.read().cpu_c, Some(48.0));

    // The device disappears — a GPU suspending, hardware unplugged.
    std::fs::remove_dir_all(tmp.path().join("hwmon0")).unwrap();
    assert_eq!(reader.read().cpu_c, None);

    // A replacement appears at a different index and is picked up.
    device(
        tmp.path(),
        "hwmon7",
        &[("name", "k10temp"), ("temp1_input", "61000")],
    );
    assert_eq!(reader.read().cpu_c, Some(61.0), "reader did not rescan");
}
