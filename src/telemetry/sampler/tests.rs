#![allow(clippy::float_cmp)]

use std::path::Path;

use super::*;
use tempfile::TempDir;

/// Fixed estimates, standing in for UPower.
struct FixedEstimates {
    empty: Option<i64>,
    full: Option<i64>,
}

impl TimeEstimates for FixedEstimates {
    fn time_to_empty_s(&self) -> Option<i64> {
        self.empty
    }
    fn time_to_full_s(&self) -> Option<i64> {
        self.full
    }
}

/// A complete fake machine: battery, hwmon, procfs, cpufreq and drm.
struct Machine {
    root: TempDir,
    paths: Paths,
}

impl Machine {
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        let at = |name: &str| root.path().join(name);

        for dir in ["power_supply/BAT0", "hwmon", "cpu", "drm", "proc"] {
            std::fs::create_dir_all(at(dir)).unwrap();
        }

        let paths = Paths {
            power_supply_base: at("power_supply"),
            battery: at("power_supply/BAT0"),
            hwmon_base: at("hwmon"),
            cpu_base: at("cpu"),
            drm_base: at("drm"),
            proc_base: at("proc"),
            db_dir: at("db"),
        };

        let machine = Self { root, paths };
        machine.battery(&[
            ("status", "Discharging"),
            ("capacity", "80"),
            ("energy_now", "46500300"),
            ("energy_full", "58073400"),
            ("energy_full_design", "58327500"),
            ("power_w_unused", "0"),
            ("power_now", "20416360"),
            ("voltage_now", "12095000"),
            ("voltage_min_design", "11550000"),
            ("cycle_count", "0"),
            ("present", "1"),
        ]);
        machine.proc(&[
            ("stat", "cpu  1000 100 500 8000 400 0 0 0 0 0\n"),
            (
                "meminfo",
                "MemTotal:       1000000 kB\nMemAvailable:    400000 kB\n",
            ),
            ("loadavg", "1.19 0.94 0.83 2/1543 1\n"),
            ("uptime", "59289.86 412345.67\n"),
            (
                "sys/kernel/random/boot_id",
                "7d0e0b50-a9e4-4880-8366-14fe16e77f9b\n",
            ),
        ]);
        machine
    }

    fn battery(&self, attributes: &[(&str, &str)]) {
        for (name, value) in attributes {
            std::fs::write(self.paths.battery.join(name), format!("{value}\n")).unwrap();
        }
    }

    fn proc(&self, files: &[(&str, &str)]) {
        for (name, contents) in files {
            let path = self.paths.proc_base.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }
    }

    fn hwmon(&self, device: &str, files: &[(&str, &str)]) {
        let dir = self.paths.hwmon_base.join(device);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, value) in files {
            std::fs::write(dir.join(name), format!("{value}\n")).unwrap();
        }
    }

    fn process(&self, pid: u32, name: &str, ticks: u64) {
        let dir = self.paths.proc_base.join(pid.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        let mut fields = vec!["0".to_owned(); 44];
        fields[0] = "S".to_owned();
        fields[11] = ticks.to_string();
        std::fs::write(
            dir.join("stat"),
            format!("{pid} ({name}) {}\n", fields.join(" ")),
        )
        .unwrap();
    }

    fn sampler(&self) -> Sampler {
        Sampler::without_estimates(self.paths.clone())
    }

    fn root(&self) -> &Path {
        self.root.path()
    }
}

#[test]
fn assembles_a_complete_sample_from_every_source() {
    let machine = Machine::new();
    machine.hwmon("hwmon0", &[("name", "k10temp"), ("temp1_input", "48500")]);
    machine.hwmon(
        "hwmon1",
        &[
            ("name", "amdgpu"),
            ("temp1_input", "44000"),
            ("power1_input", "9120000"),
        ],
    );
    machine.hwmon("hwmon2", &[("name", "nvme"), ("temp1_input", "41850")]);
    std::fs::create_dir_all(machine.root().join("cpu/cpu0/cpufreq")).unwrap();
    std::fs::write(
        machine.root().join("cpu/cpu0/cpufreq/scaling_cur_freq"),
        "1749000\n",
    )
    .unwrap();
    std::fs::create_dir_all(machine.root().join("drm/card0/device")).unwrap();
    std::fs::write(
        machine.root().join("drm/card0/device/gpu_busy_percent"),
        "0\n",
    )
    .unwrap();
    machine.process(1, "systemd", 0);

    let sample = machine
        .sampler()
        .sample()
        .expect("a readable battery yields a sample");

    assert_eq!(sample.status, "Discharging");
    assert_eq!(sample.power_state, PowerState::Discharging);
    assert!(!sample.is_charging);
    assert!(sample.is_present);
    assert_eq!(sample.charge_pct, 80.0);
    assert_eq!(sample.cycle_count, Some(0));

    assert_eq!(sample.cpu_temp_c, Some(48.5));
    assert_eq!(sample.gpu_temp_c, Some(44.0));
    assert_eq!(sample.nvme_temp_c, Some(41.9));
    assert_eq!(sample.gpu_power_w, Some(9.12));
    assert_eq!(sample.battery_temp_c, None);

    assert_eq!(sample.cpu_freq_mhz, Some(1749.0));
    assert_eq!(sample.gpu_pct, Some(0.0));
    assert_eq!(sample.mem_pct, Some(60.0));
    assert_eq!(sample.load1, Some(1.19));
    assert_eq!(sample.uptime_s, Some(59_289.86));
    assert_eq!(
        sample.boot_id.as_deref(),
        Some("7d0e0b50-a9e4-4880-8366-14fe16e77f9b")
    );
    assert!(sample.top_processes.is_some());
}

#[test]
fn energy_and_power_are_rounded_to_three_places_but_voltage_is_not() {
    // Reproduces the original's asymmetry exactly; these are the real values
    // this machine's battery reports.
    let machine = Machine::new();
    let sample = machine
        .sampler()
        .sample()
        .expect("a readable battery yields a sample");

    assert_eq!(sample.energy_wh, 46.5);
    assert_eq!(sample.energy_full_wh, 58.073);
    assert_eq!(sample.energy_design_wh, 58.328);
    assert_eq!(sample.power_w, 20.416);
    assert_eq!(sample.voltage_v, 12.095);
    assert_eq!(sample.voltage_design_v, 11.55);
    assert_eq!(sample.health_pct, 99.56);
}

#[test]
fn the_timestamp_has_the_shape_to_iso_string_produces() {
    let machine = Machine::new();
    let sample = machine
        .sampler()
        .sample()
        .expect("a readable battery yields a sample");

    assert_eq!(sample.ts.len(), 24, "{}", sample.ts);
    assert!(sample.ts.ends_with('Z'));
}

#[test]
fn the_cycle_count_is_left_for_the_store_to_integrate() {
    let machine = Machine::new();
    let sample = machine
        .sampler()
        .sample()
        .expect("a readable battery yields a sample");

    assert_eq!(sample.estimated_cycle_count, 0.0);
}

#[test]
fn utilisation_is_absent_on_the_first_sample_and_present_on_the_second() {
    let machine = Machine::new();
    let mut sampler = machine.sampler();

    assert_eq!(
        sampler
            .sample()
            .expect("a readable battery yields a sample")
            .cpu_pct,
        None,
        "one reading of a counter is not a rate"
    );

    machine.proc(&[("stat", "cpu  1250 100 500 8100 400 0 0 0 0 0\n")]);
    assert_eq!(
        sampler
            .sample()
            .expect("a readable battery yields a sample")
            .cpu_pct,
        Some(71.4)
    );
}

#[test]
fn a_missing_battery_produces_a_sample_marked_absent() {
    let machine = Machine::new();
    std::fs::remove_dir_all(&machine.paths.battery).unwrap();

    let sample = machine
        .sampler()
        .sample()
        .expect("a readable battery yields a sample");

    assert!(!sample.is_present);
    assert_eq!(sample.status, "Unknown");
    assert_eq!(sample.power_state, PowerState::Unknown);
    assert_eq!(sample.charge_pct, 0.0);
}

#[test]
fn a_present_but_unreadable_battery_yields_no_sample_at_all() {
    // Distinct from the case above. The hardware is still installed, so its
    // latched alerts still describe something real; the tick simply has
    // nothing to say. Reporting zeros instead announced a critical low
    // battery and accrued most of a cycle in one tick.
    let machine = Machine::new();
    for attribute in [
        "capacity",
        "energy_now",
        "energy_full",
        "charge_now",
        "charge_full",
    ] {
        let _ = std::fs::remove_file(machine.paths.battery.join(attribute));
    }

    // The directory survives, so the pack still reads as installed.
    assert!(machine.paths.battery.exists());
    assert!(machine.sampler().sample().is_none());
}

#[test]
fn runtime_falls_back_to_arithmetic_when_no_estimate_source_answers() {
    let machine = Machine::new();
    let sample = machine
        .sampler()
        .sample()
        .expect("a readable battery yields a sample");

    // 46.5003 Wh at 20.41636 W is 8199 seconds.
    assert_eq!(sample.time_to_empty_s, Some(8_199));
    assert_eq!(sample.time_to_full_s, None, "not charging");
}

#[test]
fn an_external_estimate_is_preferred_over_the_arithmetic_fallback() {
    let machine = Machine::new();
    let mut sampler = Sampler::new(
        machine.paths.clone(),
        Box::new(FixedEstimates {
            empty: Some(9_422),
            full: None,
        }),
    );

    assert_eq!(
        sampler
            .sample()
            .expect("a readable battery yields a sample")
            .time_to_empty_s,
        Some(9_422)
    );
}

#[test]
fn a_charging_battery_estimates_time_to_full_instead() {
    let machine = Machine::new();
    machine.battery(&[("status", "Charging")]);

    let sample = machine
        .sampler()
        .sample()
        .expect("a readable battery yields a sample");

    assert_eq!(sample.power_state, PowerState::Charging);
    assert!(sample.is_charging);
    assert_eq!(sample.time_to_empty_s, None);
    // (58.0734 - 46.5003) Wh at 20.41636 W is 2041 seconds.
    assert_eq!(sample.time_to_full_s, Some(2_041));
}

#[test]
fn a_negligible_rate_produces_no_estimate_rather_than_a_fantasy() {
    let machine = Machine::new();
    machine.battery(&[("power_now", "100000")]); // 0.1 W

    assert_eq!(
        machine
            .sampler()
            .sample()
            .expect("a readable battery yields a sample")
            .time_to_empty_s,
        None
    );
}

#[test]
fn changing_the_rail_invalidates_a_cached_estimate_immediately() {
    // A time-to-empty is meaningless the instant a charger is connected;
    // showing a stale one for up to a minute would be worse than none.
    let machine = Machine::new();
    let mut sampler = Sampler::new(
        machine.paths.clone(),
        Box::new(FixedEstimates {
            empty: Some(9_422),
            full: Some(1_800),
        }),
    );

    assert_eq!(
        sampler
            .sample()
            .expect("a readable battery yields a sample")
            .time_to_empty_s,
        Some(9_422)
    );

    machine.battery(&[("status", "Charging")]);
    let charging = sampler
        .sample()
        .expect("a readable battery yields a sample");

    assert_eq!(charging.time_to_empty_s, None);
    assert_eq!(charging.time_to_full_s, Some(1_800));
}

#[test]
fn an_estimate_is_reused_between_polls_rather_than_refetched() {
    struct Counting(std::rc::Rc<std::cell::Cell<u32>>);
    impl TimeEstimates for Counting {
        fn time_to_empty_s(&self) -> Option<i64> {
            self.0.set(self.0.get() + 1);
            Some(1_000)
        }
        fn time_to_full_s(&self) -> Option<i64> {
            None
        }
    }

    let machine = Machine::new();
    let calls = std::rc::Rc::new(std::cell::Cell::new(0));
    let mut sampler = Sampler::new(
        machine.paths.clone(),
        Box::new(Counting(std::rc::Rc::clone(&calls))),
    );

    for _ in 0..10 {
        sampler
            .sample()
            .expect("a readable battery yields a sample");
    }

    assert_eq!(
        calls.get(),
        1,
        "the estimate is cached for a minute, not fetched every tick"
    );
}

#[test]
fn two_samplers_share_no_state() {
    // What the four resetForTesting exports existed to fake.
    let machine = Machine::new();
    let mut first = machine.sampler();
    assert_eq!(
        first
            .sample()
            .expect("a readable battery yields a sample")
            .cpu_pct,
        None
    );
    machine.proc(&[("stat", "cpu  1250 100 500 8100 400 0 0 0 0 0\n")]);
    assert!(
        first
            .sample()
            .expect("a readable battery yields a sample")
            .cpu_pct
            .is_some()
    );

    let mut second = machine.sampler();
    assert_eq!(
        second
            .sample()
            .expect("a readable battery yields a sample")
            .cpu_pct,
        None,
        "a new sampler must start without history"
    );
}
