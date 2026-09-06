//! Stateful alerting with hysteresis, debouncing and priority escalation.
//!
//! Every alert family is a state machine, and that is the whole design. The
//! TypeScript implementation tracked eleven independent booleans and counters
//! and maintained the relationships between them by hand, with comments like
//! "critical suppresses warning" standing in for an invariant the code could
//! not express. Here those rules are structural: there is no state meaning
//! "warning fired and critical fired", so the suppression cannot be forgotten,
//! and an exhaustive `match` makes the compiler point at every site that would
//! need updating if a state were added.
//!
//! The engine performs no I/O. It consumes a sample and returns notifications;
//! delivering them is the caller's problem.

pub mod charge;
#[cfg(test)]
pub(crate) mod fixtures;
pub mod health;
pub mod notify;
pub mod thermal;
pub mod voltage;

pub use charge::ChargeState;
pub use health::HealthState;
pub use notify::{AlertFamily, Notification, Notifier, NullNotifier, RecordingNotifier, Urgency};
pub use thermal::ThermalState;
pub use voltage::VoltageState;

use crate::config::Thresholds;
use crate::types::Sample;

/// The stateful half of alerting: one latch per family, carried between ticks.
#[derive(Debug, Default)]
pub struct AlertEngine {
    charge: ChargeState,
    battery_temp: ThermalState,
    health: HealthState,
    voltage: VoltageState,
}

impl AlertEngine {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Advance every state machine by one sample, returning what to deliver.
    ///
    /// Pure with respect to the outside world: no notification is sent, no clock
    /// is read. The returned vector is empty on the overwhelming majority of
    /// ticks and `Vec` does not allocate until its first push, so the quiet path
    /// costs nothing.
    pub fn evaluate(&mut self, sample: &Sample, thresholds: &Thresholds) -> Vec<Notification> {
        let mut notifications = Vec::new();

        let (charge, alert) = self.charge.step(sample, thresholds);
        self.charge = charge;
        notifications.extend(alert);

        let (battery_temp, alert) = self.battery_temp.step(sample, thresholds);
        self.battery_temp = battery_temp;
        notifications.extend(alert);

        let (health, alert) = self.health.step(sample, thresholds);
        self.health = health;
        notifications.extend(alert);

        let (voltage, alert) = self.voltage.step(sample, thresholds);
        self.voltage = voltage;
        notifications.extend(alert);

        notifications
    }

    /// Drop every latch.
    ///
    /// Used when the battery disappears — an external pack unplugged, a docking
    /// station detached. Whatever was latched described hardware that is no
    /// longer there.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
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
}

#[cfg(test)]
mod thermal_health_voltage_tests {
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
    fn the_temperature_body_renders_one_decimal_with_javascript_rounding() {
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
}
