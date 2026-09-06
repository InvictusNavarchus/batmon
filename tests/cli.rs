//! End-to-end tests of the shipped binary.
//!
//! Everything else tests the library. These run the actual executable, which is
//! the only way to cover argument parsing, the process exit code, logging setup
//! and the one-shot path — and the only thing that proves the artefact the
//! installer runs actually works.
//!
//! The battery is a fixture pointed at by `BATMON_SYSFS_PATH`, so results do not
//! depend on the hardware running the tests. `/proc` is the real one, since
//! every machine has a usable one.

// Values are read straight back out of SQLite, so exact comparison is the
// assertion rather than a floating-point hazard.
#![allow(clippy::float_cmp)]

use std::path::Path;
use std::process::Command;

use rusqlite::Connection;
use tempfile::TempDir;

/// The binary under test, as built by cargo for this integration test.
const BATMON: &str = env!("CARGO_BIN_EXE_batmon");

/// A home directory for the databases and a fixture battery to read.
fn environment() -> (TempDir, TempDir) {
    let home = TempDir::new().unwrap();
    let battery = TempDir::new().unwrap();

    for (attribute, value) in [
        ("type", "Battery"),
        ("status", "Discharging"),
        ("present", "1"),
        ("capacity", "72"),
        ("energy_now", "42000000"),
        ("energy_full", "58000000"),
        ("energy_full_design", "60000000"),
        ("power_now", "12500000"),
        ("voltage_now", "11900000"),
        ("voltage_min_design", "11550000"),
        ("cycle_count", "134"),
    ] {
        std::fs::write(battery.path().join(attribute), format!("{value}\n")).unwrap();
    }

    (home, battery)
}

fn batmon(home: &Path, battery: &Path) -> Command {
    let mut command = Command::new(BATMON);
    command
        .env("HOME", home)
        .env("BATMON_SYSFS_PATH", battery)
        .env("BATMON_LOG", "batmon=info");
    command
}

fn database_dir(home: &Path) -> std::path::PathBuf {
    home.join(".local/share/batmon")
}

fn query<T: rusqlite::types::FromSql>(path: &Path, sql: &str) -> T {
    let connection = Connection::open(path).unwrap();
    connection.query_row(sql, [], |row| row.get(0)).unwrap()
}

#[test]
fn help_describes_the_available_modes() {
    let output = Command::new(BATMON).arg("--help").output().unwrap();

    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("--oneshot"), "{help}");
    assert!(help.contains("flight recorder"), "{help}");
}

#[test]
fn version_is_reported() {
    let output = Command::new(BATMON).arg("--version").output().unwrap();

    assert!(output.status.success());
    let version = String::from_utf8_lossy(&output.stdout);
    assert!(version.starts_with("batmon "), "{version}");
}

#[test]
fn an_unknown_flag_is_rejected() {
    let output = Command::new(BATMON).arg("--nonsense").output().unwrap();

    assert!(
        !output.status.success(),
        "unknown flags must not be ignored"
    );
}

#[test]
fn oneshot_creates_both_databases_and_records_one_sample() {
    let (home, battery) = environment();

    let output = batmon(home.path(), battery.path())
        .arg("--oneshot")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let databases = database_dir(home.path());
    let historical = databases.join("battery.db");
    let debug = databases.join("debug.db");

    assert_eq!(
        query::<i64>(&historical, "PRAGMA user_version"),
        7,
        "history must be migrated to the shipped schema"
    );
    assert_eq!(query::<i64>(&debug, "PRAGMA user_version"), 5);

    assert_eq!(query::<i64>(&historical, "SELECT count(*) FROM samples"), 1);
    assert_eq!(query::<i64>(&debug, "SELECT count(*) FROM samples"), 1);
}

#[test]
fn oneshot_records_the_battery_it_was_pointed_at() {
    let (home, battery) = environment();

    batmon(home.path(), battery.path())
        .arg("--oneshot")
        .status()
        .unwrap();

    let historical = database_dir(home.path()).join("battery.db");

    assert_eq!(
        query::<f64>(&historical, "SELECT charge_pct FROM samples"),
        72.0
    );
    assert_eq!(
        query::<String>(&historical, "SELECT status FROM samples"),
        "Discharging"
    );
    assert_eq!(
        query::<String>(&historical, "SELECT power_state FROM samples"),
        "discharging"
    );
    assert_eq!(
        query::<i64>(&historical, "SELECT cycle_count FROM samples"),
        134
    );
    assert_eq!(
        query::<f64>(&historical, "SELECT energy_design_wh FROM samples"),
        60.0
    );
    // 58 of 60 Wh remaining capacity.
    assert!((query::<f64>(&historical, "SELECT health_pct FROM samples") - 96.67).abs() < 0.01);
}

#[test]
fn oneshot_captures_a_usable_utilisation_figure() {
    // The whole reason one-shot samples twice: a single reading of a monotonic
    // counter cannot yield a rate, and the installer uses this to prove the
    // machine works.
    let (home, battery) = environment();

    batmon(home.path(), battery.path())
        .arg("--oneshot")
        .status()
        .unwrap();

    let debug = database_dir(home.path()).join("debug.db");
    let cpu_pct: Option<f64> = query(&debug, "SELECT cpu_pct FROM samples");

    assert!(cpu_pct.is_some(), "utilisation must not be null");
}

#[test]
fn oneshot_is_idempotent_and_appends() {
    let (home, battery) = environment();

    for _ in 0..3 {
        assert!(
            batmon(home.path(), battery.path())
                .arg("--oneshot")
                .status()
                .unwrap()
                .success()
        );
    }

    let historical = database_dir(home.path()).join("battery.db");
    assert_eq!(query::<i64>(&historical, "SELECT count(*) FROM samples"), 3);
    assert_eq!(query::<i64>(&historical, "PRAGMA user_version"), 7);
}

#[test]
fn an_absent_battery_exits_cleanly_without_creating_databases() {
    let home = TempDir::new().unwrap();
    let absent = Path::new("/nonexistent/batmon/BAT0");

    let output = batmon(home.path(), absent)
        .arg("--oneshot")
        .output()
        .unwrap();

    assert!(output.status.success(), "an absent battery is not an error");
    assert!(
        !database_dir(home.path()).join("battery.db").exists(),
        "nothing to record means nothing to create"
    );
}

#[test]
fn the_daemon_records_on_a_cadence_and_stops_on_sigterm() {
    let (home, battery) = environment();

    let mut child = batmon(home.path(), battery.path()).spawn().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(3_500));

    let terminated = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(terminated.success());

    let status = child.wait().unwrap();
    assert!(status.success(), "a terminated daemon must exit cleanly");

    let databases = database_dir(home.path());
    let debug = databases.join("debug.db");

    let rows: i64 = query(&debug, "SELECT count(*) FROM samples");
    assert!(
        (3..=5).contains(&rows),
        "expected roughly one row per second, got {rows}"
    );

    // History is seeded on the first tick and not written again for a minute.
    assert_eq!(
        query::<i64>(
            &databases.join("battery.db"),
            "SELECT count(*) FROM samples"
        ),
        1
    );

    // A clean shutdown checkpoints, so no write-ahead log is left behind.
    assert!(
        !databases.join("debug.db-wal").exists(),
        "shutdown should have checkpointed the write-ahead log"
    );
}

#[test]
fn daemon_ticks_stay_on_a_one_second_cadence() {
    /// Milliseconds since midnight, from a fixed-width `toISOString` stamp.
    fn clock_ms(ts: &str) -> i64 {
        let field = |range: std::ops::Range<usize>| ts[range].parse::<i64>().unwrap();
        let (hours, minutes, seconds, millis) =
            (field(11..13), field(14..16), field(17..19), field(20..23));
        ((hours * 60 + minutes) * 60 + seconds) * 1_000 + millis
    }

    let (home, battery) = environment();

    let mut child = batmon(home.path(), battery.path()).spawn().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(4_500));
    Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    child.wait().unwrap();

    let debug = database_dir(home.path()).join("debug.db");
    let connection = Connection::open(&debug).unwrap();
    let stamps: Vec<String> = connection
        .prepare("SELECT ts FROM samples ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();

    assert!(
        stamps.len() >= 3,
        "not enough samples to judge cadence: {stamps:?}"
    );

    // A loop that sleeps for an interval rather than toward a deadline walks
    // forward by the cost of each tick. Measured against the TypeScript daemon
    // over six hours, that cost it 113 samples — roughly 7.5 minutes of lost
    // coverage a day. Every gap here must be a second, not a second plus
    // however long the tick took.
    for pair in stamps.windows(2) {
        let gap = clock_ms(&pair[1]) - clock_ms(&pair[0]);
        assert!(
            (900..=1_100).contains(&gap),
            "interval of {gap} ms between {} and {}",
            pair[0],
            pair[1]
        );
    }
}
