// i18n -- Currency reference data and formatting.
//
// Thin re-export of the single central ISO 4217 catalogue, which lives
// in `unified_core::reference_data::currencies`. This module
// keeps its original public names so the i18n HTTP endpoints
// (`GET /api/v1/i18n/currencies[/<code>]`) and every existing caller
// keep working unchanged; the data itself is owned by unified-core and
// must never be duplicated here again.
//
// The catalogue is the full active ISO 4217 set (169 codes, with
// numeric codes and minor-unit digits). One behaviour note: IDR is 2
// minor digits here, as ISO 4217 states it -- the old 35-code table
// listed IDR as 0, so `format_currency("IDR", ...)` now renders
// `Rp15000.00` instead of `Rp15000`. The money-math helpers already
// treated IDR as two-decimal; only this display helper changes.

pub use unified_core::reference_data::currencies::{
    CurrencyInfo, builtin_currencies, currency_info, format_currency,
};
