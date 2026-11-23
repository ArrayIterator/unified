// i18n -- Currency reference data and formatting.

use serde::Serialize;

// ---------------------------------------------------------------------------
// Currencies: compact ISO 4217 table (code, English name, symbol, and the
// number of minor-unit digits). This is reference data for display and
// formatting only; exchange rates live elsewhere.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct CurrencyInfo {
    pub code: String,
    pub name: String,
    pub symbol: String,
    pub minor_digits: u8,
}

pub fn builtin_currencies() -> Vec<CurrencyInfo> {
    // (code, name, symbol, minor digits)
    let rows: &[(&str, &str, &str, u8)] = &[
        ("USD", "US Dollar", "$", 2),
        ("EUR", "Euro", "€", 2),
        ("GBP", "Pound Sterling", "£", 2),
        ("JPY", "Japanese Yen", "¥", 0),
        ("IDR", "Indonesian Rupiah", "Rp", 0),
        ("SGD", "Singapore Dollar", "S$", 2),
        ("MYR", "Malaysian Ringgit", "RM", 2),
        ("THB", "Thai Baht", "฿", 2),
        ("PHP", "Philippine Peso", "₱", 2),
        ("VND", "Vietnamese Dong", "₫", 0),
        ("INR", "Indian Rupee", "₹", 2),
        ("CNY", "Chinese Yuan", "¥", 2),
        ("HKD", "Hong Kong Dollar", "HK$", 2),
        ("AUD", "Australian Dollar", "A$", 2),
        ("CAD", "Canadian Dollar", "C$", 2),
        ("CHF", "Swiss Franc", "CHF", 2),
        ("NZD", "New Zealand Dollar", "NZ$", 2),
        ("KRW", "South Korean Won", "₩", 0),
        ("TWD", "New Taiwan Dollar", "NT$", 2),
        ("AED", "UAE Dirham", "د.إ", 2),
        ("SAR", "Saudi Riyal", "﷼", 2),
        ("TRY", "Turkish Lira", "₺", 2),
        ("BRL", "Brazilian Real", "R$", 2),
        ("MXN", "Mexican Peso", "MX$", 2),
        ("ZAR", "South African Rand", "R", 2),
        ("PLN", "Polish Zloty", "zł", 2),
        ("CZK", "Czech Koruna", "Kč", 2),
        ("SEK", "Swedish Krona", "kr", 2),
        ("NOK", "Norwegian Krone", "kr", 2),
        ("DKK", "Danish Krone", "kr", 2),
        ("BHD", "Bahraini Dinar", ".د.ب", 3),
        ("KWD", "Kuwaiti Dinar", "د.ك", 3),
        ("OMR", "Omani Rial", "﷼", 3),
        ("ILS", "Israeli New Shekel", "₪", 2),
        ("UAH", "Ukrainian Hryvnia", "₴", 2),
    ];
    rows.iter()
        .map(|(code, name, symbol, minor_digits)| CurrencyInfo {
            code: (*code).to_string(),
            name: (*name).to_string(),
            symbol: (*symbol).to_string(),
            minor_digits: *minor_digits,
        })
        .collect()
}

pub fn currency_info(code: &str) -> Option<CurrencyInfo> {
    let code = code.trim().to_uppercase();
    builtin_currencies()
        .into_iter()
        .find(|currency| currency.code == code)
}

/// Format an amount with the currency symbol and the currency's minor
/// digits (`Rp15000`, `$12.50`). Returns `None` for an unknown code.
pub fn format_currency(code: &str, amount: f64) -> Option<String> {
    let info = currency_info(code)?;
    Some(format!(
        "{}{:.*}",
        info.symbol, info.minor_digits as usize, amount
    ))
}
