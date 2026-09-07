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
#[cfg(test)]
mod properties;
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
mod tests;

#[cfg(test)]
mod thermal_health_voltage_tests;

#[cfg(test)]
mod debounced_family_tests;
