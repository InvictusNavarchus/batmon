//! Differential schema test: the Rust ladder against the TypeScript one.
//!
//! Unit tests assert the Rust migrations produce the schema we *expect*. This
//! asserts they produce the schema that already shipped, which is a different
//! and stronger claim — real databases exist in the field at historical version
//! 7 and debug version 5, and the port is only safe if both implementations
//! transform an identical starting state into an identical result.
//!
//! Temporary scaffolding. It runs the TypeScript migrator through Bun and is
//! removed together with legacy/ once the port lands.

use std::path::Path;
use std::process::Command;

use batmon::migrations::{DEBUG_MIGRATIONS, HISTORICAL_MIGRATIONS, migrate};
use rusqlite::Connection;
use tempfile::TempDir;

/// A column as SQLite reports it, in full — a differing type affinity or
/// default is exactly the kind of drift a name-only comparison would miss.
#[derive(Debug, PartialEq)]
struct Column {
    name: String,
    decl_type: Option<String>,
    not_null: bool,
    default: Option<String>,
    primary_key: i64,
}

#[derive(Debug, PartialEq)]
struct Schema {
    user_version: u32,
    tables: Vec<String>,
    columns: Vec<Column>,
    indexes: Vec<String>,
}

fn read_schema(path: &Path) -> Schema {
    let conn = Connection::open(path).unwrap();

    let user_version = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();

    let mut tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    tables.retain(|t| t != "sqlite_sequence");

    let mut columns = Vec::new();
    conn.pragma(None, "table_info", "samples", |row| {
        columns.push(Column {
            name: row.get("name")?,
            decl_type: row.get("type")?,
            not_null: row.get::<_, i64>("notnull")? != 0,
            default: row.get("dflt_value")?,
            primary_key: row.get("pk")?,
        });
        Ok(())
    })
    .unwrap();

    let mut indexes: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='index' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    indexes.sort();

    Schema {
        user_version,
        tables,
        columns,
        indexes,
    }
}

fn bun_available() -> bool {
    Command::new("bun")
        .arg("--version")
        .output()
        .is_ok_and(|out| out.status.success())
}

fn run_typescript_migrator(db: &Path, ladder: &str) {
    let output = Command::new("bun")
        .args(["legacy/tools/migrate.ts", db.to_str().unwrap(), ladder])
        .output()
        .expect("failed to invoke bun");

    assert!(
        output.status.success(),
        "typescript migrator failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Seed two identical databases, migrate one with each implementation, and
/// assert the resulting schemas are indistinguishable.
fn assert_ladders_agree(ladder: &str, seed: &str) {
    if !bun_available() {
        eprintln!("SKIPPED: bun is not on PATH, cannot diff against the TypeScript ladder");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let ts_db = tmp.path().join("typescript.db");
    let rs_db = tmp.path().join("rust.db");

    for db in [&ts_db, &rs_db] {
        let conn = Connection::open(db).unwrap();
        if !seed.is_empty() {
            conn.execute_batch(seed).unwrap();
        }
    }

    run_typescript_migrator(&ts_db, ladder);

    let conn = Connection::open(&rs_db).unwrap();
    let migrations = if ladder == "historical" {
        HISTORICAL_MIGRATIONS
    } else {
        DEBUG_MIGRATIONS
    };
    migrate(&conn, migrations).unwrap();
    drop(conn);

    assert_eq!(
        read_schema(&ts_db),
        read_schema(&rs_db),
        "schemas diverge for the {ladder} ladder"
    );
}

#[test]
fn historical_ladder_matches_on_a_fresh_database() {
    assert_ladders_agree("historical", "");
}

#[test]
fn debug_ladder_matches_on_a_fresh_database() {
    assert_ladders_agree("debug", "");
}

#[test]
fn historical_ladder_matches_when_resuming_from_version_two() {
    assert_ladders_agree(
        "historical",
        "CREATE TABLE samples (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            ts TEXT NOT NULL,
            charge_pct REAL,
            estimated_cycle_count REAL
        );
        CREATE INDEX idx_ts ON samples(ts);
        PRAGMA user_version = 2;",
    );
}

#[test]
fn historical_ladder_matches_when_renaming_legacy_columns() {
    // The version 5 standardisation, which is where a per-column guard
    // differing between implementations would show up.
    assert_ladders_agree(
        "historical",
        "CREATE TABLE samples (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            ts TEXT NOT NULL,
            percentage REAL,
            capacity_pct REAL,
            energy_design REAL,
            voltage_design REAL,
            temperature_c REAL
        );
        INSERT INTO samples (ts, percentage) VALUES ('2026-08-28T00:00:00.000Z', 85.5);
        PRAGMA user_version = 4;",
    );
}

#[test]
fn debug_ladder_matches_when_renaming_over_an_empty_placeholder() {
    assert_ladders_agree(
        "debug",
        "CREATE TABLE samples_debug (
            id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL, charge_pct REAL
        );
        INSERT INTO samples_debug (ts, charge_pct) VALUES ('2026-08-28T00:00:00.000Z', 77.7);
        CREATE TABLE samples (
            id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL, charge_pct REAL
        );
        PRAGMA user_version = 2;",
    );
}

#[test]
fn debug_ladder_matches_when_merging_two_populated_tables() {
    assert_ladders_agree(
        "debug",
        "CREATE TABLE samples_debug (
            id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL, charge_pct REAL
        );
        INSERT INTO samples_debug (id, ts, charge_pct) VALUES (2, 'b', 22.2);
        CREATE TABLE samples (
            id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL, charge_pct REAL
        );
        INSERT INTO samples (id, ts, charge_pct) VALUES (1, 'a', 11.1);
        PRAGMA user_version = 2;",
    );
}

#[test]
fn a_current_database_is_left_untouched_by_both_implementations() {
    // The upgrade path every existing install actually takes: already at the
    // head of the ladder, opened by the new binary.
    assert_ladders_agree(
        "historical",
        "CREATE TABLE samples (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            ts TEXT NOT NULL, charge_pct REAL, status TEXT, energy_wh REAL,
            energy_full_wh REAL, energy_design_wh REAL, power_w REAL, voltage_v REAL,
            voltage_design_v REAL, cycle_count INTEGER, battery_temp_c REAL,
            health_pct REAL, is_charging INTEGER, is_present INTEGER,
            time_to_empty_s INTEGER, time_to_full_s INTEGER, cpu_temp_c REAL,
            gpu_temp_c REAL, nvme_temp_c REAL, estimated_cycle_count REAL,
            cpu_pct REAL, mem_pct REAL, top_processes TEXT, cpu_freq_mhz REAL,
            gpu_pct REAL, gpu_power_w REAL, load1 REAL, power_state TEXT,
            boot_id TEXT, uptime_s REAL
        );
        CREATE INDEX idx_ts ON samples(ts);
        PRAGMA user_version = 7;",
    );
}
