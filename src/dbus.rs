//! D-Bus integration.
//!
//! Two connections, not one: UPower lives on the system bus and the notification
//! service on the session bus. Both replace subprocess spawns, but for different
//! reasons — UPower is polled on a timer and was the real cost, while
//! notifications are rare and are moved here for a capability rather than for
//! speed.

pub mod upower;

pub use upower::UPower;
