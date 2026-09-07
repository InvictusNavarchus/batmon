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
