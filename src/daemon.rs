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
    /// One row per tick that reads a battery, pruned to the newest window's
    /// worth of rows.
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
    last_known_battery_id: Option<String>,
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
            last_known_battery_id: None,
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
        // Retention counts all recorded system samples, including partial ones.
        self.prune_if_due()?;

        let Some(mut sample) = self.source.sample() else {
            if let Some(previous) = &mut self.previous {
                previous.energy_wh = None;
            }
            return Ok(());
        };

        // Confirmed absence clears battery alerts but preserves system recording.
        if !sample.is_present {
            self.engine.reset();
            self.battery_was_absent = true;
        }

        // On the first tick after a restart, pick up where the last run left
        // off. The flight recorder is preferred over the historical database
        // because it is denser and therefore closer in time; without this a
        // restart would restart the cycle integral from zero.
        if self.previous.is_none() {
            self.previous = match self.debug.latest()? {
                Some(latest) => Some(latest),
                None => match self.historical.latest() {
                    Ok(previous) => previous,
                    Err(error) => {
                        tracing::warn!(%error, "could not restore historical baseline; continuing flight recording");
                        None
                    }
                },
            };
        }

        // A pack that went away and came back may not be the same pack, and
        // whatever energy difference spans the absence did not pass through a
        // load. That is the reboot situation exactly, and gets the same
        // treatment: carry the count forward, integrate nothing.
        //
        // Clearing `previous` instead would not work — the next tick would
        // simply re-adopt the same pre-removal row from the database.
        if self.last_known_battery_id.is_none() {
            self.last_known_battery_id = self.previous.as_ref().and_then(|p| p.battery_id.clone());
        }
        if let Some(id) = &sample.battery_id {
            if self
                .last_known_battery_id
                .as_ref()
                .is_some_and(|last| last != id)
            {
                self.engine.reset();
            }
            self.last_known_battery_id = Some(id.clone());
        }
        if self
            .previous
            .as_ref()
            .is_some_and(|p| p.battery_id != sample.battery_id)
            || (self.previous.is_none() && sample.battery_id.is_some())
        {
            let carried = match &sample.battery_id {
                Some(id) => self
                    .debug
                    .cycles_for(id)?
                    .max(self.historical_cycle_total(id)),
                None => 0.0,
            };
            self.previous = Some(Sample {
                energy_wh: None,
                estimated_cycle_count: Some(carried),
                ..sample.clone()
            });
        }

        sample.estimated_cycle_count = Some(
            if !sample.is_present || std::mem::take(&mut self.battery_was_absent) {
                self.previous
                    .as_ref()
                    .and_then(|previous| previous.estimated_cycle_count)
                    .unwrap_or(0.0)
            } else {
                compute_estimated_cycles(&sample, self.previous.as_ref())
            },
        );

        // Never attach an unattributed row to a cached pack. Legacy continuous
        // unidentified histories remain usable, but a known-to-unknown transition
        // cannot claim a cycle total until attribution returns.
        if sample.battery_id.is_none()
            && (self.last_known_battery_id.is_some()
                || self
                    .previous
                    .as_ref()
                    .is_some_and(|p| p.estimated_cycle_count.is_none()))
        {
            sample.estimated_cycle_count = None;
        }

        // Reconcile the completed integral, not its baseline: adding the same
        // interval again after a concurrent oneshot would double-count wear.
        if let (Some(id), Some(total)) = (&sample.battery_id, sample.estimated_cycle_count) {
            sample.estimated_cycle_count = Some(total.max(self.historical_cycle_total(id)));
        }
        // The debug transaction reconciles its own ledger atomically with insertion.
        sample.estimated_cycle_count = self.debug.insert(&sample)?;

        // 2. Alerts.
        if sample.is_present {
            for notification in self.engine.evaluate(&sample, &self.thresholds) {
                self.notifier.deliver(&notification);
            }
        }

        // Advance the flight baseline even when the independent historical
        // write fails, so a recorded partial sample still breaks integration.
        let history_result = if self
            .tick_count
            .is_multiple_of(self.schedule.historical_interval_ticks)
        {
            self.historical.insert(&sample).map(|_| ())
        } else {
            Ok(())
        };
        self.previous = Some(sample);
        history_result
    }

    /// History is a supplementary recovery source. Its errors are visible but
    /// must not stop a healthy flight recorder from persisting current evidence.
    fn historical_cycle_total(&self, id: &str) -> f64 {
        match self.historical.cycles_for(id) {
            Ok(total) => total,
            Err(error) => {
                tracing::warn!(%error, battery_id = id, "could not reconcile historical cycle ledger; using flight recorder total");
                0.0 // max() leaves the authoritative flight total unchanged.
            }
        }
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
