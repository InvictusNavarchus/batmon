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

/// `Number.prototype.toFixed(digits)`: a decimal string with a fixed number of
/// fractional digits.
///
/// Note which rounding this uses, because JavaScript has two and they differ.
/// [`round_js`] breaks ties toward positive infinity, matching `Math.round`;
/// `toFixed` breaks them *away from zero*, so `(-0.5).toFixed(0)` is `"-1"`
/// where `Math.round(-0.5)` is `0`. Rust's [`f64::round`] happens to match
/// `toFixed` precisely — which is exactly why it must never be reached for
/// while porting a `Math.round`.
///
/// Rust's own `{:.1}` formatter cannot stand in for this: it rounds half to
/// *even*, so `format!("{:.1}", 45.25)` is `"45.2"` where JavaScript gives
/// `"45.3"`. Battery temperatures read from hwmon are millidegrees divided by a
/// thousand and land on those ties routinely.
///
/// Non-finite inputs are passed through to Rust's formatter and will render as
/// `NaN` or `inf` rather than JavaScript's `NaN`/`Infinity`. Every value that
/// reaches this has already been through [`js_number`], which rejects both.
/// Fractional digits sufficient to render any finite f64 exactly. Every one is
/// a dyadic rational, and the smallest subnormal needs 1074 places.
const EXACT_DIGITS: usize = 1080;

#[must_use]
pub fn to_fixed(value: f64, digits: usize) -> String {
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
