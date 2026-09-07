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
mod tests;
