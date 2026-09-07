//! Schema evolution for both databases, tracked with SQLite's `user_version`.
//!
//! This ladder is ported step for step from the TypeScript implementation,
//! including the awkward parts. Real databases exist in the field at historical
//! version 7 and debug version 5, and a migration that runs differently here
//! than it did there does not produce an error — it produces a subtly different
//! schema on someone's laptop. Cleverness is a liability in this module; the
//! only correct behaviour is the behaviour that already shipped.

use rusqlite::{Connection, Result};

/// One forward-only schema step.
///
/// `up` is a plain function pointer rather than a boxed closure: migrations are
/// static, known at compile time, and never capture.
pub struct Migration {
    pub version: u32,
    pub name: &'static str,
    pub up: fn(&Connection) -> Result<()>,
}

impl std::fmt::Debug for Migration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Migration")
            .field("version", &self.version)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// Whether `table` currently has a column named `column`.
fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut found = false;
    conn.pragma(None, "table_info", table, |row| {
        if row.get::<_, String>("name")? == column {
            found = true;
        }
        Ok(())
    })?;
    Ok(found)
}

/// Whether a table by this name exists.
fn has_table(conn: &Connection, table: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
        [table],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Add a column unless it is already there.
///
/// Identifiers are interpolated rather than bound because SQLite does not accept
/// parameters in DDL. Every argument reaching this function is a string literal
/// from the ladder below, never user input.
fn add_column_if_not_exists(
    conn: &Connection,
    table: &str,
    column: &str,
    column_type: &str,
) -> Result<()> {
    if !has_column(conn, table, column)? {
        conn.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {column_type};"
        ))?;
    }
    Ok(())
}

/// Rename a column, but only when the old name is present and the new one is not.
///
/// Both guards matter. Re-running must be a no-op, and a database that already
/// has the new name — because it was created fresh rather than migrated — must
/// not be touched.
fn rename_column_if_exists(conn: &Connection, table: &str, old: &str, new: &str) -> Result<()> {
    if !has_table(conn, table)? {
        return Ok(());
    }
    if has_column(conn, table, old)? && !has_column(conn, table, new)? {
        conn.execute_batch(&format!(
            "ALTER TABLE {table} RENAME COLUMN {old} TO {new};"
        ))?;
    }
    Ok(())
}

/// Rename a table, reconciling the case where the destination already exists.
///
/// The awkward branch is real and load-bearing. An earlier release could create
/// an empty `samples` table alongside a populated `samples_debug`, so a plain
/// rename would fail. An empty destination is dropped and replaced; a populated
/// one is merged into with `INSERT OR IGNORE` and the source dropped, which
/// keeps whichever rows the destination already had.
fn rename_table_if_exists(conn: &Connection, old: &str, new: &str) -> Result<()> {
    if !has_table(conn, old)? {
        return Ok(());
    }

    if !has_table(conn, new)? {
        conn.execute_batch(&format!("ALTER TABLE {old} RENAME TO {new};"))?;
        return Ok(());
    }

    let rows: i64 = conn.query_row(&format!("SELECT count(*) FROM {new};"), [], |row| {
        row.get(0)
    })?;

    if rows == 0 {
        conn.execute_batch(&format!(
            "DROP TABLE {new}; ALTER TABLE {old} RENAME TO {new};"
        ))?;
    } else {
        conn.execute_batch(&format!(
            "INSERT OR IGNORE INTO {new} SELECT * FROM {old}; DROP TABLE {old};"
        ))?;
    }
    Ok(())
}

/// Rename the five columns that were standardised across both databases.
fn standardise_column_names(conn: &Connection, table: &str) -> Result<()> {
    for (old, new) in [
        ("percentage", "charge_pct"),
        ("capacity_pct", "health_pct"),
        ("energy_design", "energy_design_wh"),
        ("voltage_design", "voltage_design_v"),
        ("temperature_c", "battery_temp_c"),
    ] {
        rename_column_if_exists(conn, table, old, new)?;
    }
    Ok(())
}

// ── historical database (battery.db) ─────────────────────────────────

/// Ladder for the permanent downsampled history. Field databases sit at 7.
pub const HISTORICAL_MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "create_initial_samples_table",
        up: |conn| {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS samples (
                    id               INTEGER PRIMARY KEY AUTOINCREMENT,
                    ts               TEXT    NOT NULL,
                    charge_pct       REAL,
                    status           TEXT,
                    energy_wh        REAL,
                    energy_full_wh   REAL,
                    energy_design_wh REAL,
                    power_w          REAL,
                    voltage_v        REAL,
                    voltage_design_v REAL,
                    cycle_count      INTEGER,
                    battery_temp_c   REAL,
                    health_pct       REAL,
                    is_charging      INTEGER,
                    is_present       INTEGER,
                    time_to_empty_s  INTEGER,
                    time_to_full_s   INTEGER,
                    cpu_temp_c       REAL,
                    gpu_temp_c       REAL,
                    nvme_temp_c      REAL
                );
                CREATE INDEX IF NOT EXISTS idx_ts ON samples(ts);",
            )
        },
    },
    Migration {
        version: 2,
        name: "add_estimated_cycle_count",
        up: |conn| add_column_if_not_exists(conn, "samples", "estimated_cycle_count", "REAL"),
    },
    Migration {
        version: 3,
        name: "add_glances_and_system_telemetry",
        up: |conn| {
            add_column_if_not_exists(conn, "samples", "cpu_pct", "REAL")?;
            add_column_if_not_exists(conn, "samples", "mem_pct", "REAL")?;
            add_column_if_not_exists(conn, "samples", "top_processes", "TEXT")
        },
    },
    Migration {
        version: 4,
        name: "add_flight_telemetry_metrics",
        up: |conn| {
            add_column_if_not_exists(conn, "samples", "cpu_freq_mhz", "REAL")?;
            add_column_if_not_exists(conn, "samples", "gpu_pct", "REAL")?;
            add_column_if_not_exists(conn, "samples", "gpu_power_w", "REAL")?;
            add_column_if_not_exists(conn, "samples", "load1", "REAL")
        },
    },
    Migration {
        version: 5,
        name: "standardize_column_names",
        up: |conn| standardise_column_names(conn, "samples"),
    },
    Migration {
        version: 6,
        name: "add_power_state",
        up: |conn| add_column_if_not_exists(conn, "samples", "power_state", "TEXT"),
    },
    Migration {
        version: 7,
        name: "add_boot_id_and_uptime",
        up: |conn| {
            add_column_if_not_exists(conn, "samples", "boot_id", "TEXT")?;
            add_column_if_not_exists(conn, "samples", "uptime_s", "REAL")
        },
    },
];

// ── debug flight recorder (debug.db) ─────────────────────────────────

/// Ladder for the rolling flight recorder. Field databases sit at 5.
pub const DEBUG_MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "create_debug_samples_table",
        up: |conn| {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS samples (
                    id                    INTEGER PRIMARY KEY AUTOINCREMENT,
                    ts                    TEXT    NOT NULL,
                    charge_pct            REAL,
                    status                TEXT,
                    energy_wh             REAL,
                    energy_full_wh        REAL,
                    energy_design_wh      REAL,
                    power_w               REAL,
                    voltage_v             REAL,
                    voltage_design_v      REAL,
                    cycle_count           INTEGER,
                    estimated_cycle_count REAL,
                    battery_temp_c        REAL,
                    health_pct            REAL,
                    is_charging           INTEGER,
                    is_present            INTEGER,
                    time_to_empty_s       INTEGER,
                    time_to_full_s        INTEGER,
                    cpu_temp_c            REAL,
                    gpu_temp_c            REAL,
                    nvme_temp_c           REAL,
                    cpu_pct               REAL,
                    mem_pct               REAL,
                    top_processes         TEXT,
                    cpu_freq_mhz          REAL,
                    gpu_pct               REAL,
                    gpu_power_w           REAL,
                    load1                 REAL
                );
                CREATE INDEX IF NOT EXISTS idx_debug_ts ON samples(ts);",
            )
        },
    },
    Migration {
        version: 2,
        name: "standardize_debug_column_names",
        up: |conn| {
            // Both names are visited because the rename to `samples` happens in
            // the next step; a database arriving here could be under either.
            standardise_column_names(conn, "samples")?;
            standardise_column_names(conn, "samples_debug")
        },
    },
    Migration {
        version: 3,
        name: "rename_samples_debug_to_samples",
        up: |conn| {
            rename_table_if_exists(conn, "samples_debug", "samples")?;
            // Migration 1 created this index on `samples`. When the rename above
            // takes the empty-placeholder branch it drops that table, and the
            // index goes with it — leaving a 1 Hz recorder to prune by full
            // table scan for the rest of the database's life. The TypeScript
            // had the same hole; recreating the index here is a deliberate
            // divergence, and a schema-only one that changes no stored value.
            if has_table(conn, "samples")? {
                conn.execute_batch("CREATE INDEX IF NOT EXISTS idx_debug_ts ON samples(ts);")?;
            }
            Ok(())
        },
    },
    Migration {
        version: 4,
        name: "add_power_state",
        up: |conn| add_column_if_not_exists(conn, "samples", "power_state", "TEXT"),
    },
    Migration {
        version: 5,
        name: "add_boot_id_and_uptime",
        up: |conn| {
            add_column_if_not_exists(conn, "samples", "boot_id", "TEXT")?;
            add_column_if_not_exists(conn, "samples", "uptime_s", "REAL")
        },
    },
];

/// Apply every migration newer than the database's recorded `user_version`.
///
/// Each step commits its own version bump rather than the whole ladder running
/// in one transaction. That matches the TypeScript behaviour exactly: a failure
/// half way leaves the completed steps applied and recorded, so the next start
/// resumes rather than repeating work that already succeeded.
pub fn migrate(conn: &Connection, migrations: &[Migration]) -> Result<()> {
    let current: u32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;

    for migration in migrations {
        if migration.version > current {
            (migration.up)(conn)?;
            conn.pragma_update(None, "user_version", migration.version)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
