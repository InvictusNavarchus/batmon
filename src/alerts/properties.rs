//! Property tests for the alerting invariants.
//!
//! The example-based tests describe scenarios someone thought of. These describe
//! the rules the engine must obey for *every* sequence of samples, which is the
//! only way to get real confidence in a hysteresis engine — its failure mode is
//! not a wrong answer on a known input, it is a notification storm after six
//! hours of readings nobody wrote a test for.

use proptest::prelude::*;

use crate::alerts::{AlertEngine, AlertFamily};
use crate::config::Thresholds;
use crate::types::{PowerState, Sample};

fn arb_power_state() -> impl Strategy<Value = PowerState> {
    prop_oneof![
        Just(PowerState::Charging),
        Just(PowerState::Discharging),
        Just(PowerState::AcIdle),
        Just(PowerState::Unknown),
    ]
}

/// A sample with plausible hardware readings across the whole interesting range.
fn arb_sample() -> impl Strategy<Value = Sample> {
    (
        0.0f64..=100.0,
        arb_power_state(),
        prop::option::of(-20.0f64..90.0),
        0.0f64..=100.0,
        0.0f64..20.0,
        0.0f64..20.0,
        prop::option::of(0.0f64..110.0),
        prop::option::of(0.0f64..100.0),
        0.0f64..80.0,
    )
        .prop_map(
            |(
                charge_pct,
                power_state,
                battery_temp_c,
                health_pct,
                voltage_v,
                voltage_design_v,
                cpu_temp_c,
                cpu_pct,
                power_w,
            )| Sample {
                charge_pct,
                power_state,
                // Kept consistent with the rail, as the telemetry reader
                // guarantees; an inconsistent pair is not a reachable state.
                is_charging: power_state == PowerState::Charging,
                battery_temp_c,
                health_pct,
                voltage_v,
                voltage_design_v,
                cpu_temp_c,
                cpu_pct,
                power_w,
                is_present: true,
                ..Sample::default()
            },
        )
}

/// A sequence whose consecutive samples resemble one another, as real telemetry
/// does — temperatures drift by fractions of a degree per second, they do not
/// teleport.
///
/// This exists because independent draws are not merely unrealistic, they are
/// *blind* to half the engine. A debounced alert needs three consecutive
/// qualifying samples, and independently sampled sequences produce such a run
/// so rarely that the heat-soak and anomaly detectors went essentially
/// unexercised: two thousand independent sequences yielded one heat-soak alert
/// and no anomalies at all.
fn arb_walk(max_len: usize) -> impl Strategy<Value = Vec<Sample>> {
    let step = (
        -4.0f64..4.0,   // charge
        -3.0f64..3.0,   // battery temperature
        -8.0f64..8.0,   // cpu temperature
        -25.0f64..25.0, // cpu utilisation
        -8.0f64..8.0,   // power draw
        // The rail changes rarely; a charger is plugged in seconds apart, not
        // every tick.
        prop::option::weighted(0.03, arb_power_state()),
    );

    (arb_sample(), prop::collection::vec(step, 0..max_len)).prop_map(|(base, steps)| {
        let mut current = base;
        let mut walk = vec![current.clone()];

        for (charge, battery, cpu_temp, cpu_pct, power, rail) in steps {
            let mut next = current.clone();
            next.charge_pct = (next.charge_pct + charge).clamp(0.0, 100.0);
            next.battery_temp_c = next
                .battery_temp_c
                .map(|t| (t + battery).clamp(-20.0, 90.0));
            next.cpu_temp_c = next.cpu_temp_c.map(|t| (t + cpu_temp).clamp(0.0, 110.0));
            next.cpu_pct = next.cpu_pct.map(|p| (p + cpu_pct).clamp(0.0, 100.0));
            next.power_w = (next.power_w + power).clamp(0.0, 80.0);
            if let Some(state) = rail {
                next.power_state = state;
                next.is_charging = state == PowerState::Charging;
            }
            walk.push(next.clone());
            current = next;
        }

        walk
    })
}

/// Every notification produced by feeding `samples` to a fresh engine.
fn evaluate_all(samples: &[Sample]) -> Vec<(usize, AlertFamily)> {
    let mut engine = AlertEngine::new();
    let thresholds = Thresholds::default();
    let mut seen = Vec::new();

    for (tick, sample) in samples.iter().enumerate() {
        for notification in engine.evaluate(sample, &thresholds) {
            seen.push((tick, notification.family));
        }
    }
    seen
}

proptest! {
    /// A single tick can never announce the same family twice.
    #[test]
    fn a_tick_announces_each_family_at_most_once(samples in arb_walk(200)) {
        let mut engine = AlertEngine::new();
        let thresholds = Thresholds::default();

        for sample in &samples {
            let alerts = engine.evaluate(sample, &thresholds);
            let mut families: Vec<_> = alerts.iter().map(|n| n.family).collect();
            let before = families.len();
            families.sort_unstable_by_key(|f| format!("{f:?}"));
            families.dedup();
            prop_assert_eq!(before, families.len(), "duplicate family in one tick");
        }
    }

    /// An unchanging reading announces at most once per family, however long it
    /// persists. This is the notification-storm invariant: hysteresis exists so
    /// a machine parked at a threshold does not talk forever.
    #[test]
    fn a_constant_reading_announces_at_most_once_per_family(
        sample in arb_sample(),
        ticks in 1usize..300,
    ) {
        let repeated: Vec<_> = std::iter::repeat_n(sample, ticks).collect();
        let announced = evaluate_all(&repeated);

        for family in [
            AlertFamily::Charge,
            AlertFamily::BatteryTemp,
            AlertFamily::Health,
            AlertFamily::Voltage,
            AlertFamily::CpuHeat,
            AlertFamily::ThermalAnomaly,
        ] {
            let count = announced.iter().filter(|(_, f)| *f == family).count();
            prop_assert!(count <= 1, "{family:?} announced {count} times");
        }
    }

    /// Heat-soak is a charging alert and the anomaly detector is a discharging
    /// one. Neither may ever fire on the wrong side of that line.
    #[test]
    fn the_thermal_families_stay_on_their_own_side_of_the_charger(
        samples in arb_walk(200),
    ) {
        let mut engine = AlertEngine::new();
        let thresholds = Thresholds::default();

        for sample in &samples {
            for notification in engine.evaluate(sample, &thresholds) {
                match notification.family {
                    AlertFamily::CpuHeat => prop_assert!(sample.is_charging),
                    AlertFamily::ThermalAnomaly => prop_assert!(!sample.is_charging),
                    _ => {}
                }
            }
        }
    }

    /// Nothing in the charge ladder may fire while the machine is on mains or
    /// the rail is unreadable — the reminders it produces would all be wrong.
    #[test]
    fn the_charge_ladder_stays_silent_off_the_discharge_path(
        levels in prop::collection::vec(0.0f64..=100.0, 0..200),
    ) {
        let mut engine = AlertEngine::new();
        let thresholds = Thresholds::default();

        for (index, level) in levels.iter().enumerate() {
            let state = if index % 2 == 0 { PowerState::AcIdle } else { PowerState::Unknown };
            let sample = Sample {
                charge_pct: *level,
                power_state: state,
                is_charging: false,
                is_present: true,
                ..Sample::default()
            };

            for notification in engine.evaluate(&sample, &thresholds) {
                prop_assert_ne!(notification.family, AlertFamily::Charge);
            }
        }
    }

    /// The debounced families cannot possibly announce before enough samples
    /// have been seen to satisfy their streak requirement.
    #[test]
    fn debounced_families_cannot_announce_before_their_streak_completes(
        samples in prop::collection::vec(arb_sample(), 0..3),
    ) {
        let thresholds = Thresholds::default();
        prop_assume!(samples.len() < thresholds.cpu_heat_debounce_samples as usize);

        for (_, family) in evaluate_all(&samples) {
            prop_assert!(
                !matches!(family, AlertFamily::CpuHeat | AlertFamily::ThermalAnomaly),
                "{family:?} announced after only {} samples",
                samples.len()
            );
        }
    }

    /// A reset engine is indistinguishable from a new one.
    #[test]
    fn resetting_is_equivalent_to_starting_over(
        history in arb_walk(100),
        future in arb_walk(100),
    ) {
        let thresholds = Thresholds::default();

        let mut used = AlertEngine::new();
        for sample in &history {
            let _ = used.evaluate(sample, &thresholds);
        }
        used.reset();

        let mut fresh = AlertEngine::new();

        for sample in &future {
            let from_reset = used.evaluate(sample, &thresholds);
            let from_fresh = fresh.evaluate(sample, &thresholds);
            prop_assert_eq!(from_reset, from_fresh);
        }
    }

    /// A nominal machine is silent forever. Any alert here is a false positive,
    /// and false positives are what train people to ignore real ones.
    #[test]
    fn a_healthy_machine_is_never_interrupted(ticks in 1usize..500) {
        let nominal = Sample {
            charge_pct: 55.0,
            power_state: PowerState::Discharging,
            is_charging: false,
            is_present: true,
            battery_temp_c: Some(31.0),
            health_pct: 96.0,
            voltage_v: 12.0,
            voltage_design_v: 11.8,
            cpu_temp_c: Some(52.0),
            cpu_pct: Some(35.0),
            power_w: 14.0,
            ..Sample::default()
        };

        let announced = evaluate_all(&std::iter::repeat_n(nominal, ticks).collect::<Vec<_>>());
        prop_assert!(announced.is_empty(), "{announced:?}");
    }

    /// Non-finite readings reach the engine when a sensor misbehaves. They must
    /// not panic it, and they must not be mistaken for a threshold crossing —
    /// every comparison against NaN is false, which is the behaviour we want.
    #[test]
    fn pathological_readings_never_panic_the_engine(
        charge in prop::num::f64::ANY,
        temp in prop::num::f64::ANY,
        cpu in prop::num::f64::ANY,
        power in prop::num::f64::ANY,
        voltage in prop::num::f64::ANY,
    ) {
        let mut engine = AlertEngine::new();
        let thresholds = Thresholds::default();
        let sample = Sample {
            charge_pct: charge,
            battery_temp_c: Some(temp),
            cpu_temp_c: Some(cpu),
            cpu_pct: Some(cpu),
            power_w: power,
            voltage_v: voltage,
            voltage_design_v: voltage,
            health_pct: charge,
            power_state: PowerState::Discharging,
            is_present: true,
            ..Sample::default()
        };

        for _ in 0..5 {
            let _ = engine.evaluate(&sample, &thresholds);
        }
    }
}

#[cfg(test)]
mod coverage {
    //! A property test that never generates an input reaching the code it
    //! describes proves nothing, and does it silently. This asserts the
    //! generator above is not vacuous.

    use super::*;
    use proptest::strategy::{Strategy, ValueTree};
    use proptest::test_runner::TestRunner;

    #[test]
    fn the_walk_generator_reaches_every_alert_family() {
        let mut runner = TestRunner::deterministic();
        let mut seen = std::collections::BTreeSet::new();

        for _ in 0..2_000 {
            let samples = arb_walk(40).new_tree(&mut runner).unwrap().current();
            for (_, family) in evaluate_all(&samples) {
                seen.insert(format!("{family:?}"));
            }
        }

        // This check earned its place: with independently drawn samples rather
        // than a walk, heat-soak appeared once in two thousand sequences and the
        // anomaly detector never at all, so both were being asserted about
        // without ever being run.
        assert_eq!(
            seen.len(),
            6,
            "the generator no longer reaches every family, only: {seen:?}"
        );
    }
}
