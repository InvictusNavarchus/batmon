//! Stateful alerting with hysteresis, debouncing and priority escalation.
//!
//! Every alert family is a state machine. That is the whole design: the
//! TypeScript implementation tracked eleven independent booleans and counters
//! and maintained the relationships between them by hand, with comments like
//! "critical suppresses warning" standing in for an invariant. Here the same
//! rules are structural — there is no value of a thermal state meaning "warning
//! fired and critical fired", so the suppression cannot be forgotten.

pub mod notify;

pub use notify::{AlertFamily, Notification, Notifier, NullNotifier, RecordingNotifier, Urgency};
