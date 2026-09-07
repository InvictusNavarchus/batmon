// Bit-exact float comparison is the entire point of this module: these
// assertions exist to catch a one-ulp drift in stored values.
#![allow(clippy::float_cmp)]

use super::*;

/// Expectations are transcribed from Bun's own output, not derived by hand.
#[test]
fn round_half_up_takes_ties_toward_positive_infinity() {
    assert_eq!(round_half_up(0.5), 1.0);
    assert_eq!(round_half_up(-0.5), 0.0);
    assert_eq!(round_half_up(1.5), 2.0);
    assert_eq!(round_half_up(-1.5), -1.0);
    assert_eq!(round_half_up(2.5), 3.0);
    assert_eq!(round_half_up(-2.5), -2.0);
    assert_eq!(round_half_up(0.499_999_999_999_999_94), 0.0);
    assert_eq!(round_half_up(-0.499_999_999_999_999_94), 0.0);
    assert_eq!(round_half_up(84.45), 84.0);
    assert_eq!(round_half_up(-0.049), 0.0);
    assert_eq!(round_half_up(0.0), 0.0);
}

#[test]
fn round_half_up_diverges_from_rust_round_exactly_on_negative_ties() {
    // The whole reason this module exists. If these ever agree, the helper
    // has stopped doing its job.
    assert_ne!(round_half_up(-0.5), (-0.5f64).round());
    assert_ne!(round_half_up(-1.5), (-1.5f64).round());
    // ...and agrees everywhere else.
    assert_eq!(round_half_up(0.5), 0.5f64.round());
    assert_eq!(round_half_up(2.5), 2.5f64.round());
}

#[test]
fn round_half_up_passes_non_finite_values_through() {
    assert!(round_half_up(f64::NAN).is_nan());
    assert_eq!(round_half_up(f64::INFINITY), f64::INFINITY);
    assert_eq!(round_half_up(f64::NEG_INFINITY), f64::NEG_INFINITY);
}

#[test]
fn round_to_one_decimal_keeps_the_scaling_error() {
    assert_eq!(round_to(45.25, 1), 45.3);
    assert_eq!(round_to(-0.05, 1), 0.0);
    assert_eq!(round_to(84.449_999, 1), 84.4);
    assert_eq!(round_to(0.049_99, 1), 0.0);
    assert_eq!(round_to(-3.15, 1), -3.1);
    assert_eq!(round_to(62.35, 1), 62.4);
    assert_eq!(round_to(-50.0, 1), -50.0);
    assert_eq!(round_to(0.0, 1), 0.0);
}

#[test]
fn round_to_reproduces_the_hwmon_millidegree_conversion() {
    // Math.round((raw / 1000) * 10) / 10 for real sensor readings.
    for (raw, expected) in [
        (84_450i64, 84.5),
        (-50_000, -50.0),
        (0, 0.0),
        (62_350, 62.4),
        (-3_150, -3.1),
        (45_250, 45.3),
    ] {
        assert_eq!(round_to(raw as f64 / 1000.0, 1), expected, "raw={raw}");
    }
}

#[test]
fn round_half_up_reproduces_the_health_percentage_expression() {
    // health_pct is Math.round(ratio * 10000) / 100 — note the asymmetric
    // scale factors, which is why it cannot be expressed as round_to.
    for (full, design, expected) in [
        (48.6, 53.0, 91.7),
        (53.0, 53.0, 100.0),
        (41.333, 53.0, 77.99),
        (0.1, 3.0, 3.33),
    ] {
        assert_eq!(round_half_up((full / design) * 10_000.0) / 100.0, expected);
    }
}

#[test]
fn parse_number_accepts_the_values_the_kernel_emits() {
    assert_eq!(parse_number("42"), Some(42.0));
    assert_eq!(parse_number("  42  "), Some(42.0));
    assert_eq!(parse_number("-22"), Some(-22.0));
    assert_eq!(parse_number("-273150"), Some(-273_150.0));
    assert_eq!(parse_number("12.75"), Some(12.75));
    assert_eq!(parse_number("+5"), Some(5.0));
    assert_eq!(parse_number("1e3"), Some(1000.0));
    assert_eq!(parse_number(""), Some(0.0));
}

#[test]
fn parse_number_rejects_non_finite_input() {
    assert_eq!(parse_number("abc"), None);
    assert_eq!(parse_number("1_000"), None);
    assert_eq!(parse_number("NaN"), None);
    // Accepted by Rust's parser, rejected by Number(); the finite filter
    // makes both reach the caller's fallback identically.
    assert_eq!(parse_number("inf"), None);
    assert_eq!(parse_number("Infinity"), None);
}

#[test]
fn parse_leading_int_stops_at_the_first_non_digit() {
    assert_eq!(parse_leading_int("  16384000 kB"), Some(16_384_000));
    assert_eq!(parse_leading_int("16384000"), Some(16_384_000));
    assert_eq!(parse_leading_int("-5 x"), Some(-5));
    assert_eq!(parse_leading_int("12.9"), Some(12));
    assert_eq!(parse_leading_int("+7"), Some(7));
    assert_eq!(parse_leading_int("kB 12"), None);
    assert_eq!(parse_leading_int(""), None);
    assert_eq!(parse_leading_int("abc"), None);
    assert_eq!(parse_leading_int("-"), None);
}

#[test]
fn parse_leading_int_returns_none_rather_than_wrapping_on_overflow() {
    assert_eq!(parse_leading_int("99999999999999999999999"), None);
}

#[test]
fn format_decimals_rounds_half_away_from_zero_at_one_decimal() {
    for (value, expected) in [
        (45.25, "45.3"),
        (84.45, "84.5"),
        (0.15, "0.1"),
        (46.05, "46.0"),
        (50.35, "50.4"),
        (32.1, "32.1"),
        (45.2, "45.2"),
        (-3.15, "-3.1"),
        (0.0, "0.0"),
        (100.0, "100.0"),
        (49.95, "50.0"),
        (42.75, "42.8"),
        (91.25, "91.3"),
        (91.75, "91.8"),
    ] {
        assert_eq!(format_decimals(value, 1), expected, "toFixed(1) of {value}");
    }
}

#[test]
fn format_decimals_rounds_half_away_from_zero_at_two_decimals() {
    for (value, expected) in [
        (15.125, "15.13"),
        (12.345, "12.35"),
        (1.005, "1.00"),
        (8.575, "8.57"),
        (15.4, "15.40"),
        (11.55, "11.55"),
        (12.524, "12.52"),
        (0.125, "0.13"),
        (2.675, "2.67"),
        (15.375, "15.38"),
    ] {
        assert_eq!(format_decimals(value, 2), expected, "toFixed(2) of {value}");
    }
}

#[test]
fn format_decimals_rounds_half_away_from_zero_at_zero_decimals() {
    for (value, expected) in [
        (84.5, "85"),
        (85.5, "86"),
        (48.0, "48"),
        (0.5, "1"),
        (-0.5, "-1"),
        (88.6, "89"),
    ] {
        assert_eq!(format_decimals(value, 0), expected, "toFixed(0) of {value}");
    }
}

#[test]
fn format_decimals_keeps_the_sign_of_a_negative_value_that_rounds_to_zero() {
    assert_eq!(format_decimals(-0.04, 1), "-0.0");
    // ...but negative zero is not negative, per the spec's strict comparison.
    assert_eq!(format_decimals(-0.0, 1), "0.0");
}

#[test]
fn format_decimals_propagates_a_carry_across_the_decimal_point() {
    assert_eq!(format_decimals(9.99, 1), "10.0");
    assert_eq!(format_decimals(99.99, 1), "100.0");
    assert_eq!(format_decimals(9.95, 1), "9.9");
    assert_eq!(format_decimals(0.0001, 2), "0.00");
}

#[test]
fn format_decimals_and_round_half_up_disagree_on_negative_halves() {
    // The distinction that makes both helpers necessary: toFixed rounds away
    // from zero, Math.round rounds toward positive infinity.
    assert_eq!(format_decimals(-0.5, 0), "-1");
    assert_eq!(round_half_up(-0.5), 0.0);
}

#[test]
fn format_decimals_beats_the_rust_formatter_on_exact_ties() {
    // Rust's formatter rounds half to even; this one does not. hwmon
    // temperatures land on these ties whenever the millidegree reading
    // ends in 250.
    assert_eq!(format!("{:.1}", 45.25f64), "45.2");
    assert_eq!(format_decimals(45.25, 1), "45.3");
}

#[test]
fn iso8601_millis_always_emits_three_fractional_digits() {
    let whole: Timestamp = "2026-09-06T12:34:56Z".parse().unwrap();
    assert_eq!(iso8601_millis(whole), "2026-09-06T12:34:56.000Z");

    let millis: Timestamp = "2026-09-06T12:34:56.789Z".parse().unwrap();
    assert_eq!(iso8601_millis(millis), "2026-09-06T12:34:56.789Z");

    // Sub-millisecond precision truncates rather than widening the field.
    let micros: Timestamp = "2026-09-06T12:34:56.789123Z".parse().unwrap();
    assert_eq!(iso8601_millis(micros), "2026-09-06T12:34:56.789Z");
}

#[test]
fn iso8601_millis_is_fixed_width_so_string_comparison_orders_correctly() {
    let earlier: Timestamp = "2026-09-06T12:34:56Z".parse().unwrap();
    let later: Timestamp = "2026-09-06T12:34:56.789Z".parse().unwrap();

    // The bug this prevents: jiff's default Display drops trailing zeros,
    // which inverts the lexicographic order the prune query depends on.
    assert!(earlier.to_string() > later.to_string());
    assert!(iso8601_millis(earlier) < iso8601_millis(later));
}

#[test]
fn now_iso8601_millis_has_the_shape_to_iso_string_produces() {
    let now = now_iso8601_millis();
    assert_eq!(now.len(), 24, "{now}");
    assert!(now.ends_with('Z'), "{now}");
    assert_eq!(now.as_bytes()[10], b'T', "{now}");
    assert_eq!(now.as_bytes()[19], b'.', "{now}");
}
