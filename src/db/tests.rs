#![allow(clippy::float_cmp)]

use super::*;

fn sample(ts: &str, charge_pct: f64) -> Sample {
    Sample {
        ts: ts.to_owned(),
        charge_pct,
        status: "Discharging".to_owned(),
        power_state: PowerState::Discharging,
        energy_wh: 45.0,
        energy_full_wh: 50.0,
        energy_design_wh: 53.0,
        power_w: 11.5,
        voltage_v: 15.4,
        voltage_design_v: 15.4,
        cycle_count: Some(120),
        estimated_cycle_count: 0.0,
        battery_temp_c: Some(32.1),
        health_pct: 94.3,
        is_charging: false,
        is_present: true,
        time_to_empty_s: Some(9_000),
        time_to_full_s: None,
        cpu_temp_c: Some(55.5),
        gpu_temp_c: Some(48.0),
        nvme_temp_c: Some(41.0),
        cpu_pct: Some(12.5),
        mem_pct: Some(38.2),
        top_processes: Some(r#"[{"name":"firefox","cpu":4.2,"mem":8.1,"count":3}]"#.to_owned()),
        cpu_freq_mhz: Some(2_800.0),
        gpu_pct: Some(3.0),
        gpu_power_w: Some(6.25),
        load1: Some(0.42),
        boot_id: Some("boot-uuid-1".to_owned()),
        uptime_s: Some(1_000.0),
    }
}

#[test]
fn a_fresh_store_has_no_rows() {
    let store = Store::open_in_memory(Database::Historical).unwrap();
    assert_eq!(store.latest().unwrap(), None);
}

#[test]
fn every_column_survives_a_round_trip() {
    // The insert list and the row mapper are written out by hand; this is
    // what catches one of them drifting from the other.
    let store = Store::open_in_memory(Database::Debug).unwrap();
    let original = sample("2026-09-06T00:00:00.000Z", 82.5);

    store.insert(&original).unwrap();

    assert_eq!(store.latest().unwrap().unwrap(), original);
}

#[test]
fn latest_returns_the_most_recently_inserted_row() {
    let store = Store::open_in_memory(Database::Debug).unwrap();
    store
        .insert(&sample("2026-09-06T00:00:00.000Z", 80.0))
        .unwrap();
    store
        .insert(&sample("2026-09-06T00:00:01.000Z", 79.0))
        .unwrap();

    let latest = store.latest().unwrap().unwrap();
    assert_eq!(latest.ts, "2026-09-06T00:00:01.000Z");
    assert_eq!(latest.charge_pct, 79.0);
}

#[test]
fn inserting_with_integration_returns_the_previous_row_and_accrues_cycles() {
    let store = Store::open_in_memory(Database::Historical).unwrap();

    let mut first = Sample {
        energy_wh: 50.0,
        estimated_cycle_count: 2.0,
        ..sample("2026-09-06T00:00:00.000Z", 90.0)
    };
    assert_eq!(store.insert_integrating_cycles(&mut first).unwrap(), None);
    assert_eq!(first.estimated_cycle_count, 0.0, "no history to carry");

    let mut second = Sample {
        energy_wh: 44.7, // 5.3 Wh out of a 53 Wh design capacity
        ..sample("2026-09-06T00:01:00.000Z", 80.0)
    };
    let previous = store
        .insert_integrating_cycles(&mut second)
        .unwrap()
        .unwrap();

    assert_eq!(previous.ts, "2026-09-06T00:00:00.000Z");
    assert!((second.estimated_cycle_count - 0.1).abs() < 1e-9);
}

#[test]
fn integration_writes_the_computed_value_back_to_the_caller() {
    // The one-shot path depends on this: it stores the same sample to both
    // databases, and the flight recorder must receive the integrated value.
    let store = Store::open_in_memory(Database::Historical).unwrap();
    store
        .insert(&Sample {
            energy_wh: 50.0,
            ..sample("2026-09-06T00:00:00.000Z", 90.0)
        })
        .unwrap();

    let mut next = Sample {
        energy_wh: 44.7,
        estimated_cycle_count: 999.0,
        ..sample("2026-09-06T00:01:00.000Z", 80.0)
    };
    store.insert_integrating_cycles(&mut next).unwrap();

    assert!(
        next.estimated_cycle_count < 1.0,
        "caller's value was not replaced"
    );
}

#[test]
fn prune_drops_rows_outside_the_retention_window() {
    let store = Store::open_in_memory(Database::Debug).unwrap();

    let now = Timestamp::now();
    let old = iso8601_millis(now.checked_sub(SignedDuration::from_hours(10)).unwrap());
    let recent = iso8601_millis(now.checked_sub(SignedDuration::from_hours(1)).unwrap());
    store.insert(&sample(&old, 70.0)).unwrap();
    store.insert(&sample(&recent, 65.0)).unwrap();

    let removed = store.prune_older_than(6).unwrap();

    assert_eq!(removed, 1);
    assert_eq!(store.latest().unwrap().unwrap().ts, recent);
}

#[test]
fn prune_refuses_a_non_positive_retention_window() {
    let store = Store::open_in_memory(Database::Debug).unwrap();
    store
        .insert(&sample("2026-09-06T00:00:00.000Z", 80.0))
        .unwrap();

    for hours in [0, -6] {
        assert!(
            matches!(
                store.prune_older_than(hours),
                Err(StoreError::InvalidRetention { .. })
            ),
            "retention of {hours} hours should be refused"
        );
    }

    assert!(
        store.latest().unwrap().is_some(),
        "a refused prune must not have deleted anything"
    );
}

#[test]
fn prune_on_an_empty_database_is_a_no_op() {
    let store = Store::open_in_memory(Database::Debug).unwrap();
    assert_eq!(store.prune_older_than(6).unwrap(), 0);
}

#[test]
fn integer_flags_read_back_as_booleans() {
    let store = Store::open_in_memory(Database::Debug).unwrap();
    store
        .conn
        .execute(
            "INSERT INTO samples (ts, status, is_charging, is_present)
             VALUES ('2026-09-06T00:00:00.000Z', 'Charging', 1, 0)",
            [],
        )
        .unwrap();

    let row = store.latest().unwrap().unwrap();
    assert!(row.is_charging);
    assert!(!row.is_present);
}

#[test]
fn a_legacy_row_with_no_power_state_is_repaired_from_its_status() {
    // Not hypothetical: 19,144 of the 19,176 rows in the historical database
    // on this machine predate schema version 6 and have a NULL here.
    let store = Store::open_in_memory(Database::Historical).unwrap();
    store
        .conn
        .execute(
            "INSERT INTO samples (ts, status, power_state)
             VALUES ('2026-08-15T16:51:39.025Z', 'Discharging', NULL)",
            [],
        )
        .unwrap();

    assert_eq!(
        store.latest().unwrap().unwrap().power_state,
        PowerState::Discharging
    );
}

#[test]
fn an_unrecognised_stored_power_state_falls_back_to_the_status() {
    let store = Store::open_in_memory(Database::Historical).unwrap();
    store
        .conn
        .execute(
            "INSERT INTO samples (ts, status, power_state)
             VALUES ('2026-09-06T00:00:00.000Z', 'Full', 'not-a-state')",
            [],
        )
        .unwrap();

    assert_eq!(
        store.latest().unwrap().unwrap().power_state,
        PowerState::AcIdle
    );
}

#[test]
fn a_stored_power_state_wins_over_the_status_string() {
    let store = Store::open_in_memory(Database::Historical).unwrap();
    store
        .conn
        .execute(
            "INSERT INTO samples (ts, status, power_state)
             VALUES ('2026-09-06T00:00:00.000Z', 'Discharging', 'ac_idle')",
            [],
        )
        .unwrap();

    assert_eq!(
        store.latest().unwrap().unwrap().power_state,
        PowerState::AcIdle
    );
}

#[test]
fn null_numerics_read_back_as_zero_rather_than_failing() {
    let store = Store::open_in_memory(Database::Historical).unwrap();
    store
        .conn
        .execute(
            "INSERT INTO samples (ts, status) VALUES ('2026-09-06T00:00:00.000Z', 'Full')",
            [],
        )
        .unwrap();

    let row = store.latest().unwrap().unwrap();
    assert_eq!(row.estimated_cycle_count, 0.0);
    assert_eq!(row.charge_pct, 0.0);
    assert_eq!(row.energy_design_wh, 0.0);
    assert_eq!(row.cycle_count, None);
    assert_eq!(row.battery_temp_c, None);
}

#[test]
fn the_two_databases_get_their_own_ladders() {
    let historical = Store::open_in_memory(Database::Historical).unwrap();
    let debug = Store::open_in_memory(Database::Debug).unwrap();

    let version = |store: &Store| -> u32 {
        store
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap()
    };

    assert_eq!(version(&historical), 7);
    assert_eq!(version(&debug), 5);
}

#[test]
fn opening_creates_missing_parent_directories() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("nested/deeper/battery.db");

    let store = Store::open(&path, Database::Historical).unwrap();
    store
        .insert(&sample("2026-09-06T00:00:00.000Z", 50.0))
        .unwrap();

    assert!(path.exists());
    assert!(store.latest().unwrap().is_some());
}

#[test]
fn a_reopened_database_still_holds_its_rows() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("battery.db");

    {
        let store = Store::open(&path, Database::Historical).unwrap();
        store
            .insert(&sample("2026-09-06T00:00:00.000Z", 50.0))
            .unwrap();
        store.checkpoint().unwrap();
    }

    let reopened = Store::open(&path, Database::Historical).unwrap();
    assert_eq!(reopened.latest().unwrap().unwrap().charge_pct, 50.0);
}
