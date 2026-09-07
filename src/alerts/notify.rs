//! What an alert is, and where one goes.
//!
//! Delivery sits behind a trait so the engine can be tested without a desktop
//! session, a message bus, or a subprocess. This is one of only three seams in
//! the crate; the state machines themselves need none, because they are pure.

/// How loudly the desktop should present a notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Urgency {
    Low,
    Normal,
    /// Bypasses do-not-disturb on most desktops, and does not auto-dismiss.
    Critical,
}

impl Urgency {
    /// The freedesktop notification-spec hint value.
    #[must_use]
    pub fn as_hint(self) -> u8 {
        match self {
            Self::Low => 0,
            Self::Normal => 1,
            Self::Critical => 2,
        }
    }

    /// Lowercase name, as used in log lines and by `notify-send -u`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Normal => "normal",
            Self::Critical => "critical",
        }
    }
}

/// Which state machine produced a notification.
///
/// Carried so a re-fire can replace the previous bubble from the same family
/// rather than stacking a second one beside it. A hysteresis engine that piles
/// up notifications is defeating its own purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AlertFamily {
    /// Charge level: unplug reminder, low battery, critical low.
    Charge,
    /// Battery pack temperature.
    BatteryTemp,
    /// Long-term capacity wear.
    Health,
    /// Charging voltage above design.
    Voltage,
    /// Heat-soak: charging while the CPU is hot.
    CpuHeat,
    /// High CPU temperature during a low workload.
    ThermalAnomaly,
}

/// One alert, ready to deliver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub family: AlertFamily,
    pub title: String,
    pub body: String,
    pub urgency: Urgency,
    /// Freedesktop icon name.
    pub icon: &'static str,
}

/// Somewhere for an alert to go.
pub trait Notifier {
    /// Deliver one notification.
    ///
    /// Infallible by design. A desktop that is not listening — no session bus,
    /// no notification daemon, a locked greeter — must never take the recorder
    /// down with it; implementations log and move on.
    fn deliver(&mut self, notification: &Notification);
}

/// A notifier that keeps everything it is given.
///
/// The test double for the [`Notifier`] seam. Public rather than test-only so
/// integration tests can drive the daemon loop with it.
#[derive(Debug, Default)]
pub struct RecordingNotifier {
    delivered: Vec<Notification>,
}

impl RecordingNotifier {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Everything delivered so far, oldest first.
    #[must_use]
    pub fn delivered(&self) -> &[Notification] {
        &self.delivered
    }

    /// Titles only — the usual granularity for asserting on a sequence.
    #[must_use]
    pub fn titles(&self) -> Vec<&str> {
        self.delivered.iter().map(|n| n.title.as_str()).collect()
    }

    /// Forget everything, so one test can assert across several phases.
    pub fn clear(&mut self) {
        self.delivered.clear();
    }
}

impl Notifier for RecordingNotifier {
    fn deliver(&mut self, notification: &Notification) {
        self.delivered.push(notification.clone());
    }
}

/// A notifier that discards everything.
#[derive(Debug, Default)]
pub struct NullNotifier;

impl Notifier for NullNotifier {
    fn deliver(&mut self, _notification: &Notification) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notification(title: &str) -> Notification {
        Notification {
            family: AlertFamily::Charge,
            title: title.to_owned(),
            body: "body".to_owned(),
            urgency: Urgency::Normal,
            icon: "battery",
        }
    }

    #[test]
    fn urgency_maps_to_the_freedesktop_hint_values() {
        assert_eq!(Urgency::Low.as_hint(), 0);
        assert_eq!(Urgency::Normal.as_hint(), 1);
        assert_eq!(Urgency::Critical.as_hint(), 2);
    }

    #[test]
    fn urgency_names_match_the_notify_send_spelling() {
        assert_eq!(Urgency::Low.as_str(), "low");
        assert_eq!(Urgency::Normal.as_str(), "normal");
        assert_eq!(Urgency::Critical.as_str(), "critical");
    }

    #[test]
    fn the_recording_notifier_preserves_order() {
        let mut notifier = RecordingNotifier::new();
        notifier.deliver(&notification("first"));
        notifier.deliver(&notification("second"));

        assert_eq!(notifier.titles(), vec!["first", "second"]);
    }

    #[test]
    fn clearing_the_recorder_drops_history_but_keeps_it_usable() {
        let mut notifier = RecordingNotifier::new();
        notifier.deliver(&notification("first"));
        notifier.clear();
        notifier.deliver(&notification("second"));

        assert_eq!(notifier.titles(), vec!["second"]);
    }

    #[test]
    fn the_null_notifier_accepts_everything_and_keeps_nothing() {
        let mut notifier = NullNotifier;
        notifier.deliver(&notification("ignored"));
    }
}
