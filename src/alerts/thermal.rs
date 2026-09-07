//! Battery pack temperature: warning, critical escalation, and the deadbands
//! between them.

use crate::alerts::notify::{AlertFamily, Notification, Urgency};
use crate::config::Thresholds;
use crate::formats::format_decimals;
use crate::types::Sample;

/// Which battery temperature alert is currently latched.
///
/// Three states, not two booleans. The TypeScript set `tempWarnFired` alongside
/// `tempCritFired` with a comment reading "critical suppresses warning alert";
/// the combination it never constructed — critical latched *without* the warning
/// suppressed — simply has no name here, so it cannot occur.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThermalState {
    #[default]
    Normal,
    WarnFired,
    /// Critical has fired; the warning is suppressed beneath it.
    CriticalFired,
}

impl ThermalState {
    /// Advance one tick.
    ///
    /// A missing reading returns the state untouched and says nothing. Battery
    /// temperature sensors drop out transiently, and treating a gap as "cooled
    /// down" would re-arm the latch and re-announce the same heat a second later.
    #[must_use]
    pub fn step(self, sample: &Sample, t: &Thresholds) -> (Self, Option<Notification>) {
        let Some(temp) = sample.battery_temp_c else {
            return (self, None);
        };

        if temp >= t.temp_crit {
            if self == Self::CriticalFired {
                return (self, None);
            }
            // The advice depends on what is actually heating the pack: charging
            // current is something the user can stop immediately, load is not.
            let advice = if sample.is_charging {
                "unplug charger immediately"
            } else {
                "reduce system load immediately"
            };
            return (
                Self::CriticalFired,
                Some(Notification {
                    family: AlertFamily::BatteryTemp,
                    title: "CRITICAL: Battery Overheating".to_owned(),
                    body: format!("Battery at {} °C – {advice}", format_decimals(temp, 1)),
                    urgency: Urgency::Critical,
                    icon: "dialog-warning",
                }),
            );
        }

        if temp >= t.temp_warn {
            // Fallen clear of the critical deadband but still warm: the critical
            // latch releases into the warning one, so cooling from 51 to 48 does
            // not re-announce a high temperature already announced.
            let state = if self == Self::CriticalFired && temp < t.temp_crit - t.temp_hysteresis_c {
                Self::WarnFired
            } else {
                self
            };

            if state != Self::Normal {
                return (state, None);
            }

            return (
                Self::WarnFired,
                Some(Notification {
                    family: AlertFamily::BatteryTemp,
                    title: "Warning: High Battery Temperature".to_owned(),
                    body: format!("Battery at {} °C", format_decimals(temp, 1)),
                    urgency: Urgency::Normal,
                    icon: "dialog-warning",
                }),
            );
        }

        // Below the warning point. Both latches release only once the pack has
        // cooled clear of the band, so hovering just under the threshold stays
        // silent rather than re-arming for an immediate second announcement.
        let state = if temp < t.temp_warn - t.temp_hysteresis_c {
            Self::Normal
        } else {
            // Between the deadband floor and the warning point: the warning
            // stays latched, but critical has long since cleared.
            match self {
                Self::CriticalFired => Self::WarnFired,
                other => other,
            }
        };

        (state, None)
    }
}
