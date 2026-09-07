//! Physical units and the conversions the kernel ABIs require.
//!
//! Only temperature gets a newtype. It is the one quantity this daemon reads
//! through two different encodings — ACPI `power_supply` reports tenths of a
//! degree, the hwmon ABI reports millidegrees — and the one where mixing them up
//! yields a number that looks entirely reasonable. Everything else (watts,
//! watt-hours, volts) arrives in exactly one encoding and is used in exactly one
//! expression, so wrapping it would add ceremony without removing a failure mode.

use crate::formats::round_to;

/// SI micro- divisor.
///
/// `power_supply` reports energy in µWh, power in µW, voltage in µV and charge
/// in µAh. All four convert identically, so the magic number is named once here
/// rather than repeated as `1_000_000` at a dozen call sites.
pub const MICRO: f64 = 1_000_000.0;

/// A temperature in degrees Celsius that has passed a plausibility check.
///
/// Constructing one is the only way to get a temperature into a sample, which
/// means the "is this reading real?" question is answered exactly once.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Celsius(f64);

impl Celsius {
    /// Coldest reading treated as a real measurement.
    ///
    /// Linux thermal drivers signal failure in-band. A sensor that is absent,
    /// disconnected or erroring reports a negative errno (`-EINVAL` is -22,
    /// `-ENODATA` is -61) or an uninitialised absolute-zero placeholder
    /// (-273.15 °C). Anything below -50 °C is one of those, not a laptop.
    pub const MIN_PLAUSIBLE: Self = Self(-50.0);

    /// From the hwmon ABI's millidegrees (`/sys/class/hwmon/*/tempN_input`).
    #[must_use]
    pub fn from_millidegrees(raw: f64) -> Option<Self> {
        Self::plausible(raw / 1_000.0)
    }

    /// From ACPI `power_supply`'s tenths of a degree (`.../BAT0/temp`).
    #[must_use]
    pub fn from_tenths(raw: f64) -> Option<Self> {
        Self::plausible(raw / 10.0)
    }

    fn plausible(degrees: f64) -> Option<Self> {
        // Finiteness is checked explicitly rather than left to the comparison.
        // NaN would fail it anyway, since every comparison against NaN is
        // false, but positive infinity would not — and an infinite temperature
        // is a broken sensor, not a hot one.
        let value = Self(degrees);
        (degrees.is_finite() && value >= Self::MIN_PLAUSIBLE).then_some(value)
    }

    /// Rounded to one decimal place with JavaScript tie semantics.
    ///
    /// Deliberately *not* folded into the constructors: the TypeScript daemon
    /// rounds system temperatures but stores battery temperature unrounded, and
    /// the port reproduces that asymmetry rather than quietly correcting it.
    /// Normalising the two is a behaviour change and belongs in its own commit.
    #[must_use]
    pub fn rounded_tenth(self) -> Self {
        Self(round_to(self.0, 1))
    }

    /// The underlying value in degrees Celsius.
    #[must_use]
    pub fn get(self) -> f64 {
        self.0
    }
}

/// Constrain a computed percentage to 0..=100.
///
/// Replaces the `Math.max(0, Math.min(100, x))` sandwich repeated at three call
/// sites. `f64::clamp` propagates NaN exactly as the JavaScript pair did.
#[must_use]
pub fn clamp_percent(value: f64) -> f64 {
    value.clamp(0.0, 100.0)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::float_cmp)]

    use super::*;

    #[test]
    fn millidegrees_convert_and_accept_the_plausible_range() {
        assert_eq!(Celsius::from_millidegrees(84_450.0).unwrap().get(), 84.45);
        assert_eq!(Celsius::from_millidegrees(0.0).unwrap().get(), 0.0);
        assert_eq!(Celsius::from_millidegrees(-50_000.0).unwrap().get(), -50.0);
    }

    #[test]
    fn millidegrees_reject_kernel_error_sentinels() {
        // -EINVAL and -ENODATA are millidegree values of -22 and -61 only if you
        // forget the driver is reporting an errno, so the floor is expressed in
        // degrees and catches the absolute-zero placeholder instead.
        assert_eq!(Celsius::from_millidegrees(-273_150.0), None);
        assert_eq!(Celsius::from_millidegrees(-50_001.0), None);
    }

    #[test]
    fn non_finite_readings_are_not_temperatures() {
        // Reachable only through the public constructors, since js_number
        // filters these out of sysfs — but an infinite reading would otherwise
        // pass the floor and reach a sample and an alert threshold.
        for raw in [f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
            assert_eq!(Celsius::from_millidegrees(raw), None, "millidegrees {raw}");
            assert_eq!(Celsius::from_tenths(raw), None, "tenths {raw}");
        }
    }

    #[test]
    fn tenths_convert_and_share_the_same_floor() {
        assert_eq!(Celsius::from_tenths(305.0).unwrap().get(), 30.5);
        assert_eq!(Celsius::from_tenths(-500.0).unwrap().get(), -50.0);
        assert_eq!(Celsius::from_tenths(-501.0), None);
        assert_eq!(Celsius::from_tenths(-2731.0), None);
    }

    #[test]
    fn both_encodings_agree_on_the_floor() {
        // The two constants the TypeScript config carried (-50000 millidegrees
        // and -500 tenths) were the same physical bound written twice.
        assert_eq!(
            Celsius::from_millidegrees(-50_000.0).unwrap(),
            Celsius::from_tenths(-500.0).unwrap()
        );
    }

    #[test]
    fn rounded_tenth_uses_javascript_tie_semantics() {
        assert_eq!(
            Celsius::from_millidegrees(84_450.0)
                .unwrap()
                .rounded_tenth()
                .get(),
            84.5
        );
        assert_eq!(
            Celsius::from_millidegrees(-3_150.0)
                .unwrap()
                .rounded_tenth()
                .get(),
            -3.1
        );
    }

    #[test]
    fn celsius_orders_by_temperature() {
        let warm = Celsius::from_millidegrees(45_000.0).unwrap();
        let hot = Celsius::from_millidegrees(50_000.0).unwrap();
        assert!(hot > warm);
        assert!(warm > Celsius::MIN_PLAUSIBLE);
    }

    #[test]
    fn clamp_percent_bounds_and_propagates_nan() {
        assert_eq!(clamp_percent(-3.0), 0.0);
        assert_eq!(clamp_percent(0.0), 0.0);
        assert_eq!(clamp_percent(42.5), 42.5);
        assert_eq!(clamp_percent(100.0), 100.0);
        assert_eq!(clamp_percent(140.0), 100.0);
        assert!(clamp_percent(f64::NAN).is_nan());
    }

    #[test]
    fn micro_divisor_converts_every_power_supply_unit() {
        assert_eq!(48_600_000.0 / MICRO, 48.6); // µWh -> Wh
        assert_eq!(11_250_000.0 / MICRO, 11.25); // µW  -> W
        assert_eq!(15_400_000.0 / MICRO, 15.4); // µV  -> V
    }
}
