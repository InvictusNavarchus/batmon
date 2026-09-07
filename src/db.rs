//! SQLite storage for both tiers.
//!
//! Two databases, one implementation. They differ only in which migration ladder
//! they run and how long their rows live: `battery.db` keeps a downsampled
//! record forever, `debug.db` keeps every second for a rolling window.

use std::path::{Path, PathBuf};

use jiff::{SignedDuration, Timestamp};
use rusqlite::{Connection, Row, named_params};

use crate::cycles::compute_estimated_cycles;
use crate::formats::iso8601_millis;
use crate::migrations::{DEBUG_MIGRATIONS, HISTORICAL_MIGRATIONS, Migration, migrate};
use crate::types::{PowerState, Sample};

/// Anything that can go wrong opening or using a store.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("could not create database directory {path}: {source}")]
    CreateDir {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("timestamp arithmetic failed: {0}")]
    Time(#[from] jiff::Error),
    #[error("retention window must be positive, got {hours} hours")]
    InvalidRetention { hours: i64 },
}

type Result<T> = std::result::Result<T, StoreError>;

/// Which of the two databases a store is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Database {
    /// `battery.db` — permanent, downsampled, tracks long-term wear.
    Historical,
    /// `debug.db` — one row per second, pruned to a rolling window.
    Debug,
}

impl Database {
    fn migrations(self) -> &'static [Migration] {
        match self {
            Self::Historical => HISTORICAL_MIGRATIONS,
            Self::Debug => DEBUG_MIGRATIONS,
        }
    }
}

/// Every column written by [`Store::insert`], in schema order.
const INSERT_SQL: &str = "INSERT INTO samples (
    ts, charge_pct, status, power_state, energy_wh, energy_full_wh, energy_design_wh,
    power_w, voltage_v, voltage_design_v, cycle_count, estimated_cycle_count,
    battery_temp_c, health_pct, is_charging, is_present, time_to_empty_s, time_to_full_s,
    cpu_temp_c, gpu_temp_c, nvme_temp_c, cpu_pct, mem_pct, top_processes,
    cpu_freq_mhz, gpu_pct, gpu_power_w, load1, boot_id, uptime_s
) VALUES (
    :ts, :charge_pct, :status, :power_state, :energy_wh, :energy_full_wh, :energy_design_wh,
    :power_w, :voltage_v, :voltage_design_v, :cycle_count, :estimated_cycle_count,
    :battery_temp_c, :health_pct, :is_charging, :is_present, :time_to_empty_s, :time_to_full_s,
    :cpu_temp_c, :gpu_temp_c, :nvme_temp_c, :cpu_pct, :mem_pct, :top_processes,
    :cpu_freq_mhz, :gpu_pct, :gpu_power_w, :load1, :boot_id, :uptime_s
)";

/// An open, migrated connection to one of the databases.
#[derive(Debug)]
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open (creating if needed), apply the PRAGMAs, and migrate.
    pub fn open(path: &Path, database: Database) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| StoreError::CreateDir {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        Self::from_connection(Connection::open(path)?, database)
    }

    /// An anonymous in-memory database. Tests get isolation by construction.
    pub fn open_in_memory(database: Database) -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?, database)
    }

    fn from_connection(conn: Connection, database: Database) -> Result<Self> {
        // WAL with synchronous=NORMAL is the whole reason a 1 Hz recorder costs
        // almost nothing: commits reach the kernel page cache without an fsync
        // per tick, and durability rides on WAL checkpoints and normal dirty
        // page writeback instead. A crash cannot corrupt the file; it can only
        // lose the last few seconds, which is an acceptable trade for a tool
        // whose alternative is wearing out the drive it is monitoring.
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA wal_autocheckpoint = 100;
             PRAGMA busy_timeout = 5000;",
        )?;

        migrate(&conn, database.migrations())?;
        Ok(Self { conn })
    }

    /// The most recently inserted row, or [`None`] on an empty database.
    ///
    /// Ordered by `id` rather than `ts` deliberately: `id` is the insertion
    /// order and cannot be perturbed by a clock step.
    pub fn latest(&self) -> Result<Option<Sample>> {
        let mut statement = self
            .conn
            .prepare("SELECT * FROM samples ORDER BY id DESC LIMIT 1")?;
        let mut rows = statement.query([])?;

        match rows.next()? {
            Some(row) => Ok(Some(sample_from_row(row)?)),
            None => Ok(None),
        }
    }

    /// Append a sample exactly as given.
    pub fn insert(&self, sample: &Sample) -> Result<()> {
        self.conn.execute(
            INSERT_SQL,
            named_params! {
                ":ts": sample.ts,
                ":charge_pct": sample.charge_pct,
                ":status": sample.status,
                ":power_state": sample.power_state.as_str(),
                ":energy_wh": sample.energy_wh,
                ":energy_full_wh": sample.energy_full_wh,
                ":energy_design_wh": sample.energy_design_wh,
                ":power_w": sample.power_w,
                ":voltage_v": sample.voltage_v,
                ":voltage_design_v": sample.voltage_design_v,
                ":cycle_count": sample.cycle_count,
                ":estimated_cycle_count": sample.estimated_cycle_count,
                ":battery_temp_c": sample.battery_temp_c,
                ":health_pct": sample.health_pct,
                ":is_charging": sample.is_charging,
                ":is_present": sample.is_present,
                ":time_to_empty_s": sample.time_to_empty_s,
                ":time_to_full_s": sample.time_to_full_s,
                ":cpu_temp_c": sample.cpu_temp_c,
                ":gpu_temp_c": sample.gpu_temp_c,
                ":nvme_temp_c": sample.nvme_temp_c,
                ":cpu_pct": sample.cpu_pct,
                ":mem_pct": sample.mem_pct,
                ":top_processes": sample.top_processes,
                ":cpu_freq_mhz": sample.cpu_freq_mhz,
                ":gpu_pct": sample.gpu_pct,
                ":gpu_power_w": sample.gpu_power_w,
                ":load1": sample.load1,
                ":boot_id": sample.boot_id,
                ":uptime_s": sample.uptime_s,
            },
        )?;
        Ok(())
    }

    /// Integrate the cycle count against this database's own last row, then append.
    ///
    /// `sample` is taken by mutable reference because the computed count has to
    /// be visible to the caller. The daemon's one-shot path relies on it: it
    /// writes the same sample to both databases, and the value stored in the
    /// flight recorder is the one integrated here.
    ///
    /// Each database integrates against its own history, so `battery.db`
    /// accumulates across 60-second gaps and `debug.db` across one-second gaps.
    /// Returns the previous row, if there was one.
    pub fn insert_integrating_cycles(&self, sample: &mut Sample) -> Result<Option<Sample>> {
        let previous = self.latest()?;
        sample.estimated_cycle_count = compute_estimated_cycles(sample, previous.as_ref());
        self.insert(sample)?;
        Ok(previous)
    }

    /// Drop rows older than `hours`, returning how many went.
    pub fn prune_older_than(&self, hours: i64) -> Result<usize> {
        // A negative window puts the cutoff in the future, and this statement
        // deletes everything older than it — which is every row. Schedule
        // validation already rejects that, but the guard belongs here too: this
        // is the boundary where the destructive statement is issued, and it is
        // public.
        if hours <= 0 {
            return Err(StoreError::InvalidRetention { hours });
        }

        let cutoff = Timestamp::now().checked_sub(SignedDuration::from_hours(hours))?;
        // Compared as text, which is only sound because every timestamp is
        // written fixed-width. See formats::iso8601_millis.
        let removed = self.conn.execute(
            "DELETE FROM samples WHERE ts < ?1",
            [iso8601_millis(cutoff)],
        )?;
        Ok(removed)
    }

    /// Flush the write-ahead log into the main database file.
    ///
    /// Called on shutdown so a stopped daemon leaves a self-contained file
    /// rather than a `-wal` sidecar the next reader has to replay.
    pub fn checkpoint(&self) -> Result<()> {
        self.conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(())
    }
}

/// Rebuild a [`Sample`] from a row, repairing what older schema versions omitted.
///
/// Columns are addressed by name, never by position, so a schema change that
/// reorders them cannot silently shift every value one column to the left.
///
/// Two repairs matter, and both are the common case rather than an edge case.
/// `power_state` did not exist before schema version 6 and is NULL in the
/// overwhelming majority of historical rows, so it is re-derived from the
/// `status` string that was always recorded alongside it. `is_charging` and
/// `is_present` are stored as integers and become real booleans here.
///
/// Nullable numerics that the type says are not optional collapse to zero, which
/// is what the TypeScript reader's `?? 0` produced and what the cycle integrator
/// already treats as "no usable history".
fn sample_from_row(row: &Row<'_>) -> rusqlite::Result<Sample> {
    let status: String = row.get::<_, Option<String>>("status")?.unwrap_or_default();

    let power_state = row
        .get::<_, Option<String>>("power_state")?
        .as_deref()
        .and_then(PowerState::parse_stored)
        .unwrap_or_else(|| PowerState::from_status(&status));

    Ok(Sample {
        ts: row.get::<_, Option<String>>("ts")?.unwrap_or_default(),
        charge_pct: row.get::<_, Option<f64>>("charge_pct")?.unwrap_or(0.0),
        status,
        power_state,
        energy_wh: row.get::<_, Option<f64>>("energy_wh")?.unwrap_or(0.0),
        energy_full_wh: row.get::<_, Option<f64>>("energy_full_wh")?.unwrap_or(0.0),
        energy_design_wh: row
            .get::<_, Option<f64>>("energy_design_wh")?
            .unwrap_or(0.0),
        power_w: row.get::<_, Option<f64>>("power_w")?.unwrap_or(0.0),
        voltage_v: row.get::<_, Option<f64>>("voltage_v")?.unwrap_or(0.0),
        voltage_design_v: row
            .get::<_, Option<f64>>("voltage_design_v")?
            .unwrap_or(0.0),
        cycle_count: row.get("cycle_count")?,
        estimated_cycle_count: row
            .get::<_, Option<f64>>("estimated_cycle_count")?
            .unwrap_or(0.0),
        battery_temp_c: row.get("battery_temp_c")?,
        health_pct: row.get::<_, Option<f64>>("health_pct")?.unwrap_or(0.0),
        is_charging: row.get::<_, Option<i64>>("is_charging")?.unwrap_or(0) != 0,
        is_present: row.get::<_, Option<i64>>("is_present")?.unwrap_or(0) != 0,
        time_to_empty_s: row.get("time_to_empty_s")?,
        time_to_full_s: row.get("time_to_full_s")?,
        cpu_temp_c: row.get("cpu_temp_c")?,
        gpu_temp_c: row.get("gpu_temp_c")?,
        nvme_temp_c: row.get("nvme_temp_c")?,
        cpu_pct: row.get("cpu_pct")?,
        mem_pct: row.get("mem_pct")?,
        top_processes: row.get("top_processes")?,
        cpu_freq_mhz: row.get("cpu_freq_mhz")?,
        gpu_pct: row.get("gpu_pct")?,
        gpu_power_w: row.get("gpu_power_w")?,
        load1: row.get("load1")?,
        boot_id: row.get("boot_id")?,
        uptime_s: row.get("uptime_s")?,
    })
}

#[cfg(test)]
mod tests;
