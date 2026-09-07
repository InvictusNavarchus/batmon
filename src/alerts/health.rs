//! Long-term capacity wear.

use crate::alerts::notify::{AlertFamily, Notification, Urgency};
use crate::config::Thresholds;
use crate::parity::to_fixed;
use crate::types::Sample;

/// Whether the wear notice has been shown.
///
/// A single latch, because health degrades over months and there is nothing to
/// escalate to. The deadband matters anyway: full-charge capacity is a computed
/// ratio that wobbles by a fraction of a percent between charge cycles, and
/// without it a battery sitting at the threshold would announce itself daily.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HealthState {
    #[default]
    Normal,
    Fired,
}

impl HealthState {
    #[must_use]
    pub fn step(self, sample: &Sample, t: &Thresholds) -> (Self, Option<Notification>) {
        if sample.health_pct < t.cap_warn {
            if self == Self::Fired {
                return (self, None);
            }
            return (
                Self::Fired,
                Some(Notification {
                    family: AlertFamily::Health,
                    title: "Battery Health Notice".to_owned(),
                    body: format!(
                        "Battery health at {}% of design capacity",
                        to_fixed(sample.health_pct, 1)
                    ),
                    urgency: Urgency::Normal,
                    icon: "battery-caution",
                }),
            );
        }

        if sample.health_pct >= t.cap_warn + t.cap_hysteresis_pct {
            return (Self::Normal, None);
        }

        (self, None)
    }
}
