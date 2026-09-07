use super::fixtures::{charging_at, discharging_at};
use super::*;
use crate::types::Sample;

fn run(engine: &mut AlertEngine, samples: &[Sample]) -> Vec<String> {
    let thresholds = Thresholds::default();
    samples
        .iter()
        .flat_map(|s| engine.evaluate(s, &thresholds))
        .map(|n| n.title)
        .collect()
}

/// `times` copies of one sample. Sample owns Strings, so it is Clone, not Copy.
fn repeat(sample: &Sample, times: usize) -> Vec<Sample> {
    std::iter::repeat_n(sample.clone(), times).collect()
}

/// Charging, with the CPU at `celsius` and the battery cool.
fn charging_cpu(celsius: f64) -> Sample {
    Sample {
        cpu_temp_c: Some(celsius),
        battery_temp_c: Some(30.0),
        ..charging_at(50.0)
    }
}

/// Discharging, CPU at `celsius`, with the given utilisation and draw.
fn idle_cpu(celsius: f64, cpu_pct: f64, power_w: f64) -> Sample {
    Sample {
        cpu_temp_c: Some(celsius),
        cpu_pct: Some(cpu_pct),
        power_w,
        battery_temp_c: Some(30.0),
        ..discharging_at(50.0)
    }
}

#[test]
fn heat_soak_requires_three_sustained_samples() {
    let mut engine = AlertEngine::new();

    assert!(run(&mut engine, &[charging_cpu(88.0)]).is_empty());
    assert!(run(&mut engine, &[charging_cpu(88.0)]).is_empty());
    assert_eq!(
        run(&mut engine, &[charging_cpu(88.0)]),
        vec!["Warning: Heat-Soak Risk"]
    );
}

#[test]
fn heat_soak_holds_through_its_deadband_without_re_announcing() {
    let mut engine = AlertEngine::new();
    assert_eq!(run(&mut engine, &repeat(&charging_cpu(88.0), 3)).len(), 1);

    // 82 is inside the 5 °C band, so returning to 88 must stay silent.
    assert!(run(&mut engine, &[charging_cpu(82.0), charging_cpu(88.0)]).is_empty());
}

#[test]
fn transient_spikes_shorter_than_the_debounce_never_fire() {
    let mut engine = AlertEngine::new();

    // Two spikes, then a drop into the deadband, then one more spike. The
    // streak has to be consecutive, so this is never three in a row.
    let quiet = run(
        &mut engine,
        &[
            charging_cpu(89.0),
            charging_cpu(89.0),
            charging_cpu(84.0),
            charging_cpu(89.0),
        ],
    );

    assert!(quiet.is_empty(), "{quiet:?}");
}

#[test]
fn heat_soak_never_fires_while_unplugged() {
    let mut engine = AlertEngine::new();
    let hot_unplugged = Sample {
        cpu_temp_c: Some(95.0),
        cpu_pct: Some(90.0),
        power_w: 45.0,
        battery_temp_c: Some(30.0),
        ..discharging_at(50.0)
    };

    assert!(run(&mut engine, &repeat(&hot_unplugged, 10)).is_empty());
}

#[test]
fn the_heat_soak_body_reports_a_whole_degree() {
    let mut engine = AlertEngine::new();
    let alerts: Vec<_> = repeat(&charging_cpu(88.6), 3)
        .iter()
        .flat_map(|s| engine.evaluate(s, &Thresholds::default()))
        .collect();

    assert_eq!(
        alerts[0].body,
        "Charging while CPU at 89 °C – unplug charger to preserve health"
    );
}

#[test]
fn a_thermal_anomaly_fires_when_the_machine_is_hot_but_idle() {
    // The August 20 incident: 84.1 °C at 8% CPU drawing 2.5 W.
    let mut engine = AlertEngine::new();
    let titles = run(&mut engine, &repeat(&idle_cpu(84.1, 8.0, 2.5), 3));

    assert_eq!(titles, vec!["CRITICAL: Thermal Anomaly"]);
}

#[test]
fn a_hot_machine_under_real_load_stays_completely_silent() {
    // Gaming or compiling at 88 °C is expected dissipation, not a fault.
    let mut engine = AlertEngine::new();
    let quiet = run(&mut engine, &repeat(&idle_cpu(88.0, 85.0, 45.0), 5));

    assert!(quiet.is_empty(), "{quiet:?}");
}

#[test]
fn low_draw_qualifies_as_idle_even_when_utilisation_is_high() {
    // Elevated CPU% but only 10 W: the body must cite the power, since that
    // is the evidence that actually qualified.
    let mut engine = AlertEngine::new();
    let thresholds = Thresholds::default();
    let sample = idle_cpu(84.0, 60.0, 10.0);

    let alerts: Vec<_> = (0..3)
        .flat_map(|_| engine.evaluate(&sample, &thresholds))
        .collect();

    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].title, "CRITICAL: Thermal Anomaly");
    assert!(alerts[0].body.contains("10.0 W"), "{:?}", alerts[0].body);
    assert!(!alerts[0].body.contains("60% CPU"), "{:?}", alerts[0].body);
}

#[test]
fn utilisation_is_preferred_over_power_when_both_qualify() {
    let mut engine = AlertEngine::new();
    let thresholds = Thresholds::default();
    let sample = idle_cpu(84.0, 8.0, 2.5);

    let alerts: Vec<_> = (0..3)
        .flat_map(|_| engine.evaluate(&sample, &thresholds))
        .collect();

    assert!(alerts[0].body.contains("(8% CPU)"), "{:?}", alerts[0].body);
}

#[test]
fn connecting_a_charger_re_arms_the_anomaly_detector() {
    let mut engine = AlertEngine::new();
    let anomalous = idle_cpu(84.0, 60.0, 10.0);

    assert_eq!(run(&mut engine, &repeat(&anomalous, 3)).len(), 1);

    // Plugging in drops the latch outright — heat-soak owns this case now.
    assert!(run(&mut engine, &[charging_cpu(72.0)]).is_empty());

    assert_eq!(run(&mut engine, &repeat(&anomalous, 3)).len(), 1);
}

#[test]
fn a_zero_power_reading_is_not_evidence_of_idleness() {
    // power_w of 0 means the sensor is silent, not that nothing is running.
    // With utilisation high too, nothing qualifies.
    let mut engine = AlertEngine::new();
    let quiet = run(&mut engine, &repeat(&idle_cpu(84.0, 90.0, 0.0), 5));

    assert!(quiet.is_empty(), "{quiet:?}");
}

#[test]
fn a_missing_cpu_reading_breaks_the_streak_but_spares_the_latch() {
    let mut engine = AlertEngine::new();
    let unreadable = Sample {
        cpu_temp_c: None,
        ..charging_at(50.0)
    };

    // Two qualifying samples, a gap, then one more: never three consecutive.
    assert!(
        run(
            &mut engine,
            &[
                charging_cpu(88.0),
                charging_cpu(88.0),
                unreadable.clone(),
                charging_cpu(88.0),
            ]
        )
        .is_empty()
    );

    // But once fired, a gap must not re-arm it.
    assert_eq!(run(&mut engine, &repeat(&charging_cpu(88.0), 2)).len(), 1);
    assert!(run(&mut engine, &[unreadable, charging_cpu(88.0)]).is_empty());
}

#[test]
fn the_two_thermal_families_never_fire_at_the_same_time() {
    // Charging is heat-soak's domain, discharging is the anomaly detector's.
    let mut engine = AlertEngine::new();
    let thresholds = Thresholds::default();

    for _ in 0..10 {
        let alerts = engine.evaluate(&charging_cpu(95.0), &thresholds);
        assert!(
            !alerts
                .iter()
                .any(|n| n.family == AlertFamily::ThermalAnomaly)
        );
    }
}

#[test]
fn resetting_clears_the_debounce_counters_as_well_as_the_latches() {
    let mut engine = AlertEngine::new();
    assert!(run(&mut engine, &repeat(&charging_cpu(88.0), 2)).is_empty());

    engine.reset();

    // The two-sample streak must not survive; three more are needed.
    assert!(run(&mut engine, &repeat(&charging_cpu(88.0), 2)).is_empty());
    assert_eq!(run(&mut engine, &[charging_cpu(88.0)]).len(), 1);
}
