#![allow(clippy::float_cmp)]

use super::*;

/// A discharging sample with a 50 Wh design capacity and a continuous boot.
fn discharging(energy_wh: f64, cycles: f64) -> Sample {
    Sample {
        energy_wh,
        energy_design_wh: 50.0,
        estimated_cycle_count: cycles,
        status: "Discharging".to_owned(),
        power_state: PowerState::Discharging,
        boot_id: Some("boot-uuid-1".to_owned()),
        uptime_s: Some(1_000.0),
        ..Sample::default()
    }
}

#[test]
fn a_first_sample_starts_at_zero() {
    assert_eq!(compute_estimated_cycles(&discharging(45.0, 0.0), None), 0.0);
}

#[test]
fn an_unusable_design_capacity_keeps_the_carried_count() {
    let prev = discharging(45.0, 2.0);

    for design in [0.0, -10.0] {
        let curr = Sample {
            energy_wh: 40.0,
            energy_design_wh: design,
            ..discharging(40.0, 0.0)
        };
        // 2.0, not 0.0: the increment is unknown, the history is not.
        assert_eq!(compute_estimated_cycles(&curr, Some(&prev)), 2.0);
    }
}

#[test]
fn charging_never_accrues_wear() {
    let prev = Sample {
        estimated_cycle_count: 3.5,
        ..discharging(40.0, 3.5)
    };
    let curr = Sample {
        status: "Charging".to_owned(),
        power_state: PowerState::Charging,
        ..discharging(35.0, 0.0)
    };

    assert_eq!(compute_estimated_cycles(&curr, Some(&prev)), 3.5);
}

#[test]
fn a_float_charge_dip_on_ac_never_accrues_wear() {
    // Plugged in and held at a charge limit, the reading drifts down. That
    // energy never went through a load.
    let prev = Sample {
        status: "Full".to_owned(),
        power_state: PowerState::AcIdle,
        ..discharging(50.0, 1.0)
    };
    let curr = Sample {
        status: "Full".to_owned(),
        power_state: PowerState::AcIdle,
        ..discharging(49.9, 0.0)
    };

    assert_eq!(compute_estimated_cycles(&curr, Some(&prev)), 1.0);
}

#[test]
fn discharging_accrues_the_fraction_of_design_capacity_drawn() {
    let prev = discharging(50.0, 1.0);
    let curr = discharging(45.0, 0.0);

    // 5 Wh out of a 50 Wh design capacity is a tenth of a cycle.
    assert_eq!(compute_estimated_cycles(&curr, Some(&prev)), 1.1);
}

#[test]
fn a_full_design_capacity_drawn_is_exactly_one_cycle() {
    let prev = discharging(50.0, 2.0);
    let curr = discharging(0.0, 0.0);

    assert_eq!(compute_estimated_cycles(&curr, Some(&prev)), 3.0);
}

#[test]
fn a_delta_larger_than_the_pack_is_rejected_as_a_glitch() {
    let prev = discharging(100.0, 2.5);
    let curr = discharging(10.0, 0.0); // 90 Wh from a 50 Wh pack

    assert_eq!(compute_estimated_cycles(&curr, Some(&prev)), 2.5);
}

#[test]
fn full_double_precision_survives_the_integration() {
    let prev = discharging(50.0, 1.0);
    let curr = discharging(49.333_333, 0.0);

    let result = compute_estimated_cycles(&curr, Some(&prev));
    assert!(
        (result - 1.013_333_34).abs() < 1e-6,
        "{result} lost precision"
    );
}

#[test]
fn sub_milliwatt_hour_increments_accumulate_instead_of_rounding_away() {
    // The failure this guards: truncating each tick's contribution makes a
    // slow discharge register as no discharge at all, forever.
    let mut cycles = 5.0;
    let mut energy = 50.0;
    let design_wh = 60.0;
    // Discharging at 8 W, so one second moves 8/3600 Wh — well below a
    // milliwatt-hour, and far below anything a rounded column would keep.
    let delta_per_second = 8.0 / 3600.0;

    for _ in 0..100 {
        let prev = Sample {
            energy_design_wh: design_wh,
            ..discharging(energy, cycles)
        };
        energy -= delta_per_second;
        let curr = Sample {
            energy_design_wh: design_wh,
            ..discharging(energy, 0.0)
        };
        cycles = compute_estimated_cycles(&curr, Some(&prev));
    }

    // 100 * (8 / 3600) / 60 = 0.0037037... cycles
    assert!(cycles > 5.0035, "{cycles}");
    assert!(cycles < 5.004, "{cycles}");
}

#[test]
fn a_changed_boot_id_carries_forward_without_integrating_the_gap() {
    let prev = Sample {
        boot_id: Some("old-boot-uuid".to_owned()),
        uptime_s: Some(36_000.0),
        ..discharging(50.0, 5.0)
    };
    // 10 Wh lower after sitting powered off. None of it went through a load.
    let curr = Sample {
        boot_id: Some("new-boot-uuid".to_owned()),
        uptime_s: Some(15.0),
        ..discharging(40.0, 0.0)
    };

    assert_eq!(compute_estimated_cycles(&curr, Some(&prev)), 5.0);
}

#[test]
fn a_decreasing_uptime_carries_forward_even_when_boot_id_matches() {
    // Covers kernels that do not expose boot_id, where uptime is the only
    // reboot signal available.
    let prev = Sample {
        uptime_s: Some(50_000.0),
        ..discharging(50.0, 5.0)
    };
    let curr = Sample {
        uptime_s: Some(30.0),
        ..discharging(40.0, 0.0)
    };

    assert_eq!(compute_estimated_cycles(&curr, Some(&prev)), 5.0);
}

#[test]
fn a_continuous_boot_integrates_normally() {
    let prev = Sample {
        uptime_s: Some(1_000.0),
        ..discharging(50.0, 5.0)
    };
    let curr = Sample {
        uptime_s: Some(1_001.0),
        ..discharging(45.0, 0.0)
    };

    assert_eq!(compute_estimated_cycles(&curr, Some(&prev)), 5.1);
}

#[test]
fn a_missing_boot_signal_on_either_side_is_not_treated_as_a_reboot() {
    // Absent data must not silently discard real discharge.
    let prev = Sample {
        boot_id: None,
        uptime_s: None,
        ..discharging(50.0, 5.0)
    };
    let curr = discharging(45.0, 0.0);

    assert_eq!(compute_estimated_cycles(&curr, Some(&prev)), 5.1);
}

#[test]
fn an_unknown_power_state_never_accrues_wear() {
    let prev = discharging(50.0, 5.0);
    let curr = Sample {
        status: "Unknown".to_owned(),
        power_state: PowerState::Unknown,
        ..discharging(45.0, 0.0)
    };

    assert_eq!(compute_estimated_cycles(&curr, Some(&prev)), 5.0);
}

#[test]
fn energy_rising_while_discharging_is_ignored_rather_than_subtracted() {
    // The count must never decrease, whatever the sensor says.
    let prev = discharging(40.0, 5.0);
    let curr = discharging(45.0, 0.0);

    assert_eq!(compute_estimated_cycles(&curr, Some(&prev)), 5.0);
}
