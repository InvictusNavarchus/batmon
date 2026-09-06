//! Gathering one tick of hardware telemetry.
//!
//! Everything is read directly from the kernel's own interfaces — `/sys` and
//! `/proc` — with no subprocesses and no helper daemons. That is the whole
//! premise of the tool: a recorder sampling once a second has to cost almost
//! nothing, or it changes the very power and thermal behaviour it exists to
//! observe.

pub mod battery;
pub mod thermal;

pub use battery::{BatteryReader, Energy};
pub use thermal::{ThermalReader, Thermals};
