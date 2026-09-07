//! High CPU temperature during a low workload.
//!
//! The most diagnostically valuable alert here, and the reason the whole flight
//! recorder exists. A machine at 84 °C while compiling is working; a machine at
//! 84 °C while idle has a cooling fault — a failing fan, a blocked vent, dried
//! thermal paste — and that is worth interrupting someone over, because it gets
//! worse and it damages the hardware.
//!
//! Only meaningful while discharging: charging is covered by the heat-soak
//! warning, which is the more actionable framing when a charger is attached.

use crate::alerts::debounce::{Debounced, Observation};
use crate::alerts::notify::{AlertFamily, Notification, Urgency};
use crate::config::Thresholds;
use crate::parity::to_fixed;
use crate::types::Sample;

/// Debounce latch for the thermal anomaly detector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AnomalyState(Debounced);

/// Why a sample counted as idle, which decides how the alert describes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IdleEvidence {
    /// CPU utilisation was low.
    Utilisation,
    /// Discharge power was low.
    Power,
}

impl AnomalyState {
    #[must_use]
    pub fn step(self, sample: &Sample, t: &Thresholds) -> (Self, Option<Notification>) {
        if sample.is_charging {
            return (Self(Debounced::Clear), None);
        }

        let evidence = idle_evidence(sample, t);

        // The None and deadband arms deliberately stay apart despite sharing a
        // body: one means the sensor said nothing, the other means it said
        // something inconclusive. Merging them would erase a real distinction.
        #[allow(clippy::match_same_arms)]
        let observation = match sample.cpu_temp_c {
            None => Observation::Neutral,
            Some(temp) if temp >= t.cpu_anomaly_temp && evidence.is_some() => {
                Observation::Qualifying
            }
            Some(temp) if temp < t.cpu_anomaly_temp - t.cpu_anomaly_hysteresis_c => {
                Observation::Clearing
            }
            // Hot but busy, or inside the deadband. Neither confirms nor denies.
            Some(_) => Observation::Neutral,
        };

        let (state, fired) = self.0.observe(observation, t.cpu_anomaly_debounce_samples);
        if !fired {
            return (Self(state), None);
        }

        let temp = sample
            .cpu_temp_c
            .expect("a qualifying observation requires a reading");
        let detail = match evidence.expect("a qualifying observation requires idle evidence") {
            IdleEvidence::Utilisation => {
                format!("{}% CPU", to_fixed(sample.cpu_pct.unwrap_or_default(), 0))
            }
            IdleEvidence::Power => format!("{} W", to_fixed(sample.power_w, 1)),
        };

        (
            Self(state),
            Some(Notification {
                family: AlertFamily::ThermalAnomaly,
                title: "CRITICAL: Thermal Anomaly".to_owned(),
                body: format!(
                    "CPU at {} °C during low workload ({detail}) – check cooling fans & ventilation",
                    to_fixed(temp, 0)
                ),
                urgency: Urgency::Critical,
                icon: "dialog-error",
            }),
        )
    }
}

/// Whether the machine looks idle, and on what grounds.
///
/// Two independent signals, either of which suffices. Utilisation is the
/// clearer one but can miss work that is not CPU-bound; discharge power catches
/// a machine drawing almost nothing regardless of what the scheduler reports.
/// Utilisation is preferred when both hold, because "8% CPU" is more legible in
/// a notification than a wattage.
fn idle_evidence(sample: &Sample, t: &Thresholds) -> Option<IdleEvidence> {
    if sample
        .cpu_pct
        .is_some_and(|pct| pct <= t.cpu_anomaly_max_load_pct)
    {
        return Some(IdleEvidence::Utilisation);
    }

    // A zero reading means the sensor is not reporting, not that the machine is
    // drawing no power, so it does not count as evidence of idleness.
    if sample.power_w > 0.0 && sample.power_w <= t.cpu_anomaly_max_power_w {
        return Some(IdleEvidence::Power);
    }

    None
}
