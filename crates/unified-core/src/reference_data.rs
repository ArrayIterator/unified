//! Reference data: platform-shared, single-source-of-truth lists.
//!
//! The four lists every module reads instead of keeping its own copy:
//!
//! * [`countries`] -- ISO 3166-1 countries with flag file paths.
//! * [`currencies`] -- the full active ISO 4217 currency catalogue.
//!
//! Languages and timezones live in the server crate (they are owned by
//! the i18n and settings modules there) and are re-exported for HTTP
//! consumers by the `reference_data` server module; this crate only
//! holds the two lists that are pure data with no server state.
//!
//! Everything here is platform-shared and NEVER tenant-scoped: the
//! lists are compiled in, identical for every tenant, and read-only.
//! No database table backs them, so no migration exists for them.

pub mod countries;
pub mod currencies;

pub use self::countries::{CountryInfo, FLAG_BASE_PATH, builtin_countries, country_info, flag_path};
pub use self::currencies::{
    CurrencyInfo, ISO_4217_CURRENCIES, builtin_currencies, currency_info, currency_minor_digits,
    format_currency, is_zero_decimal_currency,
};
