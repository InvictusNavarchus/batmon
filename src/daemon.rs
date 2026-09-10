//! The tick loop: read, record, alert, wait.
//!
//! Synchronous and single-threaded by design. SQLite, sysfs and procfs all
//! block, so an async runtime would add a scheduler without removing a single
//! wait. It also removes a whole class of bug: a timer-driven tick needs a
//! re-entrancy guard, because a timer will happily start a second tick while
//! the first is still running. A loop that sleeps cannot overlap with itself.

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
    /// One row per tick, pruned to the newest window's worth of rows.
    debug: Store,
    /// One row per minute, kept forever.
    historical: Store,
    thresholds: Thresholds,
    schedule: Schedule,
    /// The previous sample, for integrating cycles at flight-recorder resolution.
    previous: Option<Sample>,
    /// Set while the battery is away, so the sample that follows carries the
    /// cycle count forward rather than integrating across the absence.
    battery_was_absent: bool,
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
            battery_was_absent: false,
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
        // Safe ahead of the early returns: retention counts rows, so a tick that
        // records nothing pushes nothing out, and a battery that stays
        // unreadable -- or absent -- leaves what was recorded before it intact.
        // That run-up is the evidence; erasing it by age once the outage
        // outlasted the window was the opposite of what a recorder is for.
        self.prune_if_due()?;

        // Unreadable, as opposed to absent: the hardware is still there, so the
        // latched alerts still describe it. The tick is a no-op -- no row, no
        // alert evaluation, and deliberately no `engine.reset()`, which would
        // re-announce a low battery the moment the read recovered.
        let Some(mut sample) = self.source.sample() else {
            return Ok(());
        };

        // No battery: nothing to record, and every latched alert describes
        // hardware that is no longer there.
        if !sample.is_present {
            self.engine.reset();
            self.battery_was_absent = true;
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

        // A pack that went away and came back may not be the same pack, and
        // whatever energy difference spans the absence did not pass through a
        // load. That is the reboot situation exactly, and gets the same
        // treatment: carry the count forward, integrate nothing.
        //
        // Clearing `previous` instead would not work — the next tick would
        // simply re-adopt the same pre-removal row from the database.
        sample.estimated_cycle_count = if std::mem::take(&mut self.battery_was_absent) {
            self.previous
                .as_ref()
                .map_or(0.0, |previous| previous.estimated_cycle_count)
        } else {
            compute_estimated_cycles(&sample, self.previous.as_ref())
        };

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

        self.previous = Some(sample);
        Ok(())
    }

    /// Prune the flight recorder on its interval.
    ///
    /// Tick zero included: a prune only trims to the newest window's worth of
    /// rows, so one on restart cannot touch the run before it.
    fn prune_if_due(&mut self) -> Result<(), StoreError> {
        if self
            .tick_count
            .is_multiple_of(self.schedule.prune_interval_ticks)
        {
            let removed = self
                .debug
                .retain_newest(self.schedule.debug_retention_rows())?;
            tracing::debug!(removed, "pruned flight recorder");
        }
        Ok(())
    }

    /// Tick until `running` clears.
    ///
    /// The deadline advances by a fixed interval rather than sleeping for one,
    /// so the cadence does not drift by the cost of each tick. The difference is
    /// not theoretical: a loop that sleeps for the interval instead was measured
    /// over six hours recording 113 fewer samples from the same window, about
    /// 7.5 minutes of lost coverage per day. When the deadline is
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
mod tests;
