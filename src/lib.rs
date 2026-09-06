//! batmon — battery health monitor and hardware flight recorder for Linux laptops.
//!
//! The crate is deliberately split into a library plus a thin binary. Everything
//! observable lives here as `pub` items, which keeps each module independently
//! testable and — during the port — lets modules land fully tested before the
//! daemon wires them together, without tripping `dead_code`.
//!
//! The daemon is synchronous and single-threaded by design: SQLite, sysfs and
//! procfs all block, so an async runtime would buy nothing and cost a scheduler.

pub mod config;
pub mod cycles;
pub mod migrations;
pub mod parity;
pub mod paths;
pub mod types;
pub mod units;
