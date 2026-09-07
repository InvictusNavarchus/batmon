//! How values are parsed from the kernel, rounded for storage, and rendered for
//! display.
//!
//! Each helper is centralised rather than open-coded at the call sites because
//! every one of them fails silently. A rounding mode picked by accident does not
//! crash; it writes a slightly different number into a column that years of
//! history are compared against, and nothing points at the cause.
//!
//! Two of them deliberately do not use the obvious standard-library equivalent.
//! `f64::round` and `{:.N}` each break ties differently from what this daemon
//! stores and shows, so substituting either changes recorded data or on-screen
//! text. Those choices are load-bearing and the reasons are given below.

use jiff::Timestamp;

/// Round to the nearest integer, breaking ties toward positive infinity.
///
/// Not [`f64::round`], which breaks ties *away from zero*. The two agree on
/// every positive value and disagree on every negative tie: this returns `0.0`
/// for `-0.5` where [`f64::round`] returns `-1.0`. Temperatures are why that
/// matters — the hwmon floor is -50 °C, so sub-zero readings are in range.
///
/// Implemented as "floor, then step up if the fraction reaches a half" rather
/// than the usual `(x + 0.5).floor()` shortcut, which overshoots for the largest
/// double below one half: `0.499_999_999_999_999_94 + 0.5` rounds up to exactly
/// `1.0` in binary floating point, giving `1` where the answer is `0`.
#[must_use]
pub fn round_half_up(x: f64) -> f64 {
    if !x.is_finite() {
        return x;
    }
    let floor = x.floor();
    if x - floor >= 0.5 { floor + 1.0 } else { floor }
}

/// Round to `digits` decimal places by scaling up, rounding, and scaling back.
///
/// The rounding error of the scaling multiply is part of the result rather than
/// a defect in it. `-3.15` is not representable, so `-3.15 * 10.0` is
/// `-31.499999999999996` and the answer is `-3.1`, not the `-3.2` that rounding
/// the decimal literal would suggest. Stored values are compared across years of
/// recorded history, so this arithmetic has to stay put.
#[must_use]
pub fn round_to(x: f64, digits: i32) -> f64 {
    let scale = 10f64.powi(digits);
    round_half_up(x * scale) / scale
}

/// Parse a numeric attribute as emitted by `/sys`, rejecting anything
/// non-finite.
///
/// Rust's parser accepts `inf`, `infinity` and `nan`. The finite filter drops
/// them so a garbled attribute reads as absent rather than poisoning every
/// figure derived from it. Hexadecimal is not accepted, because no file under
/// `/sys/class/power_supply` or `/sys/class/hwmon` is hex-encoded.
///
/// One sharp edge: an empty string returns `Some(0.0)`, not [`None`], so an
/// empty attribute reads as a zero measurement. Callers for which that is wrong
/// filter the string first — see `BatteryReader::read_str`. Left as it is here
/// because changing it changes behaviour.
#[must_use]
pub fn parse_number(s: &str) -> Option<f64> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Some(0.0);
    }
    trimmed.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// Parse the leading integer of a string: skip whitespace, take an optional
/// sign, then consume digits and stop at the first byte that is not one.
///
/// This is what lets `"  16384000 kB"` from `/proc/meminfo` parse without
/// splitting the line first — `str::parse` rejects the trailing unit. Returns
/// [`None`] when no digits are present and on overflow, which `/proc/meminfo`
/// kilobyte values cannot approach.
#[must_use]
pub fn parse_leading_int(s: &str) -> Option<i64> {
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

/// Fractional digits sufficient to render any finite f64 exactly. Every one is
/// a dyadic rational, and the smallest subnormal needs 1074 places.
const EXACT_DIGITS: usize = 1080;

/// Render a decimal string with a fixed number of fractional digits, breaking
/// ties *away from zero*.
///
/// Rust's `{:.N}` cannot stand in for this: it rounds half to *even*, so
/// `format!("{:.1}", 45.25)` is `"45.2"` where this gives `"45.3"`. Readings
/// land on those ties constantly, because they are integers from the kernel
/// scaled by a power of ten — measured against real recorded samples, 8% of
/// whole-degree CPU temperatures differ between the two. Away-from-zero is also
/// the rounding a reader expects: 46.5 °C shown as 47, not 46.
///
/// This is a different tie rule from [`round_half_up`], which goes toward
/// positive infinity. They agree on positive values and part company on
/// negative halves: `-0.5` at zero digits renders `"-1"` here where
/// [`round_half_up`] gives `0`. The two are not interchangeable.
///
/// Non-finite inputs fall through to Rust's formatter and render as `NaN` or
/// `inf`. Every value reaching here has already been through [`parse_number`],
/// which rejects both.
#[must_use]
pub fn format_decimals(value: f64, digits: usize) -> String {
    if !value.is_finite() {
        return format!("{value}");
    }

    // Sign is taken from a strict comparison so negative zero renders unsigned,
    // matching the spec's "if x < 0" step: (-0).toFixed(1) is "0.0" but
    // (-0.04).toFixed(1) is "-0.0".
    let negative = value < 0.0;

    // The exact decimal expansion. Every finite f64 is a dyadic rational with at
    // most 1074 fractional digits, so this precision renders the double exactly
    // rather than rounding it — which is the whole point. Rounding at the target
    // precision first would let the formatter's own half-to-even step run before
    // ours, and scaling by a power of ten first is worse still: 0.15 * 10.0
    // rounds *up* to exactly 1.5, manufacturing a tie the real value does not
    // have, and turning "0.1" into "0.2".
    let exact = format!("{:.*}", EXACT_DIGITS, value.abs());
    let (integer, fraction) = exact
        .split_once('.')
        .expect("a non-zero precision always renders a decimal point");

    let (kept, remainder) = fraction.split_at(digits.min(fraction.len()));

    // Ties round away from zero, and anything above a half rounds up too, so a
    // leading digit of five or more is sufficient — whatever follows it.
    let round_up = remainder
        .as_bytes()
        .first()
        .is_some_and(|digit| *digit >= b'5');

    let mut rendered: Vec<u8> = integer.bytes().chain(kept.bytes()).collect();
    if round_up {
        carry_one(&mut rendered);
    }

    let text = String::from_utf8(rendered).expect("decimal digits are ascii");
    let (integer_out, fraction_out) = text.split_at(text.len() - digits);

    let mut out = String::with_capacity(text.len() + 2);
    if negative {
        out.push('-');
    }
    out.push_str(integer_out);
    if digits > 0 {
        out.push('.');
        out.push_str(fraction_out);
    }
    out
}

/// Add one to a big-endian decimal digit string, growing it on overflow.
fn carry_one(digits: &mut Vec<u8>) {
    for digit in digits.iter_mut().rev() {
        if *digit == b'9' {
            *digit = b'0';
        } else {
            *digit += 1;
            return;
        }
    }
    digits.insert(0, b'1');
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
mod tests;
