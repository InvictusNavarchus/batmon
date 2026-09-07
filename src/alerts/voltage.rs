//! Charging voltage above the pack's design rating.

use crate::alerts::notify::{AlertFamily, Notification, Urgency};
use crate::config::Thresholds;
use crate::formats::format_decimals;
use crate::types::Sample;

/// Whether the over-voltage warning has been shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VoltageState {
    #[default]
    Normal,
    Fired,
}

impl VoltageState {
    /// Advance one tick.
    ///
    /// Only meaningful while charging: a rail above design voltage is a charger
    /// or regulator fault, and there is no charger involved when discharging.
    /// Unplugging therefore clears the latch outright.
    ///
    /// Zero or missing voltage readings are neither a fault nor a clear. Some
    /// packs report nothing until the first charge completes, and treating an
    /// absent design voltage as "not over-voltage" would silently release a
    /// latch that a real fault had set.
    #[must_use]
    pub fn step(self, sample: &Sample, t: &Thresholds) -> (Self, Option<Notification>) {
        if !sample.is_charging {
            return (Self::Normal, None);
        }

        if sample.voltage_design_v <= 0.0 || sample.voltage_v <= 0.0 {
            return (self, None);
        }

        if sample.voltage_v > sample.voltage_design_v * t.voltage_over_ratio {
            if self == Self::Fired {
                return (self, None);
            }
            return (
                Self::Fired,
                Some(Notification {
                    family: AlertFamily::Voltage,
                    title: "Warning: Over-Voltage Charging".to_owned(),
                    body: format!(
                        "Voltage {} V well above design {} V",
                        format_decimals(sample.voltage_v, 2),
                        sample.voltage_design_v
                    ),
                    urgency: Urgency::Normal,
                    icon: "dialog-warning",
                }),
            );
        }

        // The clearing point sits below the tripping point, so a rail hovering
        // between the two holds whatever state it already had.
        if sample.voltage_v <= sample.voltage_design_v * t.voltage_clear_ratio {
            return (Self::Normal, None);
        }

        (self, None)
    }
}
