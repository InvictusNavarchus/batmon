//! Software cycle counting by energy throughput.
//!
//! Most battery management systems either do not report a cycle count or report
//! one that only moves in whole numbers, long after the wear has happened. This
//! integrates the real thing: every watt-hour that leaves the pack while
//! discharging is a fraction of a design capacity, and those fractions accumulate.

use crate::types::{PowerState, Sample};

/// Cycle count for `curr`, carried forward from `prev`.
///
/// Pure and total: no I/O, no clock, no state. The result is always the previous
/// count plus a non-negative increment, so the series can only ever rise.
///
/// Three guards keep the integral honest, and each exists because of a real way
/// the number would otherwise drift upward:
///
/// - **Only while discharging.** Energy readings dip while plugged in — float
///   charge cycles, a vendor charge limit releasing, plain sensor noise. Counting
///   those would accrue wear the battery never suffered.
/// - **Never across a reboot.** A machine powered off for a week comes back with
///   less energy than it had, and none of that loss went through a load. A
///   changed `boot_id`, or an uptime that went backwards, means the gap is not
///   ours to integrate.
/// - **Never more than one cycle in a tick.** A delta larger than the entire
///   design capacity is a driver glitch or a battery swap, not a discharge.
///
/// Returns `0.0` when there is no previous sample or no usable design capacity,
/// which is what a fresh database looks like.
#[must_use]
pub fn compute_estimated_cycles(curr: &Sample, prev: Option<&Sample>) -> f64 {
    let Some(prev) = prev else {
        return 0.0;
    };
    if curr.energy_design_wh <= 0.0 {
        return 0.0;
    }

    let carried = prev.estimated_cycle_count;

    if crossed_boot_boundary(curr, prev) {
        return carried;
    }

    if curr.power_state != PowerState::Discharging {
        return carried;
    }

    let delta_wh = prev.energy_wh - curr.energy_wh;
    if delta_wh > 0.0 && delta_wh <= curr.energy_design_wh {
        return carried + delta_wh / curr.energy_design_wh;
    }

    carried
}

/// Whether the machine rebooted between the two samples.
///
/// Two independent signals, either of which is sufficient. `boot_id` is
/// authoritative but absent on kernels that do not expose it; a decreasing
/// uptime catches the same event without it. Both are skipped when either side
/// is missing rather than guessed at, because a false positive here silently
/// discards real discharge.
fn crossed_boot_boundary(curr: &Sample, prev: &Sample) -> bool {
    let boot_id_changed = match (&prev.boot_id, &curr.boot_id) {
        (Some(before), Some(after)) => before != after,
        _ => false,
    };

    let uptime_went_backwards = match (prev.uptime_s, curr.uptime_s) {
        (Some(before), Some(after)) => after < before,
        _ => false,
    };

    boot_id_changed || uptime_went_backwards
}

#[cfg(test)]
mod tests {
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
    fn an_unusable_design_capacity_yields_zero() {
        let prev = discharging(45.0, 2.0);

        for design in [0.0, -10.0] {
            let curr = Sample {
                energy_wh: 40.0,
                energy_design_wh: design,
                ..discharging(40.0, 0.0)
            };
            assert_eq!(compute_estimated_cycles(&curr, Some(&prev)), 0.0);
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
}

#[cfg(test)]
mod properties {
    //! Invariants the integral must hold for every pair of samples.
    //!
    //! A cycle count is a number people read to decide whether to replace a
    //! battery, and it is accumulated one tick at a time over months. An error
    //! here does not announce itself — it just quietly produces a wrong number,
    //! forever. These state the shape the series must always have.

    // Exact equality is the assertion in the carry-forward properties: the
    // count must be passed through untouched, not merely close to it.
    #![allow(clippy::float_cmp)]

    use proptest::prelude::*;

    use super::compute_estimated_cycles;
    use crate::types::{PowerState, Sample};

    fn arb_power_state() -> impl Strategy<Value = PowerState> {
        prop_oneof![
            Just(PowerState::Charging),
            Just(PowerState::Discharging),
            Just(PowerState::AcIdle),
            Just(PowerState::Unknown),
        ]
    }

    /// A pair of consecutive samples with a usable design capacity.
    fn arb_pair() -> impl Strategy<Value = (Sample, Sample)> {
        (
            0.1f64..200.0,   // design capacity
            0.0f64..200.0,   // previous energy
            0.0f64..200.0,   // current energy
            0.0f64..1_000.0, // cycles carried in
            arb_power_state(),
            prop::option::of(0.0f64..1_000_000.0), // previous uptime
            prop::option::of(0.0f64..1_000_000.0), // current uptime
            prop::option::of("[a-f0-9]{8}"),       // previous boot id
            prop::option::of("[a-f0-9]{8}"),       // current boot id
        )
            .prop_map(
                |(
                    design,
                    previous_energy,
                    current_energy,
                    carried,
                    power_state,
                    previous_uptime,
                    current_uptime,
                    previous_boot,
                    current_boot,
                )| {
                    let previous = Sample {
                        energy_wh: previous_energy,
                        energy_design_wh: design,
                        estimated_cycle_count: carried,
                        uptime_s: previous_uptime,
                        boot_id: previous_boot,
                        ..Sample::default()
                    };
                    let current = Sample {
                        energy_wh: current_energy,
                        energy_design_wh: design,
                        power_state,
                        is_charging: power_state == PowerState::Charging,
                        uptime_s: current_uptime,
                        boot_id: current_boot,
                        ..Sample::default()
                    };
                    (previous, current)
                },
            )
    }

    proptest! {
        /// The series only ever rises. A count that can fall is not a count.
        #[test]
        fn the_cycle_count_never_decreases((previous, current) in arb_pair()) {
            let result = compute_estimated_cycles(&current, Some(&previous));
            prop_assert!(
                result >= previous.estimated_cycle_count,
                "{result} < {}",
                previous.estimated_cycle_count
            );
        }

        /// One tick can accrue at most one full cycle, whatever the sensors say.
        /// Larger deltas are driver glitches or battery swaps, not discharge.
        #[test]
        fn a_single_tick_accrues_at_most_one_cycle((previous, current) in arb_pair()) {
            let result = compute_estimated_cycles(&current, Some(&previous));
            let accrued = result - previous.estimated_cycle_count;
            prop_assert!(accrued <= 1.0, "accrued {accrued} in one tick");
        }

        /// The result is always a usable number for usable inputs.
        #[test]
        fn the_result_is_always_finite((previous, current) in arb_pair()) {
            let result = compute_estimated_cycles(&current, Some(&previous));
            prop_assert!(result.is_finite(), "{result}");
        }

        /// Energy lost while the machine was off went through no load, so a
        /// reboot must carry the count forward untouched — never approximately.
        #[test]
        fn a_reboot_carries_the_count_forward_exactly(
            (previous, current) in arb_pair(),
            uptime_before in 100.0f64..1_000_000.0,
            uptime_after in 0.0f64..99.0,
        ) {
            let previous = Sample {
                boot_id: Some("boot-before".to_owned()),
                uptime_s: Some(uptime_before),
                ..previous
            };
            let current = Sample {
                boot_id: Some("boot-after".to_owned()),
                uptime_s: Some(uptime_after),
                power_state: PowerState::Discharging,
                is_charging: false,
                ..current
            };

            prop_assert_eq!(
                compute_estimated_cycles(&current, Some(&previous)),
                previous.estimated_cycle_count
            );
        }

        /// Only discharging wears a battery. Every other rail state carries the
        /// count forward exactly, whatever the energy readings did.
        #[test]
        fn nothing_but_discharging_accrues_wear((previous, current) in arb_pair()) {
            prop_assume!(current.power_state != PowerState::Discharging);

            prop_assert_eq!(
                compute_estimated_cycles(&current, Some(&previous)),
                previous.estimated_cycle_count
            );
        }

        /// A first sample has nothing to carry.
        #[test]
        fn no_history_means_no_cycles((_previous, current) in arb_pair()) {
            prop_assert_eq!(compute_estimated_cycles(&current, None), 0.0);
        }

        /// Documents a quirk carried over deliberately rather than an invariant.
        ///
        /// When the design capacity reads as zero — a transient sysfs failure
        /// makes it zero rather than absent — the result is 0.0 rather than the
        /// carried count, so the accumulated history is discarded. This matches
        /// the TypeScript exactly and is reproduced for parity; whether it
        /// should keep doing that is a separate question from whether the port
        /// is faithful.
        #[test]
        fn an_unusable_design_capacity_discards_the_carried_count(
            (previous, current) in arb_pair(),
        ) {
            let current = Sample {
                energy_design_wh: 0.0,
                ..current
            };

            prop_assert_eq!(compute_estimated_cycles(&current, Some(&previous)), 0.0);
        }
    }
}
