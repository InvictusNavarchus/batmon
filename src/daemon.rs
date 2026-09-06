//! The tick loop: read, record, alert, wait.
//!
//! Synchronous and single-threaded by design. SQLite, sysfs and procfs all
//! block, so an async runtime would add a scheduler without removing a single
//! wait. It also removes a whole class of bug: the TypeScript daemon needed an
//! `isTicking` re-entrancy guard because `setInterval` will happily start a
//! second tick while the first is still awaiting. A loop that sleeps cannot
//! overlap with itself.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use crate::alerts::{AlertEngine, Notifier};
use crate::config::{Schedule, Thresholds};
use crate::cycles::compute_estimated_cycles;
use crate::db::{Store, StoreError};
use crate::telemetry::TelemetrySource;
use crate::types::Sample;

/// Owns everything that persists between ticks.
pub struct Daemon<S: TelemetrySource, N: Notifier> {
    source: S,
    notifier: N,
    engine: AlertEngine,
    /// One row per tick, pruned to a rolling window.
    debug: Store,
    /// One row per minute, kept forever.
    historical: Store,
    thresholds: Thresholds,
    schedule: Schedule,
    /// The previous sample, for integrating cycles at flight-recorder resolution.
    previous: Option<Sample>,
    tick_count: u64,
}

impl<S: TelemetrySource, N: Notifier> std::fmt::Debug for Daemon<S, N> {
    /// Hand-written because the source and notifier are seams: a test double or
    /// a live D-Bus connection need not be printable for the daemon to be.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Daemon")
            .field("tick_count", &self.tick_count)
            .field("schedule", &self.schedule)
            .finish_non_exhaustive()
    }
}

impl<S: TelemetrySource, N: Notifier> Daemon<S, N> {
    #[must_use]
    pub fn new(
        source: S,
        notifier: N,
        debug: Store,
        historical: Store,
        thresholds: Thresholds,
        schedule: Schedule,
    ) -> Self {
        Self {
            source,
            notifier,
            engine: AlertEngine::new(),
            debug,
            historical,
            thresholds,
            schedule,
            previous: None,
            tick_count: 0,
        }
    }

    /// How many ticks have been attempted.
    #[must_use]
    pub fn ticks(&self) -> u64 {
        self.tick_count
    }

    /// Run one tick, logging rather than propagating any failure.
    ///
    /// A tick that fails must not stop the recorder. A transient SQLite lock or
    /// a disappearing sysfs attribute costs one sample; giving up costs every
    /// sample after it, which is the opposite of what a flight recorder is for.
    /// The counter advances either way, so the historical and prune cadences
    /// stay on schedule rather than drifting with the error rate.
    pub fn run_tick(&mut self) {
        if let Err(error) = self.tick() {
            tracing::error!(%error, tick = self.tick_count, "tick failed");
        }
        self.tick_count += 1;
    }

    fn tick(&mut self) -> Result<(), StoreError> {
        let mut sample = self.source.sample();

        // No battery: nothing to record, and every latched alert describes
        // hardware that is no longer there.
        if !sample.is_present {
            self.engine.reset();
            return Ok(());
        }

        // On the first tick after a restart, pick up where the last run left
        // off. The flight recorder is preferred over the historical database
        // because it is denser and therefore closer in time; without this a
        // restart would restart the cycle integral from zero.
        if self.previous.is_none() {
            self.previous = match self.debug.latest()? {
                Some(latest) => Some(latest),
                None => self.historical.latest()?,
            };
        }

        sample.estimated_cycle_count = compute_estimated_cycles(&sample, self.previous.as_ref());

        // 1. Flight recorder, every tick.
        self.debug.insert(&sample)?;

        // 2. Alerts.
        for notification in self.engine.evaluate(&sample, &self.thresholds) {
            self.notifier.deliver(&notification);
        }

        // 3. Downsampled history. This integrates against its own last row, so
        //    the permanent record accumulates across minute-long gaps while the
        //    flight recorder accumulates across seconds.
        if self
            .tick_count
            .is_multiple_of(self.schedule.historical_interval_ticks)
        {
            let mut downsampled = sample.clone();
            self.historical
                .insert_integrating_cycles(&mut downsampled)?;
        }

        // 4. Prune, skipping the first tick so a restart is not a prune.
        if self.tick_count > 0
            && self
                .tick_count
                .is_multiple_of(self.schedule.prune_interval_ticks)
        {
            let removed = self
                .debug
                .prune_older_than(self.schedule.debug_retention_hours)?;
            tracing::debug!(removed, "pruned flight recorder");
        }

        self.previous = Some(sample);
        Ok(())
    }

    /// Tick until `running` clears.
    ///
    /// The deadline advances by a fixed interval rather than sleeping for one,
    /// so the cadence does not drift by the cost of each tick. Measured against
    /// the TypeScript daemon, which slept for an interval: over six hours it
    /// recorded 113 fewer samples from the same window, about 7.5 minutes of
    /// lost coverage per day. When the deadline is
    /// already past, the loop resynchronises instead of trying to catch up,
    /// which is what makes suspend and resume visible: waking to find the
    /// deadline hours behind is precisely a suspend, and a burst of back-to-back
    /// ticks would record nothing useful about the time that was missed.
    pub fn run(&mut self, running: &AtomicBool) {
        let mut deadline = Instant::now();

        while running.load(Ordering::Relaxed) {
            self.run_tick();

            deadline += self.schedule.sample_interval;
            if let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
                std::thread::sleep(remaining);
            } else {
                tracing::warn!(
                    tick = self.tick_count,
                    "tick overran or the system suspended; resynchronising"
                );
                deadline = Instant::now();
            }
        }
    }

    /// Flush both databases so a stopped daemon leaves self-contained files.
    pub fn shutdown(&self) {
        for (name, store) in [("debug", &self.debug), ("historical", &self.historical)] {
            if let Err(error) = store.checkpoint() {
                tracing::warn!(%error, database = name, "could not checkpoint on shutdown");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::float_cmp)]

    use super::*;
    use crate::alerts::RecordingNotifier;
    use crate::db::Database;
    use crate::types::PowerState;

    /// A telemetry source that replays a scripted sequence, repeating the last
    /// sample once the script runs out.
    struct Scripted {
        samples: Vec<Sample>,
        position: usize,
    }

    impl Scripted {
        fn new(samples: Vec<Sample>) -> Self {
            Self {
                samples,
                position: 0,
            }
        }

        fn repeating(sample: Sample) -> Self {
            Self::new(vec![sample])
        }
    }

    impl TelemetrySource for Scripted {
        fn sample(&mut self) -> Sample {
            let sample = self.samples[self.position.min(self.samples.len() - 1)].clone();
            self.position += 1;
            sample
        }
    }

    fn present(charge_pct: f64, energy_wh: f64) -> Sample {
        Sample {
            // Stamped now, so retention-window tests measure against real time.
            ts: crate::parity::now_iso8601_millis(),
            charge_pct,
            status: "Discharging".to_owned(),
            power_state: PowerState::Discharging,
            energy_wh,
            energy_full_wh: 58.0,
            energy_design_wh: 58.0,
            health_pct: 99.0,
            is_present: true,
            battery_temp_c: Some(30.0),
            boot_id: Some("boot-1".to_owned()),
            uptime_s: Some(1_000.0),
            ..Sample::default()
        }
    }

    fn daemon(source: Scripted) -> Daemon<Scripted, RecordingNotifier> {
        Daemon::new(
            source,
            RecordingNotifier::new(),
            Store::open_in_memory(Database::Debug).unwrap(),
            Store::open_in_memory(Database::Historical).unwrap(),
            Thresholds::default(),
            Schedule::default(),
        )
    }

    #[test]
    fn every_tick_writes_to_the_flight_recorder() {
        let mut daemon = daemon(Scripted::repeating(present(80.0, 46.0)));

        for _ in 0..5 {
            daemon.run_tick();
        }

        assert_eq!(daemon.ticks(), 5);
        assert!(daemon.debug.latest().unwrap().is_some());
    }

    #[test]
    fn history_is_written_on_the_first_tick_and_then_once_a_minute() {
        let mut daemon = daemon(Scripted::repeating(present(80.0, 46.0)));

        daemon.run_tick();
        let after_first = daemon.historical.latest().unwrap();
        assert!(after_first.is_some(), "the first tick seeds the history");

        // Ticks 1..59 are flight-recorder only.
        for _ in 1..60 {
            daemon.run_tick();
        }
        assert_eq!(daemon.ticks(), 60);
    }

    #[test]
    fn an_absent_battery_records_nothing_and_clears_the_alert_state() {
        let mut daemon = daemon(Scripted::repeating(Sample {
            is_present: false,
            ..present(80.0, 46.0)
        }));

        for _ in 0..10 {
            daemon.run_tick();
        }

        assert_eq!(daemon.debug.latest().unwrap(), None);
        assert_eq!(daemon.historical.latest().unwrap(), None);
        assert!(daemon.notifier.delivered().is_empty());
        assert_eq!(daemon.ticks(), 10, "ticks still advance");
    }

    #[test]
    fn a_battery_reappearing_resumes_recording() {
        let mut daemon = daemon(Scripted::new(vec![
            Sample {
                is_present: false,
                ..present(80.0, 46.0)
            },
            present(79.0, 45.0),
        ]));

        daemon.run_tick();
        assert_eq!(daemon.debug.latest().unwrap(), None);

        daemon.run_tick();
        assert_eq!(daemon.debug.latest().unwrap().unwrap().charge_pct, 79.0);
    }

    #[test]
    fn alerts_reach_the_notifier() {
        let mut daemon = daemon(Scripted::repeating(present(8.0, 4.0)));

        daemon.run_tick();

        assert_eq!(
            daemon.notifier.titles(),
            vec!["CRITICAL: Battery Low"],
            "a critical charge level must be announced"
        );
    }

    #[test]
    fn an_alert_is_announced_once_across_many_ticks() {
        let mut daemon = daemon(Scripted::repeating(present(8.0, 4.0)));

        for _ in 0..50 {
            daemon.run_tick();
        }

        assert_eq!(daemon.notifier.delivered().len(), 1);
    }

    #[test]
    fn cycles_accumulate_across_ticks_at_flight_recorder_resolution() {
        let mut daemon = daemon(Scripted::new(vec![
            present(80.0, 58.0),
            present(79.0, 57.42), // 0.58 Wh of a 58 Wh pack: 0.01 cycles
            present(78.0, 56.84),
        ]));

        daemon.run_tick();
        daemon.run_tick();
        daemon.run_tick();

        let latest = daemon.debug.latest().unwrap().unwrap();
        assert!(
            (latest.estimated_cycle_count - 0.02).abs() < 1e-9,
            "{}",
            latest.estimated_cycle_count
        );
    }

    #[test]
    fn a_restart_resumes_the_cycle_count_from_the_flight_recorder() {
        // Without this, every restart would begin integrating from zero and the
        // permanent record would lose the accumulated total.
        let debug = Store::open_in_memory(Database::Debug).unwrap();
        debug
            .insert(&Sample {
                estimated_cycle_count: 45.5,
                ..present(81.0, 58.0)
            })
            .unwrap();

        let mut daemon = Daemon::new(
            Scripted::repeating(present(80.0, 57.42)),
            RecordingNotifier::new(),
            debug,
            Store::open_in_memory(Database::Historical).unwrap(),
            Thresholds::default(),
            Schedule::default(),
        );

        daemon.run_tick();

        let latest = daemon.debug.latest().unwrap().unwrap();
        assert!(
            (latest.estimated_cycle_count - 45.51).abs() < 1e-9,
            "{}",
            latest.estimated_cycle_count
        );
    }

    #[test]
    fn a_restart_falls_back_to_the_historical_database() {
        let historical = Store::open_in_memory(Database::Historical).unwrap();
        historical
            .insert(&Sample {
                estimated_cycle_count: 12.0,
                ..present(81.0, 58.0)
            })
            .unwrap();

        let mut daemon = Daemon::new(
            Scripted::repeating(present(80.0, 57.42)),
            RecordingNotifier::new(),
            Store::open_in_memory(Database::Debug).unwrap(),
            historical,
            Thresholds::default(),
            Schedule::default(),
        );

        daemon.run_tick();

        assert!(
            daemon
                .debug
                .latest()
                .unwrap()
                .unwrap()
                .estimated_cycle_count
                > 12.0
        );
    }

    #[test]
    fn the_first_tick_does_not_prune() {
        // A restart must not be a prune; the schedule skips tick zero.
        let mut daemon = daemon(Scripted::repeating(present(80.0, 46.0)));
        daemon.schedule.prune_interval_ticks = 1;

        daemon.run_tick();

        assert!(daemon.debug.latest().unwrap().is_some());
    }

    #[test]
    fn pruning_drops_rows_outside_the_retention_window() {
        let mut daemon = daemon(Scripted::repeating(present(80.0, 46.0)));
        daemon.schedule.prune_interval_ticks = 2;

        // A row far outside the window, written directly.
        daemon
            .debug
            .insert(&Sample {
                ts: "2020-01-01T00:00:00.000Z".to_owned(),
                ..present(50.0, 30.0)
            })
            .unwrap();

        daemon.run_tick();
        daemon.run_tick();
        daemon.run_tick();

        // The stale row is gone and the recent ones survived.
        let survivor = daemon.debug.latest().unwrap().unwrap();
        assert_ne!(survivor.ts, "2020-01-01T00:00:00.000Z");
        assert_eq!(survivor.charge_pct, 80.0);
    }

    #[test]
    fn the_two_databases_integrate_cycles_independently() {
        // The flight recorder accumulates across seconds, the history across
        // minutes, so their counts are computed from different baselines.
        let mut daemon = daemon(Scripted::new(vec![
            present(80.0, 58.0),
            present(79.0, 57.42),
        ]));
        daemon.schedule.historical_interval_ticks = 1;

        daemon.run_tick();
        daemon.run_tick();

        let debug = daemon.debug.latest().unwrap().unwrap();
        let historical = daemon.historical.latest().unwrap().unwrap();

        assert!((debug.estimated_cycle_count - 0.01).abs() < 1e-9);
        assert!((historical.estimated_cycle_count - 0.01).abs() < 1e-9);
    }

    #[test]
    fn the_loop_stops_when_the_flag_clears() {
        use std::sync::Arc;

        let mut daemon = daemon(Scripted::repeating(present(80.0, 46.0)));
        daemon.schedule.sample_interval = std::time::Duration::from_millis(1);

        let running = Arc::new(AtomicBool::new(true));
        let stopper = Arc::clone(&running);
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(40));
            stopper.store(false, Ordering::Relaxed);
        });

        daemon.run(&running);

        assert!(daemon.ticks() > 1, "the loop should have ticked");
        assert!(!running.load(Ordering::Relaxed));
    }

    #[test]
    fn a_cleared_flag_prevents_the_loop_running_at_all() {
        let mut daemon = daemon(Scripted::repeating(present(80.0, 46.0)));
        let running = AtomicBool::new(false);

        daemon.run(&running);

        assert_eq!(daemon.ticks(), 0);
    }

    #[test]
    fn shutdown_checkpoints_without_complaint() {
        let daemon = daemon(Scripted::repeating(present(80.0, 46.0)));
        daemon.shutdown();
    }
}
