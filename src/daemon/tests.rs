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

/// A source whose battery is installed throughout but intermittently
/// unreadable, so some ticks yield nothing at all.
struct WithGaps {
    ticks: Vec<Option<Sample>>,
    position: usize,
}

impl WithGaps {
    fn new(ticks: Vec<Option<Sample>>) -> Self {
        Self { ticks, position: 0 }
    }
}

impl TelemetrySource for WithGaps {
    fn sample(&mut self) -> Option<Sample> {
        let sample = self.ticks[self.position.min(self.ticks.len() - 1)].clone();
        self.position += 1;
        sample
    }
}

impl TelemetrySource for Scripted {
    fn sample(&mut self) -> Option<Sample> {
        let sample = self.samples[self.position.min(self.samples.len() - 1)].clone();
        self.position += 1;
        Some(sample)
    }
}

fn present(charge_pct: f64, energy_wh: f64) -> Sample {
    Sample {
        ts: crate::formats::now_iso8601_millis(),
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

fn daemon<S: TelemetrySource>(source: S) -> Daemon<S, RecordingNotifier> {
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
fn a_battery_that_comes_back_does_not_have_the_gap_counted_as_discharge() {
    // A pack removed at 58 Wh and replaced by one at 20 Wh would otherwise
    // integrate the 38 Wh difference as if it had gone through a load.
    let mut daemon = daemon(Scripted::new(vec![
        present(100.0, 58.0),
        Sample {
            is_present: false,
            ..present(100.0, 58.0)
        },
        present(34.0, 20.0),
    ]));

    daemon.run_tick();
    daemon.run_tick();
    daemon.run_tick();

    let latest = daemon.debug.latest().unwrap().unwrap();
    assert_eq!(
        latest.estimated_cycle_count, 0.0,
        "the absence was integrated as discharge"
    );
}

#[test]
fn a_returning_battery_carries_the_accumulated_count_forward() {
    // Carrying forward, not resetting: the wear already recorded is real.
    let debug = Store::open_in_memory(Database::Debug).unwrap();
    debug
        .insert(&Sample {
            estimated_cycle_count: 12.5,
            ..present(100.0, 58.0)
        })
        .unwrap();

    let mut daemon = Daemon::new(
        Scripted::new(vec![
            Sample {
                is_present: false,
                ..present(100.0, 58.0)
            },
            present(34.0, 20.0),
            present(33.0, 19.42),
        ]),
        RecordingNotifier::new(),
        debug,
        Store::open_in_memory(Database::Historical).unwrap(),
        Thresholds::default(),
        Schedule::default(),
    );

    daemon.run_tick();
    daemon.run_tick();
    let after_return = daemon.debug.latest().unwrap().unwrap();
    assert_eq!(
        after_return.estimated_cycle_count, 12.5,
        "count was not carried"
    );

    // And normal integration resumes on the tick after that.
    daemon.run_tick();
    let resumed = daemon.debug.latest().unwrap().unwrap();
    assert!(
        resumed.estimated_cycle_count > 12.5,
        "integration did not resume: {}",
        resumed.estimated_cycle_count
    );
}

#[test]
fn an_unreadable_tick_records_nothing_and_keeps_the_latch() {
    // The counterpart to the absent-battery test above. There the hardware is
    // gone and clearing the latches is right; here it is still installed and
    // merely unreadable, so clearing them would re-announce a low battery the
    // moment the read recovered.
    let low = present(8.0, 4.0);
    let mut daemon = daemon(WithGaps::new(vec![
        None,
        Some(low.clone()),
        None,
        Some(low),
    ]));

    daemon.run_tick();
    assert!(
        daemon.debug.latest().unwrap().is_none(),
        "an unreadable tick must not store a fabricated flight-recorder row"
    );
    // The daemon writes both stores, and history is seeded on the first tick,
    // so checking only the flight recorder would miss a leak into the
    // permanent record.
    assert!(
        daemon.historical.latest().unwrap().is_none(),
        "an unreadable tick must not store a fabricated history row"
    );

    daemon.run_tick(); // the alert fires here
    daemon.run_tick(); // unreadable again
    daemon.run_tick(); // and the battery comes back

    assert_eq!(
        daemon.notifier.delivered().len(),
        1,
        "the gap must not clear the latch, or recovery re-announces the alert"
    );
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

/// A row from a run that ended long ago, as a crash followed by a long
/// power-off leaves it.
fn before_the_power_off() -> Sample {
    Sample {
        ts: "2020-01-01T00:00:00.000Z".to_owned(),
        ..present(50.0, 30.0)
    }
}

#[test]
fn a_long_power_off_does_not_age_out_the_previous_run() {
    // Pruning by wall-clock age erased this row five minutes into the next
    // boot whenever the machine had been off longer than the window -- the
    // run-up to a crash, gone before anyone could read it.
    let mut daemon = daemon(Scripted::repeating(present(80.0, 46.0)));
    daemon.schedule.prune_interval_ticks = 1;
    daemon.debug.insert(&before_the_power_off()).unwrap();

    for _ in 0..3 {
        daemon.run_tick();
    }

    assert_eq!(
        daemon.debug.row_count().unwrap(),
        4,
        "the previous run is still the newest recording, however old"
    );
}

#[test]
fn an_unreadable_stretch_does_not_erase_what_led_up_to_it() {
    // Reverses the reasoning of a8a3626, which pruned through the stretch so
    // the recorder would honour a wall-clock window: after six unreadable
    // hours, that deleted the lead-up to the very fault being investigated.
    let mut daemon = daemon(WithGaps::new(vec![None]));
    daemon.schedule.prune_interval_ticks = 1;
    daemon.debug.insert(&before_the_power_off()).unwrap();

    for _ in 0..3 {
        daemon.run_tick();
    }

    assert_eq!(daemon.debug.latest().unwrap(), Some(before_the_power_off()));
}

#[test]
fn pruning_keeps_the_newest_window_of_samples() {
    let mut daemon = daemon(Scripted::repeating(present(80.0, 46.0)));
    daemon.schedule.prune_interval_ticks = 1;
    // One hour at twenty-minute ticks: a window of three rows.
    daemon.schedule.debug_retention_hours = 1;
    daemon.schedule.sample_interval = std::time::Duration::from_secs(20 * 60);
    daemon.debug.insert(&before_the_power_off()).unwrap();

    for _ in 0..4 {
        daemon.run_tick();
    }

    // Five rows written. The prune runs at the top of the tick, so what remains
    // is the window plus the row the last tick wrote after it; the oldest --
    // the pre-power-off row -- is the one that went.
    assert_eq!(daemon.debug.row_count().unwrap(), 3 + 1);
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
