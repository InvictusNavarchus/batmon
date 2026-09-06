//! batmon — battery health monitor and hardware flight recorder.

use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;

use batmon::alerts::{AlertEngine, Notifier};
use batmon::config::{Schedule, Thresholds};
use batmon::daemon::Daemon;
use batmon::db::{Database, Store};
use batmon::dbus::{DesktopNotifier, UPower};
use batmon::paths::Paths;
use batmon::telemetry::{Sampler, TelemetrySource};

/// Pause between the two samples a one-shot run takes.
///
/// Utilisation is a rate, so it needs two readings of a monotonic counter to
/// exist at all. Without this pause a one-shot sample would record null CPU and
/// an empty process ranking — which is exactly what the installer's verification
/// step is meant to prove works.
const ONESHOT_WARMUP: Duration = Duration::from_millis(500);

#[derive(Debug, Parser)]
#[command(
    name = "batmon",
    about = "Battery health monitor and hardware flight recorder",
    version
)]
struct Cli {
    /// Record a single sample and exit, instead of running as a daemon.
    #[arg(long)]
    oneshot: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_logging();

    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!("{error:#}");
            ExitCode::FAILURE
        }
    }
}

/// Send structured logs to stderr, which systemd routes to the journal.
///
/// Timestamps are omitted because the journal stamps every entry itself, and two
/// timestamps per line is noise. `BATMON_LOG` takes the usual filter syntax.
fn init_logging() {
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_env("BATMON_LOG")
        .unwrap_or_else(|_| EnvFilter::new("batmon=info,warn"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .without_time()
        .with_writer(std::io::stderr)
        .init();
}

fn run(cli: &Cli) -> Result<()> {
    let paths = Paths::from_env();
    let thresholds = Thresholds::default();
    let schedule = Schedule::default();

    thresholds
        .validate()
        .context("alert thresholds are inconsistent")?;
    schedule
        .validate()
        .context("sampling schedule is inconsistent")?;

    if cli.oneshot {
        return oneshot(&paths);
    }
    daemon(&paths, thresholds, schedule)
}

/// Open both databases, applying migrations.
fn stores(paths: &Paths) -> Result<(Store, Store)> {
    let debug = Store::open(&paths.debug_db_path(), Database::Debug)
        .with_context(|| format!("opening {}", paths.debug_db_path().display()))?;
    let historical = Store::open(&paths.db_path(), Database::Historical)
        .with_context(|| format!("opening {}", paths.db_path().display()))?;
    Ok((debug, historical))
}

fn sampler(paths: &Paths) -> Sampler {
    Sampler::new(paths.clone(), Box::new(UPower::connect(&paths.battery)))
}

/// Run until a termination signal arrives.
fn daemon(paths: &Paths, thresholds: Thresholds, schedule: Schedule) -> Result<()> {
    let (debug, historical) = stores(paths)?;

    let mut daemon = Daemon::new(
        sampler(paths),
        DesktopNotifier::connect(),
        debug,
        historical,
        thresholds,
        schedule,
    );

    let running = watch_for_termination()?;

    tracing::info!(
        battery = %paths.battery.display(),
        databases = %paths.db_dir.display(),
        "batmon started"
    );

    daemon.run(&running);
    daemon.shutdown();

    tracing::info!(ticks = daemon.ticks(), "batmon stopped");
    Ok(())
}

/// Clear the returned flag when SIGINT or SIGTERM arrives.
///
/// A dedicated thread rather than a signal handler writing the flag directly,
/// so the signal that caused the shutdown can be logged. Shutdown latency is
/// bounded by one tick, because a sleeping thread finishes its sleep first.
fn watch_for_termination() -> Result<Arc<AtomicBool>> {
    use signal_hook::consts::{SIGINT, SIGTERM};
    use signal_hook::iterator::Signals;

    let running = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&running);

    let mut signals = Signals::new([SIGINT, SIGTERM]).context("installing signal handlers")?;
    std::thread::spawn(move || {
        if let Some(signal) = signals.forever().next() {
            tracing::info!(signal, "termination requested");
            flag.store(false, Ordering::Relaxed);
        }
    });

    Ok(running)
}

/// Record one sample and exit.
///
/// Used by the installer to prove the whole path works on this machine, and
/// useful by hand for a single diagnostic reading.
fn oneshot(paths: &Paths) -> Result<()> {
    let mut sampler = sampler(paths);

    // The first sample only establishes the baselines that rates are measured
    // against; its own utilisation figures are meaningless.
    if !sampler.sample().is_present {
        tracing::warn!("no battery present; nothing recorded");
        return Ok(());
    }
    std::thread::sleep(ONESHOT_WARMUP);

    let mut sample = sampler.sample();
    if !sample.is_present {
        tracing::warn!("no battery present; nothing recorded");
        return Ok(());
    }

    let (debug, historical) = stores(paths)?;

    // Each database integrates against its own last row, exactly as the daemon
    // does. Sharing one integration between them — which is what the TypeScript
    // did, by mutating a single object — makes the flight recorder adopt the
    // history's coarser baseline. Where the daemon has been running since the
    // last downsampled write, that baseline is behind, and the debug count goes
    // *backwards*: a cycle total that decreases, which the integrator's own
    // property tests forbid.
    let mut downsampled = sample.clone();
    historical.insert_integrating_cycles(&mut downsampled)?;
    debug.insert_integrating_cycles(&mut sample)?;

    let mut engine = AlertEngine::new();
    let mut notifier = DesktopNotifier::connect();
    for notification in engine.evaluate(&sample, &Thresholds::default()) {
        notifier.deliver(&notification);
    }

    historical.checkpoint()?;
    debug.checkpoint()?;

    tracing::info!(
        charge_pct = sample.charge_pct,
        health_pct = sample.health_pct,
        cycles = sample.estimated_cycle_count,
        "sample recorded"
    );
    Ok(())
}
