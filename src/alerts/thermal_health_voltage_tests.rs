use super::fixtures::{charging_at, discharging_at, mock};
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

fn at_temp(celsius: Option<f64>) -> Sample {
    Sample {
        battery_temp_c: celsius,
        ..mock()
    }
}

#[test]
fn battery_temperature_warns_escalates_and_re_arms() {
    let mut engine = AlertEngine::new();

    assert_eq!(
        run(&mut engine, &[at_temp(Some(46.0))]),
        vec!["Warning: High Battery Temperature"]
    );

    // Flapping inside the 3 °C band stays silent.
    assert!(run(&mut engine, &[at_temp(Some(44.0)), at_temp(Some(46.0))]).is_empty());

    assert_eq!(
        run(&mut engine, &[at_temp(Some(51.0))]),
        vec!["CRITICAL: Battery Overheating"]
    );

    // Cooling to 48 is still inside the critical deadband.
    assert!(run(&mut engine, &[at_temp(Some(48.0)), at_temp(Some(51.0))]).is_empty());

    // A full cool-down re-arms both, silently.
    assert!(run(&mut engine, &[at_temp(Some(35.0))]).is_empty());

    assert_eq!(
        run(&mut engine, &[at_temp(Some(51.0))]),
        vec!["CRITICAL: Battery Overheating"]
    );
}

#[test]
fn the_overheating_advice_depends_on_what_is_heating_the_pack() {
    let thresholds = Thresholds::default();

    let mut engine = AlertEngine::new();
    let discharging = engine.evaluate(
        &Sample {
            battery_temp_c: Some(51.0),
            ..discharging_at(50.0)
        },
        &thresholds,
    );
    assert!(
        discharging[0]
            .body
            .contains("reduce system load immediately"),
        "{:?}",
        discharging[0].body
    );

    let mut engine = AlertEngine::new();
    let charging = engine.evaluate(
        &Sample {
            battery_temp_c: Some(51.0),
            ..charging_at(50.0)
        },
        &thresholds,
    );
    let overheating = charging
        .iter()
        .find(|n| n.family == AlertFamily::BatteryTemp)
        .unwrap();
    assert!(
        overheating.body.contains("unplug charger immediately"),
        "{:?}",
        overheating.body
    );
}

#[test]
fn a_dropped_temperature_reading_is_not_a_cool_down() {
    let mut engine = AlertEngine::new();
    assert_eq!(run(&mut engine, &[at_temp(Some(46.0))]).len(), 1);

    // The sensor vanishes for a tick, then returns at the same temperature.
    assert!(run(&mut engine, &[at_temp(None)]).is_empty());
    assert!(
        run(&mut engine, &[at_temp(Some(46.0))]).is_empty(),
        "a gap must not re-arm the latch"
    );
}

#[test]
fn cooling_out_of_critical_into_the_warm_band_does_not_re_announce() {
    let mut engine = AlertEngine::new();
    assert_eq!(run(&mut engine, &[at_temp(Some(51.0))]).len(), 1);

    // 46 is clear of the critical deadband but still above the warning
    // point; the critical latch must release into the warning one.
    assert!(run(&mut engine, &[at_temp(Some(46.0))]).is_empty());
    // And 44 is inside the warning deadband, so still nothing.
    assert!(run(&mut engine, &[at_temp(Some(44.0))]).is_empty());
}

#[test]
fn the_temperature_body_renders_one_decimal_rounded_away_from_zero() {
    let mut engine = AlertEngine::new();
    let alerts = engine.evaluate(&at_temp(Some(45.25)), &Thresholds::default());

    assert_eq!(alerts[0].body, "Battery at 45.3 °C");
}

#[test]
fn health_warns_once_and_holds_through_its_deadband() {
    let mut engine = AlertEngine::new();
    let degraded = |pct: f64| Sample {
        health_pct: pct,
        ..mock()
    };

    assert_eq!(
        run(&mut engine, &[degraded(78.0)]),
        vec!["Battery Health Notice"]
    );

    // Wobbling below the re-arm point stays silent.
    assert!(run(&mut engine, &[degraded(79.0), degraded(78.0)]).is_empty());

    // Recovering clear of the band re-arms without announcing.
    assert!(run(&mut engine, &[degraded(83.0)]).is_empty());

    assert_eq!(
        run(&mut engine, &[degraded(78.0)]),
        vec!["Battery Health Notice"]
    );
}

#[test]
fn the_health_body_reports_the_figure_to_one_decimal() {
    let mut engine = AlertEngine::new();
    let alerts = engine.evaluate(
        &Sample {
            health_pct: 77.99,
            ..mock()
        },
        &Thresholds::default(),
    );

    assert_eq!(alerts[0].body, "Battery health at 78.0% of design capacity");
}

#[test]
fn over_voltage_fires_once_while_charging() {
    let mut engine = AlertEngine::new();
    let charging_at_volts = |v: f64, design: f64| Sample {
        voltage_v: v,
        voltage_design_v: design,
        ..charging_at(50.0)
    };

    // A pack that reports no design voltage cannot be judged.
    assert!(run(&mut engine, &[charging_at_volts(12.0, 0.0)]).is_empty());

    // 14.0 V exceeds 12.0 V by more than the 15% trip ratio.
    assert_eq!(
        run(&mut engine, &[charging_at_volts(14.0, 12.0)]),
        vec!["Warning: Over-Voltage Charging"]
    );

    // Hovering above the trip point does not re-announce.
    assert!(run(&mut engine, &[charging_at_volts(14.2, 12.0)]).is_empty());
}

#[test]
fn over_voltage_clears_on_unplugging_and_can_fire_again() {
    let mut engine = AlertEngine::new();
    let over = Sample {
        voltage_v: 14.0,
        voltage_design_v: 12.0,
        ..charging_at(50.0)
    };

    assert_eq!(run(&mut engine, std::slice::from_ref(&over)).len(), 1);
    assert!(run(&mut engine, &[discharging_at(50.0)]).is_empty());
    assert_eq!(run(&mut engine, &[over]).len(), 1);
}

#[test]
fn over_voltage_holds_between_the_trip_and_clear_ratios() {
    // Trips above 13.8 V, clears at or below 13.2 V. Between the two the
    // latch must persist, or a rail sitting at 13.5 V would flap.
    let mut engine = AlertEngine::new();
    let at_volts = |v: f64| Sample {
        voltage_v: v,
        voltage_design_v: 12.0,
        ..charging_at(50.0)
    };

    assert_eq!(run(&mut engine, &[at_volts(14.0)]).len(), 1);
    assert!(run(&mut engine, &[at_volts(13.5)]).is_empty());
    assert!(
        run(&mut engine, &[at_volts(14.0)]).is_empty(),
        "the latch must survive the deadband"
    );

    // Falling to the clear ratio re-arms it.
    assert!(run(&mut engine, &[at_volts(13.0)]).is_empty());
    assert_eq!(run(&mut engine, &[at_volts(14.0)]).len(), 1);
}

#[test]
fn the_voltage_body_renders_measured_to_two_decimals_and_design_verbatim() {
    let mut engine = AlertEngine::new();
    let alerts = engine.evaluate(
        &Sample {
            voltage_v: 15.125,
            voltage_design_v: 11.55,
            ..charging_at(50.0)
        },
        &Thresholds::default(),
    );

    let voltage = alerts
        .iter()
        .find(|n| n.family == AlertFamily::Voltage)
        .unwrap();
    assert_eq!(voltage.body, "Voltage 15.13 V well above design 11.55 V");
}

#[test]
fn resetting_drops_the_thermal_health_and_voltage_latches_too() {
    let mut engine = AlertEngine::new();
    assert_eq!(run(&mut engine, &[at_temp(Some(46.0))]).len(), 1);

    engine.reset();

    assert_eq!(run(&mut engine, &[at_temp(Some(46.0))]).len(), 1);
}
