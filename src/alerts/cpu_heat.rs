//! Heat-soak: charging while the CPU is hot.
//!
//! Charging warms the pack from inside while a hot CPU warms it from outside.
//! The combination is the single worst thing for cell longevity that a user can
//! actually do something about, which is why this alert exists at all and why it
//! only fires while a charger is attached.

use crate::alerts::debounce::{Debounced, Observation};
use crate::alerts::notify::{AlertFamily, Notification, Urgency};
use crate::config::Thresholds;
use crate::formats::to_fixed;
use crate::types::Sample;

/// Debounce latch for the heat-soak warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CpuHeatState(Debounced);

impl CpuHeatState {
    #[must_use]
    pub fn step(self, sample: &Sample, t: &Thresholds) -> (Self, Option<Notification>) {
        // Unplugged, the alert is meaningless and its latch is dropped outright.
        if !sample.is_charging {
            return (Self(Debounced::Clear), None);
        }

        // The None and deadband arms deliberately stay apart despite sharing a
        // body: one means the sensor said nothing, the other means it said
        // something inconclusive. Merging them would erase a real distinction.
        #[allow(clippy::match_same_arms)]
        let observation = match sample.cpu_temp_c {
            // Charging but the sensor said nothing. The streak breaks, because a
            // streak has to be consecutive, but a latch already set stands.
            None => Observation::Neutral,
            Some(temp) if temp >= t.cpu_hot_charging => Observation::Qualifying,
            Some(temp) if temp < t.cpu_hot_charging - t.cpu_temp_hysteresis_c => {
                Observation::Clearing
            }
            Some(_) => Observation::Neutral,
        };

        let (state, fired) = self.0.observe(observation, t.cpu_heat_debounce_samples);
        if !fired {
            return (Self(state), None);
        }

        let temp = sample
            .cpu_temp_c
            .expect("a qualifying observation requires a reading");

        (
            Self(state),
            Some(Notification {
                family: AlertFamily::CpuHeat,
                title: "Warning: Heat-Soak Risk".to_owned(),
                body: format!(
                    "Charging while CPU at {} °C – unplug charger to preserve health",
                    to_fixed(temp, 0)
                ),
                urgency: Urgency::Normal,
                icon: "dialog-warning",
            }),
        )
    }
}
