//! One-shot cycle totals must respect the flight recorder's denser evidence.

use batmon::db::{Database, Store};
use batmon::types::{PowerState, Sample};
use tempfile::TempDir;

use super::{batmon, database_dir, environment, query};

fn fixture(identified: bool) -> (TempDir, TempDir, Sample) {
    let (home, battery) = environment();
    let battery_id = identified.then(|| {
        for (attribute, value) in [
            ("manufacturer", "TestCo"),
            ("model_name", "Model"),
            ("serial_number", "pack-A"),
        ] {
            std::fs::write(battery.path().join(attribute), format!("{value}\n")).unwrap();
        }
        serde_json::json!(["TestCo", "Model", "pack-A"]).to_string()
    });
    let sample = Sample {
        ts: "2026-11-12T00:00:00.000Z".to_owned(),
        battery_id,
        status: "Discharging".to_owned(),
        power_state: PowerState::Discharging,
        energy_wh: Some(48.0),
        energy_design_wh: Some(60.0),
        estimated_cycle_count: Some(1.5),
        is_present: true,
        ..Sample::default()
    };
    (home, battery, sample)
}

fn record(home: &TempDir, battery: &TempDir) {
    let supplies = battery.path().join("power_supply");
    let hwmon = battery.path().join("hwmon");
    std::fs::create_dir_all(&supplies).unwrap();
    std::fs::create_dir_all(&hwmon).unwrap();
    let output = batmon(home.path(), battery.path())
        .env("BATMON_POWER_SUPPLY_BASE", supplies)
        .env("BATMON_HWMON_BASE", hwmon)
        .env(
            "DBUS_SYSTEM_BUS_ADDRESS",
            format!("unix:path={}/no-system-bus", battery.path().display()),
        )
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path={}/no-session-bus", battery.path().display()),
        )
        .arg("--oneshot")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_totals(home: &TempDir, expected: Option<f64>) {
    for name in ["debug.db", "battery.db"] {
        let total: Option<f64> = query(
            &database_dir(home.path()).join(name),
            "SELECT estimated_cycle_count FROM samples ORDER BY id DESC LIMIT 1",
        );
        assert_eq!(total, expected, "{name}");
    }
}

#[test]
fn a_recorded_energy_gap_is_not_bridged_by_oneshot_history() {
    for identified in [false, true] {
        let (home, battery, seed) = fixture(identified);
        let databases = database_dir(home.path());
        let debug = Store::open(&databases.join("debug.db"), Database::Debug).unwrap();
        let historical = Store::open(&databases.join("battery.db"), Database::Historical).unwrap();
        debug.insert(&seed).unwrap();
        historical.insert(&seed).unwrap();
        debug
            .insert(&Sample {
                energy_wh: None,
                ..seed
            })
            .unwrap();

        // The fixture now reads 42 Wh, but the missing flight sample broke
        // continuity with the 48 Wh historical row. Neither tier may add 0.1.
        record(&home, &battery);
        assert_totals(&home, Some(1.5));

        // A later, fully observed interval must still accrue cycles normally.
        std::fs::write(battery.path().join("energy_now"), "36000000\n").unwrap();
        record(&home, &battery);
        assert_totals(&home, Some(1.6));
        record(&home, &battery);
        assert_totals(&home, Some(1.6));
    }
}

#[test]
fn oneshot_restores_cycles_when_only_history_has_samples() {
    for identified in [false, true] {
        let (home, battery, seed) = fixture(identified);
        Store::open(
            &database_dir(home.path()).join("battery.db"),
            Database::Historical,
        )
        .unwrap()
        .insert(&seed)
        .unwrap();

        record(&home, &battery);
        assert_totals(&home, Some(1.6));
    }
}

#[test]
fn oneshot_history_preserves_recorded_identity_uncertainty() {
    let (home, battery, seed) = fixture(true);
    let databases = database_dir(home.path());
    Store::open(&databases.join("battery.db"), Database::Historical)
        .unwrap()
        .insert(&Sample {
            battery_id: None,
            ..seed.clone()
        })
        .unwrap();
    Store::open(&databases.join("debug.db"), Database::Debug)
        .unwrap()
        .insert(&Sample {
            battery_id: None,
            estimated_cycle_count: None,
            ..seed
        })
        .unwrap();
    std::fs::remove_file(battery.path().join("serial_number")).unwrap();

    record(&home, &battery);
    assert_totals(&home, None);
}
