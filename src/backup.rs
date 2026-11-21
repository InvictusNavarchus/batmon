//! Snapshots of both databases taken before a schema migration.
//!
//! Migrations rewrite the only copy of years of battery history. Steps are
//! atomic, so a failing step leaves nothing behind, but a step that succeeds and
//! is simply wrong would destroy data with no way back. Before opening either
//! database would migrate it, both are copied together into
//! `backups/pre-migration/<timestamp>-battery-v<N>-debug-v<M>/`.
//!
//! Both, not just the outdated one: they share invariants (each keeps its own
//! cycle ledger in `battery_cycles`), so restoring one from a different moment
//! than the other could leave inconsistent cycle counts.
//!
//! The caller must hold the daemon lock. That makes it the only writer, so the
//! two copies describe one moment, and the only process using the staging
//! directory.

use std::fs::{self, File};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags};

use crate::db::{Database, recorded_version};
use crate::paths::Paths;

/// Published snapshots kept. Taking a new one removes the oldest beyond this.
pub const KEEP: usize = 3;

/// Where a snapshot is assembled before it is published, beside the databases
/// so that publishing is a same-filesystem rename.
const STAGING: &str = ".pre-migration-staging";

/// The directory holding published pre-migration snapshots.
///
/// Batmon owns it entirely and prunes it. Manual backups belong elsewhere, such
/// as its parent `backups/`, which is never touched.
#[must_use]
pub fn snapshot_dir(db_dir: &Path) -> PathBuf {
    db_dir.join("backups").join("pre-migration")
}

/// One database as found on disk before migrating.
struct Source {
    database: Database,
    path: PathBuf,
    /// [`None`] when the file does not exist.
    version: Option<u32>,
}

impl Source {
    fn file_name(&self) -> &'static str {
        match self.database {
            Database::Historical => "battery.db",
            Database::Debug => "debug.db",
        }
    }

    fn pending(&self) -> bool {
        self.version
            .is_some_and(|version| version < self.database.latest_version())
    }

    fn label(&self) -> String {
        let stem = self.file_name().trim_end_matches(".db");
        match self.version {
            Some(version) => format!("{stem}-v{version}"),
            None => format!("{stem}-none"),
        }
    }
}

/// Snapshot both databases if opening either would migrate it.
///
/// Returns the snapshot that preserves the current state, or [`None`] when
/// nothing needs migrating. If a snapshot of exactly these schema versions is
/// already published, it is kept and returned instead of being replaced: after
/// a failed migration, the retry must not swap the original for a copy of
/// whatever the failed attempt left, and a restart loop must not fill the disk.
///
/// An error means no snapshot was published; the caller must not migrate.
pub fn before_migration(paths: &Paths) -> Result<Option<PathBuf>> {
    let mut sources = Vec::new();
    for (database, path) in [
        (Database::Historical, paths.db_path()),
        (Database::Debug, paths.debug_db_path()),
    ] {
        let version = recorded_version(&path)
            .with_context(|| format!("reading the schema version of {}", path.display()))?;
        sources.push(Source {
            database,
            path,
            version,
        });
    }

    if !sources.iter().any(Source::pending) {
        return Ok(None);
    }

    let label = sources
        .iter()
        .map(Source::label)
        .collect::<Vec<_>>()
        .join("-");
    let published = snapshot_dir(&paths.db_dir);
    if let Some(existing) = find_published(&published, &label)? {
        tracing::info!(
            snapshot = %existing.display(),
            "keeping the existing pre-migration snapshot"
        );
        return Ok(Some(existing));
    }

    let staging = paths.db_dir.join(STAGING);
    let snapshot = published.join(format!("{}-{label}", timestamp()));
    if let Err(error) = stage(&sources, &staging).and_then(|()| publish(&staging, &snapshot)) {
        // Best effort: the next attempt clears it anyway.
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }

    tracing::info!(snapshot = %snapshot.display(), "databases backed up before migrating");
    if let Err(error) = prune(&published, KEEP) {
        // The snapshot is published; failing to tidy older ones must not block
        // the migration it protects.
        tracing::warn!("could not prune old pre-migration snapshots: {error:#}");
    }
    Ok(Some(snapshot))
}

/// Copy and verify every existing database into a fresh staging directory.
fn stage(sources: &[Source], staging: &Path) -> Result<()> {
    // Under the lock no other process uses this directory, so anything in it
    // was left by an interrupted attempt.
    if staging.exists() {
        fs::remove_dir_all(staging)
            .with_context(|| format!("removing stale staging {}", staging.display()))?;
    }
    fs::create_dir_all(staging)
        .with_context(|| format!("creating staging {}", staging.display()))?;

    for source in sources {
        let Some(version) = source.version else {
            continue;
        };
        let copy = staging.join(source.file_name());
        snapshot(&source.path, &copy)
            .with_context(|| format!("copying {}", source.path.display()))?;
        verify(&copy, version)?;
        File::open(&copy)
            .and_then(|file| file.sync_all())
            .with_context(|| format!("flushing {}", copy.display()))?;
    }
    sync_dir(staging)
}

/// Write a consistent, self-contained copy of the database at `source`.
///
/// `VACUUM INTO` reads through SQLite, so the copy includes commits still in the
/// WAL, which copying the file would miss. It produces a single file with no
/// sidecars. Row ids are kept: `samples` has an explicit integer primary key,
/// and `battery_cycles` is keyed by its text `battery_id`.
fn snapshot(source: &Path, copy: &Path) -> Result<()> {
    let conn = Connection::open_with_flags(
        source,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let destination = copy
        .to_str()
        .with_context(|| format!("{} is not valid UTF-8", copy.display()))?;
    conn.execute("VACUUM INTO ?1", [destination])?;
    Ok(())
}

/// Check that a copy is intact and holds the schema version it was taken at.
fn verify(copy: &Path, version: u32) -> Result<()> {
    let conn = Connection::open_with_flags(
        copy,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    // A healthy database yields exactly one row, `ok`; anything else lists
    // problems, the first of which is enough to report.
    let integrity: String = conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    ensure!(
        integrity == "ok",
        "{} failed its integrity check: {integrity}",
        copy.display()
    );
    let copied: u32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    ensure!(
        copied == version,
        "{} holds schema version {copied}, expected {version}",
        copy.display()
    );
    Ok(())
}

/// Make the staged snapshot visible under its final name in one step.
fn publish(staging: &Path, snapshot: &Path) -> Result<()> {
    let published = snapshot.parent().context("snapshot has no parent")?;
    fs::create_dir_all(published).with_context(|| format!("creating {}", published.display()))?;
    fs::rename(staging, snapshot).with_context(|| format!("publishing {}", snapshot.display()))?;
    sync_dir(published)
}

/// A published snapshot of exactly these schema versions, if there is one.
fn find_published(published: &Path, label: &str) -> Result<Option<PathBuf>> {
    let suffix = format!("-{label}");
    Ok(snapshots(published)?.into_iter().find(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(&suffix))
    }))
}

/// Remove all but the newest `keep` published snapshots.
fn prune(published: &Path, keep: usize) -> Result<()> {
    let mut all = snapshots(published)?;
    // Names start with a UTC timestamp, so they sort chronologically.
    all.sort();
    let excess = all.len().saturating_sub(keep);
    for old in &all[..excess] {
        fs::remove_dir_all(old).with_context(|| format!("removing {}", old.display()))?;
        tracing::info!(snapshot = %old.display(), "removed old pre-migration snapshot");
    }
    Ok(())
}

/// Published snapshot directories, recognised by their timestamp prefix so that
/// anything else placed beside them is never pruned.
fn snapshots(published: &Path) -> Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(published) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error).with_context(|| format!("listing {}", published.display()));
        }
    };

    let mut found = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("listing {}", published.display()))?;
        let is_snapshot =
            entry.file_type()?.is_dir() && entry.file_name().to_str().is_some_and(is_snapshot_name);
        if is_snapshot {
            found.push(entry.path());
        }
    }
    Ok(found)
}

/// Whether `name` starts like `20261121T190400Z-`.
fn is_snapshot_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() > 17
        && bytes[..8].iter().all(u8::is_ascii_digit)
        && bytes[8] == b'T'
        && bytes[9..15].iter().all(u8::is_ascii_digit)
        && bytes[15] == b'Z'
        && bytes[16] == b'-'
}

fn timestamp() -> String {
    jiff::Timestamp::now()
        .strftime("%Y%m%dT%H%M%SZ")
        .to_string()
}

/// Persist a directory's entries, so a rename or new file survives a crash.
fn sync_dir(dir: &Path) -> Result<()> {
    File::open(dir)
        .and_then(|file| file.sync_all())
        .with_context(|| format!("flushing {}", dir.display()))
}

#[cfg(test)]
mod tests;
