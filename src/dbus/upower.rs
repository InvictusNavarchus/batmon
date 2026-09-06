//! Smoothed runtime estimates from UPower, over D-Bus.
//!
//! Replaces two `busctl` subprocess spawns per minute — roughly 1,440 process
//! creations a day — with one long-lived connection. For a daemon whose premise
//! is that measuring must not perturb what it measures, that is the single
//! largest avoidable cost in the program.
//!
//! UPower's estimates are used in preference to dividing energy by present draw
//! because UPower averages across minutes. Instantaneous arithmetic reports two
//! hours remaining while a video decodes and six while it does not; the smoothed
//! figure is the one a person can act on.

use std::path::Path;

use zbus::blocking::{Connection, Proxy};

use crate::telemetry::TimeEstimates;

const SERVICE: &str = "org.freedesktop.UPower";
const INTERFACE: &str = "org.freedesktop.UPower.Device";

/// A connection to UPower's view of one battery.
pub struct UPower {
    device: Option<Proxy<'static>>,
}

impl std::fmt::Debug for UPower {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UPower")
            .field("connected", &self.device.is_some())
            .finish()
    }
}

impl UPower {
    /// Connect to the system bus and bind to the device for this battery.
    ///
    /// Never fails. UPower is optional — plenty of systems do not run it, and a
    /// container or a minimal install may have no system bus at all — and a
    /// flight recorder must not decline to record because an optional
    /// convenience is missing. A failed connection degrades to no estimates,
    /// and the sampler falls back to arithmetic.
    #[must_use]
    pub fn connect(battery_dir: &Path) -> Self {
        let path = device_path(battery_dir);

        let device = match Connection::system() {
            Ok(connection) => match Proxy::new(&connection, SERVICE, path.clone(), INTERFACE) {
                Ok(proxy) => {
                    tracing::debug!(device = %path, "bound to UPower device");
                    Some(proxy)
                }
                Err(error) => {
                    tracing::warn!(%error, device = %path, "no UPower device; using arithmetic estimates");
                    None
                }
            },
            Err(error) => {
                tracing::warn!(%error, "no system bus; using arithmetic estimates");
                None
            }
        };

        Self { device }
    }

    /// Read one integer property, treating anything non-positive as absent.
    ///
    /// UPower reports zero for "unknown" as well as for "already there", and
    /// neither is a runtime worth recording. The original applied the same rule
    /// by parsing busctl's output and rejecting non-positive values.
    fn positive_property(&self, name: &str) -> Option<i64> {
        let device = self.device.as_ref()?;
        match device.get_property::<i64>(name) {
            Ok(seconds) if seconds > 0 => Some(seconds),
            Ok(_) => None,
            Err(error) => {
                tracing::debug!(%error, property = name, "UPower property unavailable");
                None
            }
        }
    }
}

impl TimeEstimates for UPower {
    fn time_to_empty_s(&self) -> Option<i64> {
        self.positive_property("TimeToEmpty")
    }

    fn time_to_full_s(&self) -> Option<i64> {
        self.positive_property("TimeToFull")
    }
}

/// The D-Bus object path UPower publishes for a `power_supply` device.
///
/// Mirrors `up_device_compute_object_path` in UPower's own `up-device.c`: the
/// sysfs directory name is prefixed with `battery_`, and characters that are not
/// legal in a D-Bus path become underscores. Apple Silicon's `macsmc-battery`
/// is the case that makes this more than string concatenation.
#[must_use]
pub fn device_path(battery_dir: &Path) -> String {
    let name = battery_dir
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();

    let normalised: String = name
        .chars()
        .map(|character| match character {
            '-' | '.' | ':' | '@' => '_',
            other => other,
        })
        .collect();

    format!("/org/freedesktop/UPower/devices/battery_{normalised}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_paths_for_conventionally_named_batteries() {
        assert_eq!(
            device_path(Path::new("/sys/class/power_supply/BAT0")),
            "/org/freedesktop/UPower/devices/battery_BAT0"
        );
        assert_eq!(
            device_path(Path::new("/sys/class/power_supply/BAT1")),
            "/org/freedesktop/UPower/devices/battery_BAT1"
        );
    }

    #[test]
    fn normalises_hyphens_as_upower_does() {
        // Apple Silicon under Linux, and the reason this is not concatenation.
        assert_eq!(
            device_path(Path::new("/sys/class/power_supply/macsmc-battery")),
            "/org/freedesktop/UPower/devices/battery_macsmc_battery"
        );
    }

    #[test]
    fn normalises_every_character_illegal_in_a_dbus_path() {
        assert_eq!(
            device_path(Path::new("/sys/class/power_supply/bat.0@aux:1")),
            "/org/freedesktop/UPower/devices/battery_bat_0_aux_1"
        );
    }

    #[test]
    fn a_trailing_separator_does_not_produce_an_empty_name() {
        assert_eq!(
            device_path(Path::new("/sys/class/power_supply/BAT0/")),
            "/org/freedesktop/UPower/devices/battery_BAT0"
        );
    }

    #[test]
    fn connecting_without_upower_degrades_instead_of_failing() {
        // The property reads must return None rather than panicking, whatever
        // the bus situation on the machine running the tests.
        let upower = UPower::connect(Path::new("/sys/class/power_supply/__batmon_absent__"));

        assert_eq!(upower.time_to_empty_s(), None);
        assert_eq!(upower.time_to_full_s(), None);
    }
}
