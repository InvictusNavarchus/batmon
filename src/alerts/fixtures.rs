//! Shared sample fixtures for the alert state machine tests.
//!
//! One place to build a plausible sample, so the assertions across the alert
//! families describe the same scenarios.

use crate::types::{PowerState, Sample};

/// A plausible mid-charge sample: 50%, discharging, everything nominal.
pub(crate) fn mock() -> Sample {
    Sample {
        ts: "2026-08-28T00:00:00.000Z".to_owned(),
        charge_pct: 50.0,
        status: "Discharging".to_owned(),
        power_state: PowerState::Discharging,
        energy_wh: 30.0,
        energy_full_wh: 60.0,
        energy_design_wh: 60.0,
        power_w: 10.0,
        voltage_v: 12.0,
        voltage_design_v: 12.0,
        cycle_count: Some(50),
        estimated_cycle_count: 50.0,
        battery_temp_c: Some(30.0),
        health_pct: 95.0,
        is_charging: false,
        is_present: true,
        time_to_empty_s: Some(7_200),
        time_to_full_s: None,
        cpu_temp_c: Some(45.0),
        gpu_temp_c: None,
        nvme_temp_c: None,
        cpu_pct: Some(5.0),
        mem_pct: Some(20.0),
        top_processes: None,
        cpu_freq_mhz: Some(2_400.0),
        gpu_pct: None,
        gpu_power_w: None,
        load1: Some(0.5),
        boot_id: Some("mock-boot-id".to_owned()),
        uptime_s: Some(12_345.6),
    }
}

/// Discharging at `charge_pct`.
pub(crate) fn discharging_at(charge_pct: f64) -> Sample {
    Sample {
        charge_pct,
        status: "Discharging".to_owned(),
        power_state: PowerState::Discharging,
        is_charging: false,
        ..mock()
    }
}

/// Charging at `charge_pct`.
pub(crate) fn charging_at(charge_pct: f64) -> Sample {
    Sample {
        charge_pct,
        status: "Charging".to_owned(),
        power_state: PowerState::Charging,
        is_charging: true,
        ..mock()
    }
}

/// On mains but moving no energy — full, or held at a charge limit.
pub(crate) fn ac_idle_at(charge_pct: f64) -> Sample {
    Sample {
        charge_pct,
        status: "Full".to_owned(),
        power_state: PowerState::AcIdle,
        is_charging: false,
        ..mock()
    }
}

/// A state the daemon cannot classify.
pub(crate) fn unknown_at(charge_pct: f64) -> Sample {
    Sample {
        charge_pct,
        status: "Unknown".to_owned(),
        power_state: PowerState::Unknown,
        is_charging: false,
        ..mock()
    }
}
