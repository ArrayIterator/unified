//! Money at the database boundary: integer minor units (`i64`)
//! against Diesel `PgNumeric` (PostgreSQL NUMERIC columns).
//!
//! DB columns stay NUMERIC: no column type changes and no
//! migration. These helpers are the only conversion between the
//! in-Rust minor-unit representation and the stored value, so a
//! future persistence wiring converts here and nowhere else.
//!
//! `PgNumeric` stores base-10000 digit groups with a `weight`
//! (how many groups sit before the decimal point, minus one) and
//! a `scale` (significant decimal digits). The encoding below is
//! exact integer arithmetic: no float ever holds the amount.
//!
//! Scale is 2 for two-decimal currencies and 0 for zero-decimal
//! currencies (JPY, KRW, VND, CLP, ISK, UGX, XAF, XOF, XPF),
//! matching the billing and store modules' lists. Reading a
//! stored value with more decimals than the currency carries
//! rounds half away from zero to minor units, matching the
//! long-standing `round_money` behaviour.

use diesel::data_types::PgNumeric;

/// True for zero-decimal currencies (no minor units).
pub fn is_zero_decimal_currency(currency: &str) -> bool {
    matches!(
        currency,
        "JPY" | "KRW" | "VND" | "CLP" | "ISK" | "UGX" | "XAF" | "XOF" | "XPF"
    )
}

/// Minor-unit scale for a currency: 0 for zero-decimal
/// currencies, 2 otherwise.
pub fn minor_scale(currency: &str) -> u16 {
    if is_zero_decimal_currency(currency) {
        0
    } else {
        2
    }
}

/// Encode integer minor units as a NUMERIC value with the
/// currency's scale.
pub fn minor_to_pg_numeric(amount_minor: i64, currency: &str) -> PgNumeric {
    let scale = minor_scale(currency);
    let negative = amount_minor < 0;
    let absolute = (amount_minor as i128).unsigned_abs();
    let divisor: u128 = 10u128.pow(u32::from(scale));
    let int_part = absolute / divisor;
    let frac_part = absolute % divisor;

    // Integer part, least-significant base-10000 group first.
    let mut int_groups: Vec<i16> = Vec::new();
    let mut rest = int_part;
    while rest > 0 {
        int_groups.push((rest % 10_000) as i16);
        rest /= 10_000;
    }
    if int_groups.is_empty() {
        int_groups.push(0);
    }
    int_groups.reverse();

    // Fractional part: exactly `scale` decimal digits, right-padded
    // with zeros to whole base-10000 groups; trailing all-zero
    // groups carry no value and are dropped.
    let mut frac_groups: Vec<i16> = Vec::new();
    if scale > 0 {
        let frac_text = format!("{frac_part:0width$}", width = scale as usize);
        let mut padded = frac_text;
        while padded.len() % 4 != 0 {
            padded.push('0');
        }
        for chunk in padded.as_bytes().chunks(4) {
            let group: i16 = std::str::from_utf8(chunk)
                .unwrap_or("0")
                .parse()
                .unwrap_or(0);
            frac_groups.push(group);
        }
        while frac_groups.last() == Some(&0) {
            frac_groups.pop();
        }
    }

    let mut digits = int_groups;
    let weight = (digits.len() as i16) - 1;
    digits.extend(frac_groups);
    if negative {
        PgNumeric::Negative {
            weight,
            scale,
            digits,
        }
    } else {
        PgNumeric::Positive {
            weight,
            scale,
            digits,
        }
    }
}

/// Decode a NUMERIC value to integer minor units for the currency.
/// Returns `None` for NaN, for values that do not fit in `i64`
/// minor units, and for malformed digit groups.
pub fn pg_numeric_to_minor(value: &PgNumeric, currency: &str) -> Option<i64> {
    let (weight, scale, digits, negative) = match value {
        PgNumeric::Positive {
            weight,
            scale,
            digits,
        } => (*weight, *scale, digits, false),
        PgNumeric::Negative {
            weight,
            scale,
            digits,
        } => (*weight, *scale, digits, true),
        PgNumeric::NaN => return None,
    };
    if digits.iter().any(|digit| *digit < 0 || *digit > 9_999) {
        return None;
    }
    let scale = usize::from(scale);
    // Digit groups as decimal text: the first group is unpadded,
    // every later group is zero-padded to four digits.
    let groups_text = |groups: &[i16]| -> String {
        let mut out = String::new();
        for (index, digit) in groups.iter().enumerate() {
            if index == 0 {
                out.push_str(&digit.to_string());
            } else {
                out.push_str(&format!("{digit:04}"));
            }
        }
        out
    };
    // Groups before the decimal point: `weight + 1` of them.
    let int_group_count = weight as i64 + 1;
    let (int_text, frac_text) = if int_group_count <= 0 {
        // Value below one: leading zero groups, then every digit
        // group is fractional.
        let leading = "0000".repeat((-int_group_count) as usize);
        let frac_groups: String = digits
            .iter()
            .map(|digit| format!("{digit:04}"))
            .collect();
        ("0".to_string(), format!("{leading}{frac_groups}"))
    } else if int_group_count as usize >= digits.len() {
        // No fractional groups; the integer part is zero-padded
        // with whole groups past the last digit.
        let mut int_groups = groups_text(digits);
        let missing = int_group_count as usize - digits.len();
        int_groups.push_str(&"0000".repeat(missing));
        (int_groups, String::new())
    } else {
        let count = int_group_count as usize;
        let int_groups = groups_text(&digits[..count]);
        let frac_groups: String = digits[count..]
            .iter()
            .map(|digit| format!("{digit:04}"))
            .collect();
        (int_groups, frac_groups)
    };
    // The stored value carries `scale` significant decimal digits.
    let mut frac_digits = frac_text;
    if frac_digits.len() > scale {
        frac_digits.truncate(scale);
    }
    while frac_digits.len() < scale {
        frac_digits.push('0');
    }
    decimal_to_minor(&int_text, &frac_digits, minor_scale(currency), negative)
}

/// Convert a decimal `int_part.frac_part` string to minor units,
/// rounding half away from zero on the first discarded fraction
/// digit. `frac_part` may carry more digits than the currency's
/// scale; `minor_scale` is the target scale (0 or 2).
fn decimal_to_minor(
    int_text: &str,
    frac_text: &str,
    target_scale: u16,
    negative: bool,
) -> Option<i64> {
    let target = usize::from(target_scale);
    let int_value: i128 = if int_text.is_empty() {
        0
    } else {
        int_text.parse().ok()?
    };
    let mut minor = int_value.checked_mul(10i128.pow(u32::from(target_scale)))?;
    if target == 0 {
        // Zero-decimal currency: round on the first fraction digit.
        if frac_text.as_bytes().first().is_some_and(|digit| *digit >= b'5') {
            minor = minor.checked_add(1)?;
        }
    } else {
        let kept: i128 = if frac_text.len() >= target {
            frac_text[..target].parse().ok()?
        } else {
            let mut padded = frac_text.to_string();
            while padded.len() < target {
                padded.push('0');
            }
            padded.parse().ok()?
        };
        minor = minor.checked_add(kept)?;
        if frac_text.as_bytes().get(target).is_some_and(|digit| *digit >= b'5') {
            minor = minor.checked_add(1)?;
        }
    }
    let minor = if negative { -minor } else { minor };
    i64::try_from(minor).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(minor: i64, currency: &str) {
        let numeric = minor_to_pg_numeric(minor, currency);
        assert_eq!(
            pg_numeric_to_minor(&numeric, currency),
            Some(minor),
            "round-trip failed for {minor} {currency}: {numeric:?}"
        );
    }

    #[test]
    fn minor_to_pg_numeric_round_trips_two_decimal_currencies() {
        for minor in [
            0,
            1,
            -1,
            5,
            99,
            -99,
            100,
            101,
            -250,
            1025,
            10050,
            -10050,
            123456789,
            -123456789,
            1_000_000_000_000,
            -9_999_999_999_999_999,
        ] {
            round_trip(minor, "USD");
            round_trip(minor, "EUR");
        }
    }

    #[test]
    fn minor_to_pg_numeric_round_trips_zero_decimal_currencies() {
        for minor in [0, 1, -1, 500, 10000, -10000, 123456789] {
            round_trip(minor, "JPY");
            round_trip(minor, "KRW");
        }
    }

    #[test]
    fn minor_to_pg_numeric_encodes_known_values() {
        assert_eq!(
            minor_to_pg_numeric(10050, "USD"),
            PgNumeric::Positive {
                weight: 0,
                scale: 2,
                digits: vec![100, 5000],
            }
        );
        assert_eq!(
            minor_to_pg_numeric(-250, "USD"),
            PgNumeric::Negative {
                weight: 0,
                scale: 2,
                digits: vec![2, 5000],
            }
        );
        assert_eq!(
            minor_to_pg_numeric(500, "JPY"),
            PgNumeric::Positive {
                weight: 0,
                scale: 0,
                digits: vec![500],
            }
        );
        assert_eq!(
            minor_to_pg_numeric(0, "USD"),
            PgNumeric::Positive {
                weight: 0,
                scale: 2,
                digits: vec![0],
            }
        );
    }

    #[test]
    fn pg_numeric_to_minor_rounds_extra_decimals_half_away_from_zero() {
        // 10.005 stored with scale 3 -> 1001 minor units.
        // The fractional group is 50: 0.005 * 10000.
        let value = PgNumeric::Positive {
            weight: 0,
            scale: 3,
            digits: vec![10, 50],
        };
        assert_eq!(pg_numeric_to_minor(&value, "USD"), Some(1001));
        let value = PgNumeric::Negative {
            weight: 0,
            scale: 3,
            digits: vec![10, 50],
        };
        assert_eq!(pg_numeric_to_minor(&value, "USD"), Some(-1001));
        // 10.004 -> 1000 minor units.
        let value = PgNumeric::Positive {
            weight: 0,
            scale: 3,
            digits: vec![10, 40],
        };
        assert_eq!(pg_numeric_to_minor(&value, "USD"), Some(1000));
        // Zero-decimal: 10.5 JPY rounds to 11.
        let value = PgNumeric::Positive {
            weight: 0,
            scale: 1,
            digits: vec![10, 5000],
        };
        assert_eq!(pg_numeric_to_minor(&value, "JPY"), Some(11));
    }

    #[test]
    fn pg_numeric_to_minor_rejects_nan_and_overflow() {
        assert_eq!(pg_numeric_to_minor(&PgNumeric::NaN, "USD"), None);
        // More than i64 minor units: 10^20 major units. In Postgres
        // numeric encoding the first digit group sits at 10000^weight,
        // so 10^20 needs weight 5 (1 x 10000^5), not weight 4.
        let value = PgNumeric::Positive {
            weight: 5,
            scale: 0,
            digits: vec![1, 0, 0, 0, 0, 0],
        };
        assert_eq!(pg_numeric_to_minor(&value, "USD"), None);
    }

    #[test]
    fn pg_numeric_to_minor_reads_sub_one_values() {
        // 0.05 stored as one fractional group.
        let value = PgNumeric::Positive {
            weight: -1,
            scale: 2,
            digits: vec![500],
        };
        assert_eq!(pg_numeric_to_minor(&value, "USD"), Some(5));
    }
}
