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
