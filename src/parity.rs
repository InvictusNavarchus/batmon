//! Exact-semantics helpers for the places JavaScript and Rust quietly disagree.
//!
//! The Rust daemon must produce byte-identical rows to the TypeScript one so the
//! two databases can be diffed column by column during the migration. Three
//! behaviours make that non-trivial. Each is centralised here rather than
//! open-coded at the call sites, because every one of them fails silently: the
//! output is plausible, just wrong, and only a differential diff would catch it.

use jiff::Timestamp;

/// `Math.round` semantics: ties go toward positive infinity.
///
/// Rust's [`f64::round`] rounds half *away from zero*, so the two disagree on
/// every negative tie: `Math.round(-0.5)` is `0` where `(-0.5f64).round()` is
/// `-1.0`. Temperatures are the reason this matters — the hwmon floor is -50 °C,
/// so sub-zero readings are in range and would round the wrong way.
///
/// Implemented as "floor, then step up if the fraction reaches a half" rather
/// than the usual `(x + 0.5).floor()` shortcut, which overshoots for the largest
/// double below one half: `0.499_999_999_999_999_94 + 0.5` rounds up to exactly
/// `1.0` in binary floating point, yielding `1` where JavaScript yields `0`.
#[must_use]
pub fn round_js(x: f64) -> f64 {
    if !x.is_finite() {
        return x;
    }
    let floor = x.floor();
    if x - floor >= 0.5 { floor + 1.0 } else { floor }
}

/// `Math.round(x * 10^digits) / 10^digits`, including the intermediate rounding
/// error of the scaling multiply — which is load-bearing, not incidental.
///
/// `-3.15` is not representable, so `-3.15 * 10.0` is `-31.499999999999996` and
/// the correct answer is `-3.1`, not the `-3.2` you would get from rounding the
/// decimal literal. Reproducing the multiply reproduces the error.
#[must_use]
pub fn round_to(x: f64, digits: i32) -> f64 {
    let scale = 10f64.powi(digits);
    round_js(x * scale) / scale
}

/// `Number(s)` for the subset of inputs the kernel actually emits, returning
/// [`None`] wherever JavaScript would produce a non-finite value and the caller
/// would fall back.
///
/// Rust's parser accepts `inf`, `infinity` and `nan`, which `Number()` rejects;
/// the finite filter collapses both paths to the same outcome, so the divergence
/// is unobservable. The one real difference is hexadecimal: `Number("0x10")` is
/// `16` where this returns [`None`]. No file under `/sys/class/power_supply` or
/// `/sys/class/hwmon` is hex-encoded, so implementing it would be dead code.
#[must_use]
pub fn js_number(s: &str) -> Option<f64> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Some(0.0);
    }
    trimmed.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// `Number.parseInt(s, 10)`: skip leading whitespace, take an optional sign, then
/// consume digits and stop at the first byte that is not one.
///
/// This is what makes `"  16384000 kB"` from `/proc/meminfo` parse without
/// splitting the line first — `str::parse` would reject the trailing unit.
/// Returns [`None`] where JavaScript returns `NaN`, and also on overflow, where
/// JavaScript would silently widen to a float; `/proc/meminfo` values are
/// kilobytes and cannot approach [`i64::MAX`].
#[must_use]
pub fn js_parse_int(s: &str) -> Option<i64> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    let negative = match bytes.get(i) {
        Some(b'-') => {
            i += 1;
            true
        }
        Some(b'+') => {
            i += 1;
            false
        }
        _ => false,
    };

    let first_digit = i;
    let mut acc: i64 = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        acc = acc
            .checked_mul(10)?
            .checked_add(i64::from(bytes[i] - b'0'))?;
        i += 1;
    }
    if i == first_digit {
        return None;
    }

    Some(if negative { -acc } else { acc })
}

/// `Date.prototype.toISOString()`: UTC with exactly three fractional digits.
///
/// The fixed width is a correctness requirement, not cosmetics. Debug rows are
/// pruned with `WHERE ts < cutoff`, which SQLite evaluates as a *string*
/// comparison, and jiff's default `Display` emits only as many fractional digits
/// as the value needs. Mixed widths sort wrongly against each other: `Z` is
/// `0x5A` and `.` is `0x2E`, so a whole-second `…:56Z` sorts *after*
/// `…:56.789Z` from the same second, and prune would spare rows it should drop.
#[must_use]
pub fn iso8601_millis(ts: Timestamp) -> String {
    format!("{ts:.3}")
}

/// Current time in [`iso8601_millis`] form.
#[must_use]
pub fn now_iso8601_millis() -> String {
    iso8601_millis(Timestamp::now())
}

#[cfg(test)]
mod tests {
    // Bit-exact float comparison is the entire point of this module: these
    // assertions exist to catch a one-ulp drift from the TypeScript daemon.
    #![allow(clippy::float_cmp)]

    use super::*;

    /// Expectations are transcribed from Bun's own output, not derived by hand.
    #[test]
    fn round_js_matches_javascript_on_ties_and_the_sub_half_edge_case() {
        assert_eq!(round_js(0.5), 1.0);
        assert_eq!(round_js(-0.5), 0.0);
        assert_eq!(round_js(1.5), 2.0);
        assert_eq!(round_js(-1.5), -1.0);
        assert_eq!(round_js(2.5), 3.0);
        assert_eq!(round_js(-2.5), -2.0);
        assert_eq!(round_js(0.499_999_999_999_999_94), 0.0);
        assert_eq!(round_js(-0.499_999_999_999_999_94), 0.0);
        assert_eq!(round_js(84.45), 84.0);
        assert_eq!(round_js(-0.049), 0.0);
        assert_eq!(round_js(0.0), 0.0);
    }

    #[test]
    fn round_js_diverges_from_rust_round_exactly_on_negative_ties() {
        // The whole reason this module exists. If these ever agree, the helper
        // has stopped doing its job.
        assert_ne!(round_js(-0.5), (-0.5f64).round());
        assert_ne!(round_js(-1.5), (-1.5f64).round());
        // ...and agrees everywhere else.
        assert_eq!(round_js(0.5), 0.5f64.round());
        assert_eq!(round_js(2.5), 2.5f64.round());
    }

    #[test]
    fn round_js_passes_non_finite_values_through() {
        assert!(round_js(f64::NAN).is_nan());
        assert_eq!(round_js(f64::INFINITY), f64::INFINITY);
        assert_eq!(round_js(f64::NEG_INFINITY), f64::NEG_INFINITY);
    }

    #[test]
    fn round_to_one_decimal_matches_javascript() {
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
    fn round_js_reproduces_the_health_percentage_expression() {
        // health_pct is Math.round(ratio * 10000) / 100 — note the asymmetric
        // scale factors, which is why it cannot be expressed as round_to.
        for (full, design, expected) in [
            (48.6, 53.0, 91.7),
            (53.0, 53.0, 100.0),
            (41.333, 53.0, 77.99),
            (0.1, 3.0, 3.33),
        ] {
            assert_eq!(round_js((full / design) * 10_000.0) / 100.0, expected);
        }
    }

    #[test]
    fn js_number_matches_javascript_for_kernel_emitted_values() {
        assert_eq!(js_number("42"), Some(42.0));
        assert_eq!(js_number("  42  "), Some(42.0));
        assert_eq!(js_number("-22"), Some(-22.0));
        assert_eq!(js_number("-273150"), Some(-273_150.0));
        assert_eq!(js_number("12.75"), Some(12.75));
        assert_eq!(js_number("+5"), Some(5.0));
        assert_eq!(js_number("1e3"), Some(1000.0));
        assert_eq!(js_number(""), Some(0.0));
    }

    #[test]
    fn js_number_rejects_everything_javascript_would_leave_non_finite() {
        assert_eq!(js_number("abc"), None);
        assert_eq!(js_number("1_000"), None);
        assert_eq!(js_number("NaN"), None);
        // Accepted by Rust's parser, rejected by Number(); the finite filter
        // makes both reach the caller's fallback identically.
        assert_eq!(js_number("inf"), None);
        assert_eq!(js_number("Infinity"), None);
    }

    #[test]
    fn js_parse_int_matches_javascript() {
        assert_eq!(js_parse_int("  16384000 kB"), Some(16_384_000));
        assert_eq!(js_parse_int("16384000"), Some(16_384_000));
        assert_eq!(js_parse_int("-5 x"), Some(-5));
        assert_eq!(js_parse_int("12.9"), Some(12));
        assert_eq!(js_parse_int("+7"), Some(7));
        assert_eq!(js_parse_int("kB 12"), None);
        assert_eq!(js_parse_int(""), None);
        assert_eq!(js_parse_int("abc"), None);
        assert_eq!(js_parse_int("-"), None);
    }

    #[test]
    fn js_parse_int_returns_none_rather_than_wrapping_on_overflow() {
        assert_eq!(js_parse_int("99999999999999999999999"), None);
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
}
