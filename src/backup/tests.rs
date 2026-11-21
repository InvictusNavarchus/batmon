use std::path::PathBuf;

use rusqlite::Connection;
use tempfile::TempDir;

use super::*;
use crate::db::Store;
use crate::migrations::{DEBUG_MIGRATIONS, HISTORICAL_MIGRATIONS, migrate};

fn paths(db_dir: &Path) -> Paths {
    Paths {
        power_supply_base: PathBuf::from("/ps"),
        battery: PathBuf::from("/ps/BAT0"),
        hwmon_base: PathBuf::from("/hwmon"),
        cpu_base: PathBuf::from("/cpu"),
        drm_base: PathBuf::from("/drm"),
        proc_base: PathBuf::from("/proc"),
        db_dir: db_dir.to_path_buf(),
    }
}

/// A WAL database migrated to `version`, holding one row marked `marker`.
///
/// The connection is returned open with checkpointing disabled, so the row is
/// committed but still only in the WAL.
fn database_at(path: &Path, database: Database, version: u32, marker: &str) -> Connection {
    let ladder = match database {
        Database::Historical => HISTORICAL_MIGRATIONS,
        Database::Debug => DEBUG_MIGRATIONS,
    };
    let conn = Connection::open(path).unwrap();
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA wal_autocheckpoint = 0;")
        .unwrap();
    migrate(&conn, &ladder[..version as usize]).unwrap();
    conn.execute("INSERT INTO samples (ts) VALUES (?1)", [marker])
        .unwrap();
    conn
}

fn version_of(path: &Path) -> u32 {
    Connection::open(path)
        .unwrap()
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap()
}

fn markers(path: &Path) -> Vec<String> {
    Connection::open(path)
        .unwrap()
        .prepare("SELECT ts FROM samples ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

/// Historical one version behind, debug current: the usual upgrade shape.
fn outdated_pair(dir: &Path) -> (Connection, Connection) {
    let historical = database_at(
        &dir.join("battery.db"),
        Database::Historical,
        Database::Historical.latest_version() - 1,
        "history-row",
    );
    let debug = database_at(
        &dir.join("debug.db"),
        Database::Debug,
        Database::Debug.latest_version(),
        "debug-row",
    );
    (historical, debug)
}

#[test]
fn nothing_is_backed_up_without_databases() {
    let tmp = TempDir::new().unwrap();

    assert_eq!(before_migration(&paths(tmp.path())).unwrap(), None);
    assert!(!tmp.path().join("backups").exists());
}

#[test]
fn nothing_is_backed_up_when_both_databases_are_current() {
    let tmp = TempDir::new().unwrap();
    Store::open(&tmp.path().join("battery.db"), Database::Historical).unwrap();
    Store::open(&tmp.path().join("debug.db"), Database::Debug).unwrap();

    assert_eq!(before_migration(&paths(tmp.path())).unwrap(), None);
    assert!(!tmp.path().join("backups").exists());
}

#[test]
fn both_databases_are_copied_when_either_needs_migrating() {
    let tmp = TempDir::new().unwrap();
    let _open = outdated_pair(tmp.path());
    let older = Database::Historical.latest_version() - 1;

    let snapshot = before_migration(&paths(tmp.path())).unwrap().unwrap();

    assert_eq!(snapshot.parent().unwrap(), snapshot_dir(tmp.path()));
    let name = snapshot.file_name().unwrap().to_str().unwrap();
    assert!(
        name.ends_with(&format!(
            "-battery-v{older}-debug-v{}",
            Database::Debug.latest_version()
        )),
        "{name}"
    );

    // Rows still only in the WAL made it into the copies.
    assert_eq!(markers(&snapshot.join("battery.db")), ["history-row"]);
    assert_eq!(markers(&snapshot.join("debug.db")), ["debug-row"]);
    assert_eq!(version_of(&snapshot.join("battery.db")), older);
    // Self-contained files, and nothing left staged.
    assert!(!snapshot.join("battery.db-wal").exists());
    assert!(!tmp.path().join(STAGING).exists());
    // Backing up does not migrate.
    assert_eq!(version_of(&tmp.path().join("battery.db")), older);
}

#[test]
fn a_missing_database_is_skipped_and_named_as_absent() {
    let tmp = TempDir::new().unwrap();
    let _historical = database_at(
        &tmp.path().join("battery.db"),
        Database::Historical,
        1,
        "row",
    );

    let snapshot = before_migration(&paths(tmp.path())).unwrap().unwrap();

    assert!(snapshot.join("battery.db").exists());
    assert!(!snapshot.join("debug.db").exists());
    assert!(
        snapshot
            .to_str()
            .unwrap()
            .ends_with("-battery-v1-debug-none")
    );
}

#[test]
fn a_retry_keeps_the_original_snapshot() {
    let tmp = TempDir::new().unwrap();
    let (historical, _debug) = outdated_pair(tmp.path());

    let first = before_migration(&paths(tmp.path())).unwrap().unwrap();
    // Rows recorded after the first attempt must not replace the original.
    historical
        .execute("INSERT INTO samples (ts) VALUES ('later')", [])
        .unwrap();
    let second = before_migration(&paths(tmp.path())).unwrap().unwrap();

    assert_eq!(first, second);
    assert_eq!(snapshots(&snapshot_dir(tmp.path())).unwrap().len(), 1);
    assert_eq!(markers(&first.join("battery.db")), ["history-row"]);
}

#[test]
fn stale_staging_from_an_interrupted_attempt_is_replaced() {
    let tmp = TempDir::new().unwrap();
    let _open = outdated_pair(tmp.path());
    let staging = tmp.path().join(STAGING);
    fs::create_dir_all(&staging).unwrap();
    fs::write(staging.join("battery.db"), "half-written").unwrap();

    let snapshot = before_migration(&paths(tmp.path())).unwrap().unwrap();

    assert_eq!(markers(&snapshot.join("battery.db")), ["history-row"]);
    assert!(!staging.exists());
}

#[test]
fn a_failed_publication_leaves_no_snapshot_and_no_staging() {
    let tmp = TempDir::new().unwrap();
    let _open = outdated_pair(tmp.path());
    // A file where the snapshot directory belongs makes publishing fail after
    // both copies were staged.
    fs::create_dir_all(tmp.path().join("backups")).unwrap();
    fs::write(snapshot_dir(tmp.path()), "in the way").unwrap();

    assert!(before_migration(&paths(tmp.path())).is_err());
    assert!(!tmp.path().join(STAGING).exists());
    assert!(snapshot_dir(tmp.path()).is_file());
}

#[test]
fn retention_keeps_the_newest_snapshots_and_ignores_everything_else() {
    let tmp = TempDir::new().unwrap();
    let _open = outdated_pair(tmp.path());
    let published = snapshot_dir(tmp.path());
    for old in [
        "20240101T000000Z-battery-v1-debug-v1",
        "20240201T000000Z-battery-v2-debug-v2",
        "20240301T000000Z-battery-v3-debug-v3",
    ] {
        fs::create_dir_all(published.join(old)).unwrap();
    }
    // Not batmon's: a manual backup beside the snapshot directory, and an
    // unrecognised entry inside it.
    let manual = tmp.path().join("backups/battery_20260827_183431.db");
    fs::write(&manual, "manual").unwrap();
    fs::create_dir_all(published.join("keep-me")).unwrap();

    let snapshot = before_migration(&paths(tmp.path())).unwrap().unwrap();

    let mut kept = snapshots(&published).unwrap();
    kept.sort();
    assert_eq!(kept.len(), KEEP);
    assert_eq!(kept.last().unwrap(), &snapshot);
    assert!(
        !published
            .join("20240101T000000Z-battery-v1-debug-v1")
            .exists()
    );
    assert!(published.join("keep-me").exists());
    assert!(manual.exists());
}

#[test]
fn snapshot_names_are_recognised_by_their_timestamp() {
    assert!(is_snapshot_name("20261121T190400Z-battery-v11-debug-v10"));
    assert!(!is_snapshot_name("keep-me"));
    assert!(!is_snapshot_name("20261121T190400Z"));
    assert!(!is_snapshot_name("2026112XT190400Z-battery-v1-debug-v1"));
    assert!(!is_snapshot_name(".pre-migration-staging"));
}

#[test]
fn a_corrupt_copy_is_rejected() {
    let tmp = TempDir::new().unwrap();
    let copy = tmp.path().join("copy.db");
    fs::write(&copy, "not a database").unwrap();

    assert!(verify(&copy, 1).is_err());
}

#[test]
fn a_copy_at_the_wrong_version_is_rejected() {
    let tmp = TempDir::new().unwrap();
    let copy = tmp.path().join("copy.db");
    drop(database_at(&copy, Database::Historical, 2, "row"));

    assert!(verify(&copy, 2).is_ok());
    assert!(verify(&copy, 3).is_err());
}
