use super::*;

fn memory() -> Connection {
    Connection::open_in_memory().unwrap()
}

fn columns(conn: &Connection, table: &str) -> Vec<String> {
    let mut names = Vec::new();
    conn.pragma(None, "table_info", table, |row| {
        names.push(row.get::<_, String>("name")?);
        Ok(())
    })
    .unwrap();
    names
}

fn indexes(conn: &Connection, table: &str) -> Vec<String> {
    let mut names = Vec::new();
    conn.pragma(None, "index_list", table, |row| {
        names.push(row.get::<_, String>("name")?);
        Ok(())
    })
    .unwrap();
    names
}

fn user_version(conn: &Connection) -> u32 {
    conn.pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap()
}

#[test]
fn historical_ladder_reaches_version_seven_with_every_column() {
    let conn = memory();
    migrate(&conn, HISTORICAL_MIGRATIONS).unwrap();

    assert_eq!(user_version(&conn), 7);

    let cols = columns(&conn, "samples");
    for expected in [
        "id",
        "ts",
        "charge_pct",
        "health_pct",
        "energy_design_wh",
        "voltage_design_v",
        "battery_temp_c",
        "estimated_cycle_count",
        "cpu_pct",
        "mem_pct",
        "top_processes",
        "cpu_freq_mhz",
        "gpu_pct",
        "gpu_power_w",
        "load1",
        "power_state",
        "boot_id",
        "uptime_s",
    ] {
        assert!(cols.iter().any(|c| c == expected), "missing {expected}");
    }
    assert!(indexes(&conn, "samples").iter().any(|i| i == "idx_ts"));
}

#[test]
fn debug_ladder_reaches_version_five_with_every_column() {
    let conn = memory();
    migrate(&conn, DEBUG_MIGRATIONS).unwrap();

    assert_eq!(user_version(&conn), 5);

    let cols = columns(&conn, "samples");
    for expected in [
        "id",
        "ts",
        "charge_pct",
        "estimated_cycle_count",
        "load1",
        "power_state",
        "boot_id",
        "uptime_s",
    ] {
        assert!(cols.iter().any(|c| c == expected), "missing {expected}");
    }
    assert!(
        indexes(&conn, "samples")
            .iter()
            .any(|i| i == "idx_debug_ts")
    );
}

#[test]
fn running_the_ladder_twice_changes_nothing() {
    let conn = memory();
    migrate(&conn, HISTORICAL_MIGRATIONS).unwrap();
    let after_first = columns(&conn, "samples");

    migrate(&conn, HISTORICAL_MIGRATIONS).unwrap();

    assert_eq!(user_version(&conn), 7);
    assert_eq!(columns(&conn, "samples"), after_first);
}

#[test]
fn resumes_from_an_intermediate_version() {
    let conn = memory();
    (HISTORICAL_MIGRATIONS[0].up)(&conn).unwrap();
    (HISTORICAL_MIGRATIONS[1].up)(&conn).unwrap();
    conn.pragma_update(None, "user_version", 2).unwrap();

    assert!(!columns(&conn, "samples").iter().any(|c| c == "cpu_pct"));

    migrate(&conn, HISTORICAL_MIGRATIONS).unwrap();

    assert_eq!(user_version(&conn), 7);
    let cols = columns(&conn, "samples");
    for expected in ["cpu_pct", "boot_id", "uptime_s"] {
        assert!(cols.iter().any(|c| c == expected), "missing {expected}");
    }
}

#[test]
fn renaming_legacy_columns_preserves_the_rows() {
    let conn = memory();
    conn.execute_batch(
        "CREATE TABLE samples (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            ts TEXT NOT NULL,
            percentage REAL,
            capacity_pct REAL,
            energy_design REAL,
            voltage_design REAL,
            temperature_c REAL
        );
        INSERT INTO samples (ts, percentage, capacity_pct, energy_design, voltage_design, temperature_c)
        VALUES ('2026-08-28T00:00:00.000Z', 85.5, 95.0, 52.4, 11.4, 32.1);",
    )
    .unwrap();
    conn.pragma_update(None, "user_version", 4).unwrap();

    migrate(&conn, HISTORICAL_MIGRATIONS).unwrap();

    let (charge, health, design_wh, design_v, temp): (f64, f64, f64, f64, f64) = conn
        .query_row(
            "SELECT charge_pct, health_pct, energy_design_wh, voltage_design_v, battery_temp_c
             FROM samples WHERE id = 1",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();

    assert!((charge - 85.5).abs() < f64::EPSILON);
    assert!((health - 95.0).abs() < f64::EPSILON);
    assert!((design_wh - 52.4).abs() < f64::EPSILON);
    assert!((design_v - 11.4).abs() < f64::EPSILON);
    assert!((temp - 32.1).abs() < f64::EPSILON);
}

#[test]
fn renames_samples_debug_over_an_empty_placeholder_samples_table() {
    let conn = memory();
    conn.execute_batch(
        "CREATE TABLE samples_debug (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            ts TEXT NOT NULL,
            charge_pct REAL
        );
        INSERT INTO samples_debug (ts, charge_pct) VALUES ('2026-08-28T00:00:00.000Z', 77.7);
        CREATE TABLE samples (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            ts TEXT NOT NULL,
            charge_pct REAL
        );",
    )
    .unwrap();
    conn.pragma_update(None, "user_version", 2).unwrap();

    migrate(&conn, DEBUG_MIGRATIONS).unwrap();

    let charge: f64 = conn
        .query_row("SELECT charge_pct FROM samples WHERE id = 1", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!((charge - 77.7).abs() < f64::EPSILON);
    assert!(!has_table(&conn, "samples_debug").unwrap());
}

#[test]
fn the_flight_recorder_index_survives_the_table_rename() {
    // The rename's empty-placeholder branch drops the destination table,
    // taking migration 1's index with it. Without recreating it, that database
    // ends up with a different schema from every other.
    let conn = memory();
    conn.execute_batch(
        "CREATE TABLE samples_debug (
            id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL, charge_pct REAL
        );
        INSERT INTO samples_debug (ts, charge_pct) VALUES ('2026-08-28T00:00:00.000Z', 77.7);
        CREATE TABLE samples (
            id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL, charge_pct REAL
        );
        CREATE INDEX idx_debug_ts ON samples(ts);",
    )
    .unwrap();
    conn.pragma_update(None, "user_version", 2).unwrap();

    migrate(&conn, DEBUG_MIGRATIONS).unwrap();

    assert!(
        indexes(&conn, "samples")
            .iter()
            .any(|i| i == "idx_debug_ts"),
        "the rename dropped migration 1's index without recreating it"
    );
    // And the rows still made it across.
    let count: i64 = conn
        .query_row("SELECT count(*) FROM samples", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn merges_into_a_populated_destination_rather_than_discarding_it() {
    // The other half of rename_table_if_exists: both tables hold rows, so the
    // destination's rows must survive and the source's must be folded in.
    let conn = memory();
    conn.execute_batch(
        "CREATE TABLE samples_debug (
            id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL, charge_pct REAL
        );
        INSERT INTO samples_debug (id, ts, charge_pct) VALUES (2, 'b', 22.2);
        CREATE TABLE samples (
            id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL, charge_pct REAL
        );
        INSERT INTO samples (id, ts, charge_pct) VALUES (1, 'a', 11.1);",
    )
    .unwrap();
    conn.pragma_update(None, "user_version", 2).unwrap();

    migrate(&conn, DEBUG_MIGRATIONS).unwrap();

    let count: i64 = conn
        .query_row("SELECT count(*) FROM samples", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 2, "both rows must survive the merge");
    assert!(!has_table(&conn, "samples_debug").unwrap());
}

#[test]
fn migration_versions_are_contiguous_and_ordered() {
    // A gap or a duplicate would silently skip a step on a field database.
    for ladder in [HISTORICAL_MIGRATIONS, DEBUG_MIGRATIONS] {
        for (index, migration) in ladder.iter().enumerate() {
            assert_eq!(
                migration.version,
                u32::try_from(index).unwrap() + 1,
                "{} is out of sequence",
                migration.name
            );
        }
    }
}

#[test]
fn a_database_already_at_the_latest_version_is_left_alone() {
    // Idempotence: re-running the ladder against a database already at the
    // head version must change nothing and must leave stored rows alone.
    // Note the head schema here is built by the ladder itself, so this does
    // not prove that a v7 database written before the port opens untouched --
    // that needs a captured legacy fixture, which does not exist yet.
    let conn = memory();
    migrate(&conn, HISTORICAL_MIGRATIONS).unwrap();
    conn.execute(
        "INSERT INTO samples (ts, charge_pct) VALUES ('2026-09-06T00:00:00.000Z', 64.0)",
        [],
    )
    .unwrap();

    migrate(&conn, HISTORICAL_MIGRATIONS).unwrap();

    let count: i64 = conn
        .query_row("SELECT count(*) FROM samples", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(user_version(&conn), 7);
}
