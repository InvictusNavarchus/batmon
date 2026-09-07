//! The vocabulary shared by every module: what a power rail can be doing, and
//! what one tick of telemetry looks like.

/// What the battery rail is doing, derived from the kernel's `status` string.
///
/// The kernel reports a handful of driver-specific strings; this collapses them
/// into the four states the alert engine and the cycle integrator reason about.
/// `AcIdle` is the important one — a machine plugged in at a charge limit is
/// neither charging nor discharging, and prompting it to "plug in charger" would
/// be nonsense.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PowerState {
    Charging,
    Discharging,
    /// On mains but not moving energy: `Full`, or held at a vendor charge limit.
    AcIdle,
    #[default]
    Unknown,
}

impl PowerState {
    /// Classify a raw `/sys/class/power_supply/*/status` value.
    ///
    /// Matching is exact and case-sensitive, mirroring the kernel's own spelling
    /// in `power_supply_sysfs.c`. Anything unrecognised is [`PowerState::Unknown`]
    /// rather than a guess, so a new driver string shows up in the data as a gap
    /// instead of a plausible lie.
    #[must_use]
    pub fn from_status(status: &str) -> Self {
        match status {
            "Charging" => Self::Charging,
            "Discharging" => Self::Discharging,
            "Full" | "Not charging" => Self::AcIdle,
            _ => Self::Unknown,
        }
    }

    /// The form persisted in the `power_state` column.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Charging => "charging",
            Self::Discharging => "discharging",
            Self::AcIdle => "ac_idle",
            Self::Unknown => "unknown",
        }
    }

    /// Parse a value read back out of the `power_state` column.
    ///
    /// Returns [`None`] for anything unrecognised so the caller can fall back to
    /// re-deriving from `status`, which is what rows written before schema
    /// version 6 require — they predate the column entirely.
    #[must_use]
    pub fn parse_stored(value: &str) -> Option<Self> {
        match value {
            "charging" => Some(Self::Charging),
            "discharging" => Some(Self::Discharging),
            "ac_idle" => Some(Self::AcIdle),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }

    /// Whether this state means energy is flowing into the battery.
    #[must_use]
    pub fn is_charging(self) -> bool {
        matches!(self, Self::Charging)
    }
}

/// One tick of telemetry: a flat mirror of the `samples` table.
///
/// The shape is deliberately schema-shaped rather than domain-shaped. It is
/// written to two databases and read back by third-party SQL, so grouping fields
/// into nested structs would buy tidiness in Rust at the cost of a translation
/// layer nobody asked for. `Option` marks exactly the columns that are nullable
/// because the hardware may not expose them.
///
/// [`Default`] exists to serve as the test fixture: `Sample { charge_pct: 80.0,
/// ..Default::default() }` replaces the `createMockSample` helper the TypeScript
/// tests carried, without a fixture module that production code can reach.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Sample {
    /// UTC timestamp, `toISOString` shape. See [`crate::formats::iso8601_millis`].
    pub ts: String,
    /// State of charge, percent.
    pub charge_pct: f64,
    /// Raw kernel `status` string, preserved verbatim alongside `power_state`.
    pub status: String,
    pub power_state: PowerState,
    pub energy_wh: f64,
    pub energy_full_wh: f64,
    pub energy_design_wh: f64,
    pub power_w: f64,
    pub voltage_v: f64,
    pub voltage_design_v: f64,
    /// Cycle count as reported by the battery management system, if it reports one.
    pub cycle_count: Option<i64>,
    /// Cycle count integrated from observed energy throughput.
    pub estimated_cycle_count: f64,
    /// Battery pack temperature. Absent on many laptops.
    pub battery_temp_c: Option<f64>,
    /// Full-charge capacity as a percentage of design capacity.
    pub health_pct: f64,
    pub is_charging: bool,
    /// Whether a battery is physically present. A false value suppresses all
    /// recording and alerting for the tick.
    pub is_present: bool,
    pub time_to_empty_s: Option<i64>,
    pub time_to_full_s: Option<i64>,
    pub cpu_temp_c: Option<f64>,
    pub gpu_temp_c: Option<f64>,
    pub nvme_temp_c: Option<f64>,
    pub cpu_pct: Option<f64>,
    pub mem_pct: Option<f64>,
    /// Top five process groups by CPU delta, as a JSON array.
    pub top_processes: Option<String>,
    pub cpu_freq_mhz: Option<f64>,
    pub gpu_pct: Option<f64>,
    pub gpu_power_w: Option<f64>,
    pub load1: Option<f64>,
    /// Kernel boot session UUID, used to detect reboots between samples.
    pub boot_id: Option<String>,
    /// Monotonic uptime, used to detect reboots when `boot_id` is unavailable.
    pub uptime_s: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_kernel_status_strings_to_power_states() {
        assert_eq!(PowerState::from_status("Charging"), PowerState::Charging);
        assert_eq!(
            PowerState::from_status("Discharging"),
            PowerState::Discharging
        );
        assert_eq!(PowerState::from_status("Full"), PowerState::AcIdle);
        assert_eq!(PowerState::from_status("Not charging"), PowerState::AcIdle);
    }

    #[test]
    fn treats_unrecognised_status_strings_as_unknown() {
        assert_eq!(PowerState::from_status("Unknown"), PowerState::Unknown);
        assert_eq!(PowerState::from_status(""), PowerState::Unknown);
        assert_eq!(PowerState::from_status("charging"), PowerState::Unknown);
        assert_eq!(PowerState::from_status("Full "), PowerState::Unknown);
    }

    #[test]
    fn stored_representation_round_trips() {
        for state in [
            PowerState::Charging,
            PowerState::Discharging,
            PowerState::AcIdle,
            PowerState::Unknown,
        ] {
            assert_eq!(PowerState::parse_stored(state.as_str()), Some(state));
        }
    }

    #[test]
    fn rejects_unknown_stored_values_so_callers_can_re_derive() {
        // Rows written before schema version 6 have no power_state at all; the
        // caller re-derives from `status` when this returns None.
        assert_eq!(PowerState::parse_stored(""), None);
        assert_eq!(PowerState::parse_stored("Charging"), None);
        assert_eq!(PowerState::parse_stored("idle"), None);
    }

    #[test]
    fn only_charging_counts_as_charging() {
        assert!(PowerState::Charging.is_charging());
        assert!(!PowerState::AcIdle.is_charging());
        assert!(!PowerState::Discharging.is_charging());
        assert!(!PowerState::Unknown.is_charging());
    }

    #[test]
    fn default_sample_is_an_absent_battery_in_an_unknown_state() {
        let sample = Sample::default();
        assert_eq!(sample.power_state, PowerState::Unknown);
        assert!(!sample.is_present);
        assert!(!sample.is_charging);
        assert_eq!(sample.battery_temp_c, None);
    }
}
