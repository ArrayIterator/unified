//! Money: integer minor-unit representation and its boundaries.
//!
//! Money inside the server is integer minor units (`i64` cents for
//! two-decimal currencies, whole units for zero-decimal ones). Two
//! boundaries convert:
//!
//! * JSON: the HTTP API and hook payloads carry integer minor units
//!   (for example `1025` for 1025 minor units of USD), converted
//!   here by the `serde_major` helpers. A field annotated with
//!   `#[serde(with = "unified_core::money::serde_major")]` is `i64`
//!   minor units in Rust and an integer JSON number on the wire.
//!   Deserializing still accepts a legacy major-unit JSON float
//!   (for example `10.25`) for backward compatibility with older
//!   emitters, converting it with a range check; that legacy path
//!   rejects out-of-range and non-finite values instead of
//!   silently saturating.
//! * Database: NUMERIC columns stay NUMERIC. See
//!   `crate::db::money` for the `PgNumeric` conversions.
//!
//! The JSON conversion assumes two decimal places. Zero-decimal
//! currencies (JPY, KRW, ...) must NOT use these serde helpers on a
//! bare field, because the field cannot see the sibling `currency`
//! value; code that handles zero-decimal currencies converts
//! explicitly with the currency-aware `major_to_minor` /
//! `minor_to_major` helpers in the billing and store modules
//! instead. In practice the store/billing JSON surfaces use
//! two-decimal currencies, and hook payloads built by hand keep
//! using the currency-aware helpers, so this assumption only
//! affects the struct-level JSON shape, never the hook contracts.

use serde::{Deserialize, Deserializer, Serializer};

/// Largest major-unit magnitude that still converts to an `i64`
/// minor-unit value at two decimals: `i64::MAX / 100`, rounded
/// down. Anything larger would overflow the cast, so it is an
/// error instead of silently saturating to `i64::MAX`.
const MAX_MAJOR: f64 = 92_233_720_368_547_758.0;

/// Convert a two-decimal major-unit float to integer minor units,
/// rounding half away from zero. Returns `None` for non-finite
/// values and for magnitudes that would overflow `i64` - never
/// silently saturates.
pub fn major_to_minor_checked(major: f64) -> Option<i64> {
    if !major.is_finite() || major.abs() > MAX_MAJOR {
        return None;
    }
    let scaled = (major * 100.0).round();
    if scaled > i64::MAX as f64 || scaled < i64::MIN as f64 {
        return None;
    }
    Some(scaled as i64)
}

/// Serde bridge for one money field: `i64` minor units in Rust,
/// an integer JSON number on the wire.
///
/// Deserializing accepts an integer JSON number (minor units, the
/// canonical form) or a legacy major-unit float, which is converted
/// with the same range check as `major_to_minor_checked`.
/// Non-finite and out-of-range values are rejected instead of
/// silently becoming zero or saturating.
pub mod serde_major {
    use super::*;

    /// Accept either an integer JSON number (minor units) or a legacy
    /// major-unit float. Module-level (not function-local) so the
    /// `option` submodule below can share it: `use super::*` only
    /// imports module-level items, so a function-local enum here
    /// would be out of scope in `option::deserialize` (E0425).
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum MinorOrMajor {
        Minor(i64),
        Major(f64),
    }

    pub fn serialize<S>(minor: &i64, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_i64(*minor)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<i64, D::Error>
    where
        D: Deserializer<'de>,
    {
        match MinorOrMajor::deserialize(deserializer)? {
            MinorOrMajor::Minor(minor) => Ok(minor),
            MinorOrMajor::Major(major) => major_to_minor_checked(major).ok_or_else(|| {
                serde::de::Error::custom("money amount is out of range or non-finite")
            }),
        }
    }

    /// Serde bridge for `Option<i64>` money fields: `None` stays
    /// JSON `null`, `Some` converts like `serde_major`.
    pub mod option {
        use super::*;

        pub fn serialize<S>(minor: &Option<i64>, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            match minor {
                Some(value) => serializer.serialize_some(value),
                None => serializer.serialize_none(),
            }
        }

        pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
        where
            D: Deserializer<'de>,
        {
            let value = Option::<MinorOrMajor>::deserialize(deserializer)?;
            match value {
                Some(MinorOrMajor::Minor(minor)) => Ok(Some(minor)),
                Some(MinorOrMajor::Major(major)) => {
                    major_to_minor_checked(major).map(Some).ok_or_else(|| {
                        serde::de::Error::custom("money amount is out of range or non-finite")
                    })
                }
                None => Ok(None),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{major_to_minor_checked, serde_major};
    use serde::{Deserialize, Serialize};

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Priced {
        #[serde(with = "serde_major")]
        price: i64,
        #[serde(with = "serde_major::option", default)]
        renewal_price: Option<i64>,
    }

    #[test]
    fn serde_major_round_trips_minor_units() {
        let priced = Priced {
            price: 1025,
            renewal_price: Some(999),
        };
        let json = serde_json::to_string(&priced).unwrap();
        assert_eq!(json, r#"{"price":1025,"renewal_price":999}"#);
        let back: Priced = serde_json::from_str(&json).unwrap();
        assert_eq!(back, priced);
    }

    #[test]
    fn serde_major_accepts_legacy_major_float_and_rounds_half_away_from_zero() {
        let priced: Priced = serde_json::from_str(r#"{"price":10.25}"#).unwrap();
        assert_eq!(priced.price, 1025);
        let priced: Priced = serde_json::from_str(r#"{"price":10}"#).unwrap();
        // A bare integer is now canonical minor units, not 10 major.
        assert_eq!(priced.price, 10);
        assert_eq!(priced.renewal_price, None);
        let priced: Priced = serde_json::from_str(r#"{"price":10.005}"#).unwrap();
        assert_eq!(priced.price, 1001);
        let priced: Priced = serde_json::from_str(r#"{"price":-10.005}"#).unwrap();
        assert_eq!(priced.price, -1001);
    }

    #[test]
    fn serde_major_rejects_non_finite_and_out_of_range_numbers() {
        assert!(serde_json::from_str::<Priced>(r#"{"price":1e400}"#).is_err());
        // 1e17 major units would saturate to i64::MAX under the old
        // unchecked cast; it is now an error.
        assert!(serde_json::from_str::<Priced>(r#"{"price":1e17}"#).is_err());
        assert!(serde_json::from_str::<Priced>(r#"{"price":-1e17}"#).is_err());
    }

    #[test]
    fn serde_major_option_round_trips_none_as_null() {
        let priced = Priced {
            price: 0,
            renewal_price: None,
        };
        let json = serde_json::to_string(&priced).unwrap();
        assert_eq!(json, r#"{"price":0,"renewal_price":null}"#);
        let back: Priced = serde_json::from_str(&json).unwrap();
        assert_eq!(back, priced);
    }

    #[test]
    fn major_to_minor_checked_rejects_overflow() {
        assert_eq!(major_to_minor_checked(10.25), Some(1025));
        assert_eq!(major_to_minor_checked(0.0), Some(0));
        assert_eq!(major_to_minor_checked(1e17), None);
        assert_eq!(major_to_minor_checked(f64::MAX), None);
        assert_eq!(major_to_minor_checked(f64::NAN), None);
        assert_eq!(major_to_minor_checked(f64::INFINITY), None);
    }
}
