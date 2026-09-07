//! The charge ladder: unplug reminder, low battery, critical low.

use crate::alerts::notify::{AlertFamily, Notification, Urgency};
use crate::config::Thresholds;
use crate::types::{PowerState, Sample};

/// Which charge alert, if any, is currently latched.
///
/// A single state rather than three independent booleans. With booleans the
/// critical alert has to set two latches at once so that the low alert stays
/// suppressed — a relationship no type enforces, upheld only by a comment —
/// while a combination that must never occur, low latched without critical,
/// remains representable. Naming the reachable states makes the suppression
/// structural: there is no value here meaning "both fired".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChargeState {
    #[default]
    Normal,
    /// The unplug reminder has fired and has not re-armed.
    HighFired,
    /// The low-battery reminder has fired.
    LowFired,
    /// The critical alert has fired; the low reminder is suppressed beneath it.
    CriticalFired,
}

impl ChargeState {
    /// Advance one tick, returning the new state and any alert to deliver.
    #[must_use]
    pub fn step(self, sample: &Sample, t: &Thresholds) -> (Self, Option<Notification>) {
        // The unplug reminder re-arms on level alone, whatever the rail is
        // doing, so this runs before the power-state dispatch.
        let state = if self == Self::HighFired
            && sample.charge_pct < t.charge_high_warn - t.charge_hysteresis_pct
        {
            Self::Normal
        } else {
            self
        };

        // `is_charging` is checked alongside the power state because a driver
        // can report a charging current without a matching status string.
        if sample.is_charging || sample.power_state == PowerState::Charging {
            return charging(state, sample, t);
        }

        match sample.power_state {
            // Plugged in but idle: drop the discharge latches without
            // suggesting the user connect a charger they already connected.
            PowerState::AcIdle => (state.without_discharge_latches(), None),
            PowerState::Discharging => discharging(state, sample, t),
            // Nothing is known about the rail, so nothing is claimed about it —
            // and no latch is disturbed either.
            PowerState::Charging | PowerState::Unknown => (state, None),
        }
    }

    fn without_discharge_latches(self) -> Self {
        match self {
            Self::LowFired | Self::CriticalFired => Self::Normal,
            other => other,
        }
    }
}

fn charging(
    state: ChargeState,
    sample: &Sample,
    t: &Thresholds,
) -> (ChargeState, Option<Notification>) {
    let state = state.without_discharge_latches();

    if sample.charge_pct >= t.charge_high_warn && state != ChargeState::HighFired {
        return (
            ChargeState::HighFired,
            Some(Notification {
                family: AlertFamily::Charge,
                title: "Battery Charge Target Reached".to_owned(),
                body: format!(
                    "Level reached {}% – unplug charger to preserve health",
                    sample.charge_pct
                ),
                urgency: Urgency::Normal,
                icon: "battery-full-charging",
            }),
        );
    }

    (state, None)
}

fn discharging(
    state: ChargeState,
    sample: &Sample,
    t: &Thresholds,
) -> (ChargeState, Option<Notification>) {
    let charge = sample.charge_pct;

    if charge <= t.charge_crit_warn {
        if state == ChargeState::CriticalFired {
            return (state, None);
        }
        // Escalating straight from Normal is expected: a level that drops
        // through both thresholds between two samples must produce the critical
        // alert alone, never both.
        return (
            ChargeState::CriticalFired,
            Some(Notification {
                family: AlertFamily::Charge,
                title: "CRITICAL: Battery Low".to_owned(),
                body: format!("{charge}% remaining – connect charger immediately"),
                urgency: Urgency::Critical,
                icon: "battery-empty",
            }),
        );
    }

    if charge <= t.charge_low_warn {
        // Recovered past the critical band, but still inside the low one: the
        // critical latch releases and the low latch stays, so climbing back
        // through 10% does not re-announce a low battery already announced.
        let state = if state == ChargeState::CriticalFired
            && charge > t.charge_crit_warn + t.charge_hysteresis_pct
        {
            ChargeState::LowFired
        } else {
            state
        };

        if matches!(state, ChargeState::LowFired | ChargeState::CriticalFired) {
            return (state, None);
        }

        return (
            ChargeState::LowFired,
            Some(Notification {
                family: AlertFamily::Charge,
                title: "Low Battery".to_owned(),
                body: format!("{charge}% remaining – plug in charger"),
                urgency: Urgency::Normal,
                icon: "battery-caution",
            }),
        );
    }

    // Above the low threshold. Both latches release once the level has climbed
    // clear of the deadband; between the threshold and the band, the low latch
    // holds so dithering across 20% stays silent.
    let state = match state {
        ChargeState::LowFired | ChargeState::CriticalFired => {
            if charge > t.charge_low_warn + t.charge_hysteresis_pct {
                ChargeState::Normal
            } else {
                ChargeState::LowFired
            }
        }
        other => other,
    };

    (state, None)
}
