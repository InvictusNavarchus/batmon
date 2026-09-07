use super::fixtures::{ac_idle_at, charging_at, discharging_at, unknown_at};
use super::*;

/// Feed samples in order and collect every title produced.
fn run(engine: &mut AlertEngine, samples: &[Sample]) -> Vec<String> {
    let thresholds = Thresholds::default();
    samples
        .iter()
        .flat_map(|s| engine.evaluate(s, &thresholds))
        .map(|n| n.title)
        .collect()
}

#[test]
fn the_unplug_reminder_fires_once_and_holds_through_its_deadband() {
    let mut engine = AlertEngine::new();

    assert_eq!(
        run(&mut engine, &[charging_at(80.0)]),
        vec!["Battery Charge Target Reached"]
    );

    // Flapping across the threshold stays inside the 5% band.
    assert!(
        run(&mut engine, &[charging_at(79.0), charging_at(80.0)]).is_empty(),
        "dithering must not re-announce"
    );

    // Falling through 75% re-arms it, silently.
    assert!(run(&mut engine, &[charging_at(74.0)]).is_empty());

    assert_eq!(
        run(&mut engine, &[charging_at(80.0)]),
        vec!["Battery Charge Target Reached"]
    );
}

#[test]
fn the_unplug_reminder_stays_quiet_through_a_full_charge_and_a_brief_unplug() {
    let mut engine = AlertEngine::new();
    assert_eq!(run(&mut engine, &[charging_at(80.0)]).len(), 1);

    let quiet = run(
        &mut engine,
        &[
            charging_at(90.0),
            charging_at(95.0),
            charging_at(100.0),
            // The kernel reports Full with no charging current at the top.
            ac_idle_at(100.0),
            // Trickle top-off flips the status back.
            charging_at(100.0),
            // A momentary unplug and reconnect, both above the deadband.
            discharging_at(85.0),
            charging_at(84.0),
        ],
    );

    assert!(quiet.is_empty(), "{quiet:?}");
}

#[test]
fn a_level_that_drops_through_both_thresholds_announces_only_the_critical() {
    let mut engine = AlertEngine::new();

    assert_eq!(
        run(&mut engine, &[discharging_at(9.0)]),
        vec!["CRITICAL: Battery Low"],
        "the low reminder must be suppressed beneath the critical one"
    );
}

#[test]
fn low_escalates_to_critical_without_repeating_itself() {
    let mut engine = AlertEngine::new();

    assert_eq!(
        run(&mut engine, &[discharging_at(20.0)]),
        vec!["Low Battery"]
    );
    assert!(run(&mut engine, &[discharging_at(19.0)]).is_empty());
    assert_eq!(
        run(&mut engine, &[discharging_at(10.0)]),
        vec!["CRITICAL: Battery Low"]
    );
}

#[test]
fn the_low_reminder_re_arms_on_a_charger_or_a_real_recovery() {
    let mut engine = AlertEngine::new();
    assert_eq!(
        run(&mut engine, &[discharging_at(20.0)]),
        vec!["Low Battery"]
    );

    // 22% is inside the deadband, so returning to 20% stays silent.
    assert!(run(&mut engine, &[discharging_at(22.0), discharging_at(20.0)]).is_empty());

    // Connecting a charger drops the latch without announcing anything.
    assert!(run(&mut engine, &[charging_at(20.0)]).is_empty());

    // Unplugging again at the same level re-announces.
    assert_eq!(
        run(&mut engine, &[discharging_at(20.0)]),
        vec!["Low Battery"]
    );
}

#[test]
fn a_recovery_clear_of_the_deadband_re_arms_the_low_reminder() {
    let mut engine = AlertEngine::new();
    assert_eq!(
        run(&mut engine, &[discharging_at(20.0)]),
        vec!["Low Battery"]
    );

    assert!(run(&mut engine, &[discharging_at(26.0)]).is_empty());
    assert_eq!(
        run(&mut engine, &[discharging_at(20.0)]),
        vec!["Low Battery"]
    );
}

#[test]
fn climbing_out_of_critical_into_the_low_band_does_not_re_announce_low() {
    let mut engine = AlertEngine::new();
    assert_eq!(
        run(&mut engine, &[discharging_at(8.0)]),
        vec!["CRITICAL: Battery Low"]
    );

    // Above the critical deadband but still low: the critical latch releases
    // into the low one rather than into Normal, so no second announcement.
    assert!(run(&mut engine, &[discharging_at(18.0)]).is_empty());
}

#[test]
fn being_on_mains_silences_the_discharge_ladder_entirely() {
    let mut engine = AlertEngine::new();

    // Capped at a charge limit, then well into critical territory. Nagging
    // someone to connect a charger they already connected is worse than
    // silence.
    assert!(run(&mut engine, &[ac_idle_at(15.0), ac_idle_at(5.0)]).is_empty());

    // An unreadable rail claims nothing either.
    assert!(run(&mut engine, &[unknown_at(10.0)]).is_empty());
}

#[test]
fn reconnecting_and_unplugging_again_re_announces_critical() {
    let mut engine = AlertEngine::new();
    assert_eq!(
        run(&mut engine, &[discharging_at(10.0)]),
        vec!["CRITICAL: Battery Low"]
    );

    assert!(run(&mut engine, &[ac_idle_at(10.0)]).is_empty());
    assert_eq!(
        run(&mut engine, &[discharging_at(10.0)]),
        vec!["CRITICAL: Battery Low"]
    );
}

#[test]
fn an_unknown_rail_disturbs_no_latch() {
    // Distinct from ac_idle, which clears them: unknown means we learned
    // nothing, so nothing should change.
    let mut engine = AlertEngine::new();
    assert_eq!(
        run(&mut engine, &[discharging_at(20.0)]),
        vec!["Low Battery"]
    );

    assert!(run(&mut engine, &[unknown_at(20.0)]).is_empty());
    assert!(
        run(&mut engine, &[discharging_at(20.0)]).is_empty(),
        "the latch must survive an unknown reading"
    );
}

#[test]
fn notifications_carry_the_level_and_the_right_urgency() {
    let mut engine = AlertEngine::new();
    let thresholds = Thresholds::default();

    let critical = engine.evaluate(&discharging_at(7.0), &thresholds);
    assert_eq!(critical.len(), 1);
    assert_eq!(critical[0].urgency, Urgency::Critical);
    assert_eq!(critical[0].family, AlertFamily::Charge);
    assert_eq!(
        critical[0].body,
        "7% remaining – connect charger immediately"
    );

    let mut engine = AlertEngine::new();
    let high = engine.evaluate(&charging_at(80.0), &thresholds);
    assert_eq!(high[0].urgency, Urgency::Normal);
    assert_eq!(
        high[0].body,
        "Level reached 80% – unplug charger to preserve health"
    );
    assert_eq!(high[0].icon, "battery-full-charging");
}

#[test]
fn a_whole_percentage_renders_without_a_decimal_point() {
    // charge_pct is an f64 but the kernel reports whole percentages; the
    // body text has to read "80%", not "80.0%".
    let mut engine = AlertEngine::new();
    let alerts = engine.evaluate(&charging_at(80.0), &Thresholds::default());
    assert!(
        alerts[0].body.starts_with("Level reached 80% "),
        "{:?}",
        alerts[0].body
    );
}

#[test]
fn resetting_drops_every_latch() {
    let mut engine = AlertEngine::new();
    assert_eq!(
        run(&mut engine, &[discharging_at(20.0)]),
        vec!["Low Battery"]
    );

    engine.reset();

    assert_eq!(
        run(&mut engine, &[discharging_at(20.0)]),
        vec!["Low Battery"],
        "a reset engine must behave like a new one"
    );
}
