//! Desktop notification delivery over the session bus.
//!
//! Unlike the UPower change, this is not about cost. Alerts fire a handful of
//! times a day, so spawning `notify-send` was never expensive. What the direct
//! call buys is `replaces_id`: `Notify` returns the identifier of the bubble it
//! created, and passing that identifier back on the next alert of the same kind
//! replaces the bubble instead of stacking a second one beside it.
//!
//! That matters more than it sounds. The entire point of the hysteresis engine
//! is to avoid burying the user in notifications, and a re-fired alert that
//! piles up next to its predecessor undoes that work at the last step.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::Value;

use crate::alerts::{AlertFamily, Notification, Notifier, Urgency};

const SERVICE: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
const INTERFACE: &str = "org.freedesktop.Notifications";

/// Application name shown by the notification server.
const APP_NAME: &str = "batmon";

/// Freedesktop notification category, so desktops can group and style these.
const CATEGORY: &str = "device";

/// Let the notification server decide how long a bubble stays up.
const EXPIRE_DEFAULT: i32 = -1;

/// Identifier meaning "this is a new bubble, do not replace anything".
const NO_REPLACEMENT: u32 = 0;

/// Shortest gap between attempts to bind to a notification server.
///
/// A daemon enabled with the user session can start before the desktop's
/// notification service does, and without a retry its alerts would be
/// journal-only until someone restarted it. Alerts are rare enough that
/// retrying on each one costs nothing, and this bound exists only so a machine
/// that genuinely has no bus does not attempt a connection per notification.
const RECONNECT_INTERVAL: Duration = Duration::from_secs(30);

/// The most recent bubble identifier per alert family.
#[derive(Debug, Default)]
struct ReplacementIds(HashMap<AlertFamily, u32>);

impl ReplacementIds {
    /// The bubble to replace for this family, or [`NO_REPLACEMENT`].
    fn previous(&self, family: AlertFamily) -> u32 {
        self.0.get(&family).copied().unwrap_or(NO_REPLACEMENT)
    }

    /// Remember the identifier the server returned.
    ///
    /// A returned zero means the server declined to give us a handle, so there
    /// is nothing to replace next time and the entry is dropped rather than
    /// stored — passing zero back is what asks for a fresh bubble anyway.
    fn record(&mut self, family: AlertFamily, id: u32) {
        if id == NO_REPLACEMENT {
            self.0.remove(&family);
        } else {
            self.0.insert(family, id);
        }
    }
}

/// Sends notifications to the desktop, and logs every one regardless.
pub struct DesktopNotifier {
    server: Option<Proxy<'static>>,
    replacements: ReplacementIds,
    /// When binding was last attempted, successfully or not.
    last_attempt: Instant,
}

impl std::fmt::Debug for DesktopNotifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DesktopNotifier")
            .field("connected", &self.server.is_some())
            .finish_non_exhaustive()
    }
}

impl DesktopNotifier {
    /// Connect to the session bus.
    ///
    /// Never fails. A daemon started before a graphical session, or running on a
    /// headless machine, has no notification server to talk to — and the flight
    /// recorder's job is recording, not talking. Alerts still reach the journal.
    #[must_use]
    pub fn connect() -> Self {
        let server = bind();
        if server.is_none() {
            tracing::warn!(
                "no notification server; alerts will be journalled and delivery retried"
            );
        }

        Self {
            server,
            replacements: ReplacementIds::default(),
            last_attempt: Instant::now(),
        }
    }

    /// Send one notification, returning the identifier the server assigned.
    fn send(&self, notification: &Notification, replaces: u32) -> Option<u32> {
        let server = self.server.as_ref()?;

        let hints: HashMap<&str, Value<'_>> = HashMap::from([
            ("urgency", Value::U8(notification.urgency.as_hint())),
            ("category", Value::Str(CATEGORY.into())),
        ]);

        let arguments = (
            APP_NAME,
            replaces,
            notification.icon,
            notification.title.as_str(),
            notification.body.as_str(),
            &[] as &[&str],
            hints,
            EXPIRE_DEFAULT,
        );

        match server.call::<_, _, u32>("Notify", &arguments) {
            Ok(id) => Some(id),
            Err(error) => {
                tracing::warn!(%error, title = %notification.title, "notification not delivered");
                None
            }
        }
    }
}

impl Notifier for DesktopNotifier {
    fn deliver(&mut self, notification: &Notification) {
        // Logged before sending, and whether or not sending works. The journal
        // is the durable record; the bubble is a courtesy.
        log(notification);

        // Pick up a notification server that appeared after startup, which is
        // the normal case for a user service enabled at login.
        if self.server.is_none() && self.last_attempt.elapsed() >= RECONNECT_INTERVAL {
            self.last_attempt = Instant::now();
            self.server = bind();
            if self.server.is_some() {
                tracing::info!("notification server appeared; delivery resumed");
            }
        }

        let replaces = self.replacements.previous(notification.family);
        if let Some(id) = self.send(notification, replaces) {
            self.replacements.record(notification.family, id);
        }
    }
}

/// Bind to the session bus's notification service, if there is one.
///
/// Logs at debug rather than warn: this runs on every retry, and a headless
/// machine failing repeatedly is expected rather than notable.
fn bind() -> Option<Proxy<'static>> {
    match Connection::session() {
        Ok(connection) => match Proxy::new(&connection, SERVICE, PATH, INTERFACE) {
            Ok(proxy) => Some(proxy),
            Err(error) => {
                tracing::debug!(%error, "no notification server on the session bus");
                None
            }
        },
        Err(error) => {
            tracing::debug!(%error, "no session bus");
            None
        }
    }
}

/// Record an alert in the journal at a level matching its urgency.
fn log(notification: &Notification) {
    let title = notification.title.as_str();
    let body = notification.body.as_str();
    match notification.urgency {
        Urgency::Critical => tracing::error!(%title, %body, "alert"),
        Urgency::Normal => tracing::warn!(%title, %body, "alert"),
        Urgency::Low => tracing::info!(%title, %body, "alert"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notification(family: AlertFamily) -> Notification {
        Notification {
            family,
            title: "Low Battery".to_owned(),
            body: "20% remaining".to_owned(),
            urgency: Urgency::Normal,
            icon: "battery-caution",
        }
    }

    #[test]
    fn a_family_with_no_history_asks_for_a_new_bubble() {
        let ids = ReplacementIds::default();
        assert_eq!(ids.previous(AlertFamily::Charge), NO_REPLACEMENT);
    }

    #[test]
    fn a_recorded_identifier_is_offered_back_for_the_same_family() {
        let mut ids = ReplacementIds::default();
        ids.record(AlertFamily::Charge, 42);

        assert_eq!(ids.previous(AlertFamily::Charge), 42);
    }

    #[test]
    fn families_replace_their_own_bubbles_and_not_each_others() {
        // A thermal alert must not overwrite a low-battery bubble.
        let mut ids = ReplacementIds::default();
        ids.record(AlertFamily::Charge, 42);
        ids.record(AlertFamily::BatteryTemp, 43);

        assert_eq!(ids.previous(AlertFamily::Charge), 42);
        assert_eq!(ids.previous(AlertFamily::BatteryTemp), 43);
        assert_eq!(ids.previous(AlertFamily::Health), NO_REPLACEMENT);
    }

    #[test]
    fn a_later_identifier_supersedes_the_earlier_one() {
        let mut ids = ReplacementIds::default();
        ids.record(AlertFamily::Charge, 42);
        ids.record(AlertFamily::Charge, 77);

        assert_eq!(ids.previous(AlertFamily::Charge), 77);
    }

    #[test]
    fn a_server_that_returns_no_handle_leaves_nothing_to_replace() {
        // Zero is not a bubble; storing it would ask to replace bubble zero.
        let mut ids = ReplacementIds::default();
        ids.record(AlertFamily::Charge, 42);
        ids.record(AlertFamily::Charge, NO_REPLACEMENT);

        assert_eq!(ids.previous(AlertFamily::Charge), NO_REPLACEMENT);
    }

    #[test]
    fn urgency_maps_onto_the_freedesktop_hint_values() {
        // What notify-send -u encoded positionally, sent as a typed hint.
        assert_eq!(Urgency::Low.as_hint(), 0);
        assert_eq!(Urgency::Normal.as_hint(), 1);
        assert_eq!(Urgency::Critical.as_hint(), 2);
    }

    /// A notifier in the state a headless machine produces.
    ///
    /// Constructed directly rather than through `connect()`, which would bind to
    /// whatever session bus happens to be running and — on a desktop — deliver
    /// real notifications during `cargo test` while never reaching the branch
    /// the test claims to cover.
    fn disconnected() -> DesktopNotifier {
        DesktopNotifier {
            server: None,
            replacements: ReplacementIds::default(),
            // Freshly attempted, so delivery inside a test will not reach for
            // the real session bus during the reconnect window.
            last_attempt: Instant::now(),
        }
    }

    #[test]
    fn delivering_without_a_notification_server_does_not_panic() {
        // Headless machines and daemons started before a graphical session must
        // still record; the journal is the durable half.
        let mut notifier = disconnected();
        notifier.deliver(&notification(AlertFamily::Charge));
        notifier.deliver(&notification(AlertFamily::BatteryTemp));
    }

    #[test]
    fn a_disconnected_notifier_does_not_retry_within_the_reconnect_window() {
        // Both that the retry is bounded, and — since the real session bus on a
        // developer machine would answer — that `cargo test` cannot deliver
        // notifications to somebody's desktop.
        let mut notifier = disconnected();
        for _ in 0..20 {
            notifier.deliver(&notification(AlertFamily::Charge));
        }

        assert!(
            notifier.server.is_none(),
            "delivery reached for the real session bus inside the retry window"
        );
    }

    #[test]
    fn a_stale_attempt_makes_the_next_delivery_retry() {
        // The property that matters: a notifier that failed at startup does try
        // again, so a desktop appearing later restores alerts without a restart.
        let mut notifier = disconnected();
        notifier.last_attempt = Instant::now()
            .checked_sub(RECONNECT_INTERVAL + Duration::from_secs(1))
            .expect("the process has not been running since the epoch");

        notifier.deliver(&notification(AlertFamily::Charge));

        assert!(
            notifier.last_attempt.elapsed() < RECONNECT_INTERVAL,
            "the attempt timestamp was not refreshed, so retries would be unbounded"
        );
    }

    #[test]
    fn a_disconnected_notifier_records_no_replacement_ids() {
        // Nothing was delivered, so there is no bubble to replace next time.
        // Storing an id here would ask a future server to replace something
        // that never existed.
        let mut notifier = disconnected();
        notifier.deliver(&notification(AlertFamily::Charge));

        assert_eq!(
            notifier.replacements.previous(AlertFamily::Charge),
            NO_REPLACEMENT
        );
    }
}
