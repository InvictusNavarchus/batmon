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

pub mod anomaly;
pub mod charge;
pub mod cpu_heat;
pub mod debounce;
#[cfg(test)]
pub(crate) mod fixtures;
pub mod health;
pub mod notify;
pub mod thermal;
pub mod voltage;

pub use anomaly::AnomalyState;
pub use charge::ChargeState;
pub use cpu_heat::CpuHeatState;
pub use debounce::Debounced;
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
    cpu_heat: CpuHeatState,
    anomaly: AnomalyState,
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

        let (cpu_heat, alert) = self.cpu_heat.step(sample, thresholds);
        self.cpu_heat = cpu_heat;
        notifications.extend(alert);

        let (anomaly, alert) = self.anomaly.step(sample, thresholds);
        self.anomaly = anomaly;
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

#[cfg(test)]
mod debounced_family_tests {
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
}
