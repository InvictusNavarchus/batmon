//! Temperatures, read straight from the hwmon ABI.
//!
//! The significant change from the TypeScript daemon is here: sensor *paths* are
//! resolved once and reused, rather than rescanning `/sys/class/hwmon` on every
//! tick. That scan was measured at roughly 5.6 ms per sample — the second
//! largest cost in the loop — and it was pure waste, because hwmon numbering is
//! assigned when a driver initialises and does not change for the life of a boot.

use std::path::{Path, PathBuf};

use crate::parity::js_number;
use crate::units::{Celsius, MICRO};

/// Drivers that report a CPU package or core temperature.
const CPU_DRIVERS: &[&str] = &[
    "k10temp",
    "zenpower",
    "coretemp",
    "cpu_thermal",
    "soc_thermal",
];

/// Drivers that report a GPU temperature.
const GPU_DRIVERS: &[&str] = &["amdgpu", "i915", "xe", "nouveau"];

/// Highest `tempN_*` index searched when hunting for a labelled sensor.
const MAX_LABELLED_SENSORS: u8 = 16;

/// Highest `tempN_input` index searched under a battery's own hwmon directory.
const MAX_BATTERY_SENSORS: u8 = 3;

/// Reads between rescans while any sensor class is still missing.
///
/// A sensor that was absent when the tree was scanned cannot be detected as
/// stale, because there is no path to fail — so without this an incomplete set
/// would stay incomplete for the life of the process. Drivers genuinely do
/// appear late: a module loaded towards the end of boot, a discrete GPU
/// resuming from runtime suspend, a battery re-registering its hwmon child.
///
/// At one read per second this retries about once a minute, so the scan's ~4 ms
/// costs well under a tenth of a millisecond per tick amortised — while a
/// machine that simply has no such sensor pays exactly that and nothing more.
const RESCAN_INTERVAL: u32 = 60;

/// One tick of thermal readings.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Thermals {
    /// Battery pack temperature. Absent on most laptops.
    pub battery_c: Option<f64>,
    pub cpu_c: Option<f64>,
    pub gpu_c: Option<f64>,
    pub nvme_c: Option<f64>,
    /// GPU package power, reported only by amdgpu.
    pub gpu_power_w: Option<f64>,
}

/// Sensor files located during a scan.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Sensors {
    cpu: Option<PathBuf>,
    gpu_temp: Option<PathBuf>,
    gpu_power: Option<PathBuf>,
    nvme: Option<PathBuf>,
    /// A `tempN_input` under the battery's own directory, used only when the
    /// battery exposes no `temp` attribute of its own.
    battery: Option<PathBuf>,
}

impl Sensors {
    /// Whether any sensor class went unlocated, and so is worth looking for again.
    fn is_incomplete(&self) -> bool {
        self.cpu.is_none()
            || self.gpu_temp.is_none()
            || self.gpu_power.is_none()
            || self.nvme.is_none()
            || self.battery.is_none()
    }
}

/// Reads temperatures, remembering where it found them.
#[derive(Debug)]
pub struct ThermalReader {
    battery_dir: PathBuf,
    hwmon_base: PathBuf,
    sensors: Option<Sensors>,
    /// Reads since the last scan, for retrying an incomplete set.
    reads_since_scan: u32,
}

impl ThermalReader {
    #[must_use]
    pub fn new(battery_dir: impl Into<PathBuf>, hwmon_base: impl Into<PathBuf>) -> Self {
        Self {
            battery_dir: battery_dir.into(),
            hwmon_base: hwmon_base.into(),
            sensors: None,
            reads_since_scan: 0,
        }
    }

    /// Read every temperature, scanning for sensors first if necessary.
    ///
    /// A sensor that stops reading invalidates the cache, so the next tick
    /// rescans. That covers the cases where hwmon numbering really can change —
    /// a GPU going into runtime suspend, an external device disconnecting —
    /// without paying for a scan when nothing has moved.
    pub fn read(&mut self) -> Thermals {
        // Two ways the cache is dropped. A located path that stops reading is
        // detected below; a class that was never located at all cannot be, so
        // an incomplete set is retried on the slow cadence instead.
        self.reads_since_scan = self.reads_since_scan.saturating_add(1);
        if self.reads_since_scan >= RESCAN_INTERVAL
            && self.sensors.as_ref().is_some_and(Sensors::is_incomplete)
        {
            self.sensors = None;
        }
        if self.sensors.is_none() {
            self.reads_since_scan = 0;
        }

        let sensors = self
            .sensors
            .get_or_insert_with(|| scan(&self.hwmon_base, &self.battery_dir));

        let cpu_c = read_optional(sensors.cpu.as_deref());
        let gpu_c = read_optional(sensors.gpu_temp.as_deref());
        let nvme_c = read_optional(sensors.nvme.as_deref());
        let gpu_power_w = sensors
            .gpu_power
            .as_deref()
            .and_then(read_watts)
            .map(|watts| crate::parity::round_to(watts, 2));

        // The battery's hwmon sensor is cached exactly like the others, so it
        // has to be read before the staleness decision and counted in it.
        let battery_hwmon = sensors.battery.as_deref().and_then(read_millidegrees);
        let battery_c = attribute_temp(&self.battery_dir)
            .or(battery_hwmon)
            .map(Celsius::get);

        let expected = [
            (sensors.cpu.is_some(), cpu_c.is_some()),
            (sensors.gpu_temp.is_some(), gpu_c.is_some()),
            (sensors.nvme.is_some(), nvme_c.is_some()),
            (sensors.gpu_power.is_some(), gpu_power_w.is_some()),
            (sensors.battery.is_some(), battery_hwmon.is_some()),
        ];
        let stale = expected.iter().any(|(located, read)| *located && !*read);

        if stale {
            self.sensors = None;
        }

        Thermals {
            battery_c,
            cpu_c: cpu_c.map(|c| c.rounded_tenth().get()),
            gpu_c: gpu_c.map(|c| c.rounded_tenth().get()),
            nvme_c: nvme_c.map(|c| c.rounded_tenth().get()),
            gpu_power_w,
        }
    }
}

/// The pack's own `temp` attribute, in tenths of a degree.
///
/// Preferred over an hwmon child device when present. Deliberately unrounded,
/// unlike every other temperature here: the TypeScript daemon rounds system
/// temperatures and stores this one raw, and reproducing the asymmetry keeps
/// stored values identical. Normalising it is a behaviour change for its own
/// commit.
fn attribute_temp(battery_dir: &Path) -> Option<Celsius> {
    read_attribute_number(&battery_dir.join("temp")).and_then(Celsius::from_tenths)
}

/// Locate every sensor of interest under `hwmon_base`.
///
/// Selection order matters and mirrors the original exactly: entries are visited
/// in sorted name order, the first CPU driver that yields a usable reading wins,
/// and NVMe takes the first match.
fn scan(hwmon_base: &Path, battery_dir: &Path) -> Sensors {
    let mut sensors = Sensors {
        battery: scan_battery_hwmon(battery_dir),
        ..Sensors::default()
    };

    let Ok(entries) = std::fs::read_dir(hwmon_base) else {
        return sensors;
    };
    let mut names: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .collect();
    names.sort();

    for name in names {
        let device = hwmon_base.join(name);
        let Ok(driver) = std::fs::read_to_string(device.join("name")) else {
            continue;
        };
        let driver = driver.trim();

        if sensors.cpu.is_none() && CPU_DRIVERS.contains(&driver) {
            sensors.cpu = locate_cpu_sensor(&device, driver);
        }

        if GPU_DRIVERS.contains(&driver) {
            let temp = usable_temp(&device.join("temp1_input"));
            let power = (driver == "amdgpu")
                .then(|| locate_gpu_power(&device))
                .flatten();

            // Both readings are always adopted together, so they can only ever
            // describe one device. Reporting a discrete GPU's 90 W beside an
            // integrated GPU's 42 °C would describe a machine that does not
            // exist, and a GPU in runtime suspend is exactly how that happens:
            // temp1_input errors while power1_input still reads.
            let have_both = sensors.gpu_temp.is_some() && sensors.gpu_power.is_some();
            let candidate_is_complete = temp.is_some() && power.is_some();
            let nothing_adopted_yet = sensors.gpu_temp.is_none() && sensors.gpu_power.is_none();

            // Take whatever the first GPU offers, then upgrade only to a later
            // device that can supply the full pair itself.
            if nothing_adopted_yet || (!have_both && candidate_is_complete) {
                sensors.gpu_temp = temp;
                sensors.gpu_power = power;
            }
        }

        if sensors.nvme.is_none() && driver == "nvme" {
            sensors.nvme = usable_temp(&device.join("temp1_input"));
        }
    }

    sensors
}

/// Find the CPU package sensor for one device.
///
/// Intel's `coretemp` exposes one sensor per core plus a package-wide one, and
/// only the package figure is meaningful here — a single core briefly boosting
/// says nothing about what the battery beneath it is experiencing. Everything
/// else reports a single usable sensor at `temp1_input`.
fn locate_cpu_sensor(device: &Path, driver: &str) -> Option<PathBuf> {
    if driver == "coretemp" {
        for index in 1..=MAX_LABELLED_SENSORS {
            let label_file = device.join(format!("temp{index}_label"));
            let input_file = device.join(format!("temp{index}_input"));

            let Ok(label) = std::fs::read_to_string(&label_file) else {
                continue;
            };
            if !label.trim().starts_with("Package id") {
                continue;
            }
            if let Some(path) = usable_temp(&input_file) {
                return Some(path);
            }
        }
    }

    usable_temp(&device.join("temp1_input"))
}

/// AMD reports package power under one of two names depending on the generation.
fn locate_gpu_power(device: &Path) -> Option<PathBuf> {
    ["power1_input", "power1_average"]
        .into_iter()
        .map(|name| device.join(name))
        .find(|path| read_watts(path).is_some())
}

/// A `tempN_input` under the battery's own directory, for packs whose driver
/// registers an hwmon device instead of exposing a `temp` attribute.
fn scan_battery_hwmon(battery_dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(battery_dir).ok()?;
    let mut names: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .collect();
    names.sort();

    for name in names {
        if !name.to_string_lossy().starts_with("hwmon") {
            continue;
        }
        for index in 1..=MAX_BATTERY_SENSORS {
            let candidate = battery_dir.join(&name).join(format!("temp{index}_input"));
            if read_millidegrees(&candidate).is_some() {
                return Some(candidate);
            }
        }
    }
    None
}

/// The path, if it currently holds a plausible temperature.
fn usable_temp(path: &Path) -> Option<PathBuf> {
    read_millidegrees(path).map(|_| path.to_path_buf())
}

/// A `power_supply` attribute, treating an empty file as absent.
///
/// Deliberately different from [`read_number`], which the hwmon paths use. There
/// an empty file reads as 0 °C, because the TypeScript passed trimmed contents
/// straight to `Number()` and `Number("")` is zero — verified against it, and
/// pinned by a test. The battery reader discards empty attributes before
/// parsing, and this has to match it: otherwise an empty `temp` file reports a
/// plausible 0 °C and masks the hwmon fallback that would have answered.
fn read_attribute_number(path: &Path) -> Option<f64> {
    std::fs::read_to_string(path)
        .ok()
        .map(|contents| contents.trim().to_owned())
        .filter(|contents| !contents.is_empty())
        .and_then(|contents| js_number(&contents))
}

fn read_number(path: &Path) -> Option<f64> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|contents| js_number(contents.trim()))
}

fn read_millidegrees(path: &Path) -> Option<Celsius> {
    read_number(path).and_then(Celsius::from_millidegrees)
}

fn read_optional(path: Option<&Path>) -> Option<Celsius> {
    path.and_then(read_millidegrees)
}

/// Microwatts to watts. Negative readings are rejected as driver noise; a GPU
/// cannot consume less than nothing, and zero is a real value for one asleep.
fn read_watts(path: &Path) -> Option<f64> {
    read_number(path)
        .filter(|microwatts| *microwatts >= 0.0)
        .map(|microwatts| microwatts / MICRO)
}

#[cfg(test)]
mod tests {
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
}
