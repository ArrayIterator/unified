// Reference data -- ISO 4217 currency catalogue.
//
// The single central currency catalogue for the whole platform.
// Every module that names a currency reads it from here
// (`unified_core::reference_data::currencies`); the i18n module
// re-exports it for its HTTP endpoints, and the money helpers
// (`unified_core::db::money`, pricing, payments, bank_transfer, ...)
// derive their zero-decimal behaviour from `minor_digits` here
// instead of keeping their own lists.
//
// Columns: (alphabetic code, numeric code, English name, symbol,
// minor-unit digits). Names are the ISO 4217 English names; symbols
// are conventional display symbols and are NOT part of ISO 4217.
// The list is the active ISO 4217 set: withdrawn codes (such as HRK,
// replaced by EUR when Croatia adopted the euro in 2023; BGN, replaced
// by EUR when Bulgaria adopted the euro in 2026; ANG, replaced by XCG
// in 2025) are absent, and supranational / funds codes (XAU, XAG, XDR,
// XBA-XBD, XTS, XXX, CHE, CHW, COU, MXV, UYW) are present with minor
// digits 0 where ISO 4217 defines no minor unit for them.
//
// One deliberate deviation from the ISO 4217 minor-unit column:
// none. IDR is 2 here, as ISO 4217 states it (the old 35-code i18n
// table listed IDR as 0); the money-math helpers already treated IDR
// as two-decimal, so this only changes the `format_currency` display
// for IDR from `Rp15000` to `Rp15000.00`.

use serde::Serialize;

/// One ISO 4217 currency: alphabetic + numeric code, English name,
/// display symbol, and the number of minor-unit digits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CurrencyInfo {
    pub code: String,
    /// ISO 4217 numeric code, zero-padded to three digits ("840").
    pub numeric_code: String,
    pub name: String,
    pub symbol: String,
    pub minor_digits: u8,
}

/// (code, numeric code, name, symbol, minor digits), alphabetical.
pub static ISO_4217_CURRENCIES: &[(&str, &str, &str, &str, u8)] = &[
    ("AED", "784", "UAE Dirham", "د.إ", 2),
    ("AFN", "971", "Afghan Afghani", "؋", 2),
    ("ALL", "008", "Albanian Lek", "Lek", 2),
    ("AMD", "051", "Armenian Dram", "֏", 2),
    ("AOA", "973", "Angolan Kwanza", "Kz", 2),
    ("ARS", "032", "Argentine Peso", "$", 2),
    ("AUD", "036", "Australian Dollar", "A$", 2),
    ("AWG", "533", "Aruban Florin", "ƒ", 2),
    ("AZN", "944", "Azerbaijani Manat", "₼", 2),
    ("BAM", "977", "Bosnia-Herzegovina Convertible Mark", "KM", 2),
    ("BBD", "052", "Barbadian Dollar", "Bds$", 2),
    ("BDT", "050", "Bangladeshi Taka", "৳", 2),
    ("BHD", "048", "Bahraini Dinar", ".د.ب", 3),
    ("BIF", "108", "Burundian Franc", "FBu", 0),
    ("BMD", "060", "Bermudian Dollar", "$", 2),
    ("BND", "096", "Brunei Dollar", "B$", 2),
    ("BOB", "068", "Bolivian Boliviano", "Bs", 2),
    ("BRL", "986", "Brazilian Real", "R$", 2),
    ("BSD", "044", "Bahamian Dollar", "B$", 2),
    ("BTN", "064", "Bhutanese Ngultrum", "Nu.", 2),
    ("BWP", "072", "Botswana Pula", "P", 2),
    ("BYN", "933", "Belarusian Ruble", "Br", 2),
    ("BZD", "084", "Belize Dollar", "BZ$", 2),
    ("CAD", "124", "Canadian Dollar", "C$", 2),
    ("CDF", "976", "Congolese Franc", "FC", 2),
    ("CHE", "947", "WIR Euro", "CHE", 2),
    ("CHF", "756", "Swiss Franc", "CHF", 2),
    ("CHW", "948", "WIR Franc", "CHW", 2),
    ("CLP", "152", "Chilean Peso", "$", 0),
    ("CNY", "156", "Chinese Yuan", "¥", 2),
    ("COP", "170", "Colombian Peso", "$", 2),
    ("COU", "970", "Colombian Real Value Unit", "COU", 2),
    ("CRC", "188", "Costa Rican Colón", "₡", 2),
    ("CUP", "192", "Cuban Peso", "$", 2),
    ("CVE", "132", "Cape Verdean Escudo", "$", 2),
    ("CZK", "203", "Czech Koruna", "Kč", 2),
    ("DJF", "262", "Djiboutian Franc", "Fdj", 0),
    ("DKK", "208", "Danish Krone", "kr", 2),
    ("DOP", "214", "Dominican Peso", "RD$", 2),
    ("DZD", "012", "Algerian Dinar", "د.ج", 2),
    ("EGP", "818", "Egyptian Pound", "E£", 2),
    ("ERN", "232", "Eritrean Nakfa", "Nfk", 2),
    ("ETB", "230", "Ethiopian Birr", "Br", 2),
    ("EUR", "978", "Euro", "€", 2),
    ("FJD", "242", "Fijian Dollar", "FJ$", 2),
    ("FKP", "238", "Falkland Islands Pound", "£", 2),
    ("GBP", "826", "Pound Sterling", "£", 2),
    ("GEL", "981", "Georgian Lari", "₾", 2),
    ("GHS", "936", "Ghanaian Cedi", "₵", 2),
    ("GIP", "292", "Gibraltar Pound", "£", 2),
    ("GMD", "270", "Gambian Dalasi", "D", 2),
    ("GNF", "324", "Guinean Franc", "FG", 0),
    ("GTQ", "320", "Guatemalan Quetzal", "Q", 2),
    ("GYD", "328", "Guyanese Dollar", "G$", 2),
    ("HKD", "344", "Hong Kong Dollar", "HK$", 2),
    ("HNL", "340", "Honduran Lempira", "L", 2),
    ("HTG", "332", "Haitian Gourde", "G", 2),
    ("HUF", "348", "Hungarian Forint", "Ft", 2),
    ("IDR", "360", "Indonesian Rupiah", "Rp", 2),
    ("ILS", "376", "Israeli New Shekel", "₪", 2),
    ("INR", "356", "Indian Rupee", "₹", 2),
    ("IQD", "368", "Iraqi Dinar", "ع.د", 3),
    ("IRR", "364", "Iranian Rial", "﷼", 2),
    ("ISK", "352", "Icelandic Króna", "kr", 0),
    ("JMD", "388", "Jamaican Dollar", "J$", 2),
    ("JOD", "400", "Jordanian Dinar", "د.ا", 3),
    ("JPY", "392", "Japanese Yen", "¥", 0),
    ("KES", "404", "Kenyan Shilling", "KSh", 2),
    ("KGS", "417", "Kyrgyzstani Som", "с", 2),
    ("KHR", "116", "Cambodian Riel", "៛", 2),
    ("KMF", "174", "Comorian Franc", "CF", 0),
    ("KPW", "408", "North Korean Won", "₩", 2),
    ("KRW", "410", "South Korean Won", "₩", 0),
    ("KWD", "414", "Kuwaiti Dinar", "د.ك", 3),
    ("KYD", "136", "Cayman Islands Dollar", "CI$", 2),
    ("KZT", "398", "Kazakhstani Tenge", "₸", 2),
    ("LAK", "418", "Lao Kip", "₭", 2),
    ("LBP", "422", "Lebanese Pound", "L£", 2),
    ("LKR", "144", "Sri Lankan Rupee", "Rs", 2),
    ("LRD", "430", "Liberian Dollar", "L$", 2),
    ("LSL", "426", "Lesotho Loti", "L", 2),
    ("LYD", "434", "Libyan Dinar", "ل.د", 3),
    ("MAD", "504", "Moroccan Dirham", "د.م.", 2),
    ("MDL", "498", "Moldovan Leu", "L", 2),
    ("MGA", "969", "Malagasy Ariary", "Ar", 2),
    ("MKD", "807", "Macedonian Denar", "ден", 2),
    ("MMK", "104", "Myanmar Kyat", "K", 2),
    ("MNT", "496", "Mongolian Tögrög", "₮", 2),
    ("MOP", "446", "Macanese Pataca", "MOP$", 2),
    ("MRU", "929", "Mauritanian Ouguiya", "UM", 2),
    ("MUR", "480", "Mauritian Rupee", "Rs", 2),
    ("MVR", "462", "Maldivian Rufiyaa", "Rf", 2),
    ("MWK", "454", "Malawian Kwacha", "MK", 2),
    ("MXN", "484", "Mexican Peso", "MX$", 2),
    ("MXV", "979", "Mexican Investment Unit (UDI)", "MXV", 2),
    ("MYR", "458", "Malaysian Ringgit", "RM", 2),
    ("MZN", "943", "Mozambican Metical", "MT", 2),
    ("NAD", "516", "Namibian Dollar", "N$", 2),
    ("NGN", "566", "Nigerian Naira", "₦", 2),
    ("NIO", "558", "Nicaraguan Córdoba", "C$", 2),
    ("NOK", "578", "Norwegian Krone", "kr", 2),
    ("NPR", "524", "Nepalese Rupee", "Rs", 2),
    ("NZD", "554", "New Zealand Dollar", "NZ$", 2),
    ("OMR", "512", "Omani Rial", "﷼", 3),
    ("PAB", "590", "Panamanian Balboa", "B/.", 2),
    ("PEN", "604", "Peruvian Sol", "S/", 2),
    ("PGK", "598", "Papua New Guinean Kina", "K", 2),
    ("PHP", "608", "Philippine Peso", "₱", 2),
    ("PKR", "586", "Pakistani Rupee", "Rs", 2),
    ("PLN", "985", "Polish Zloty", "zł", 2),
    ("PYG", "600", "Paraguayan Guaraní", "₲", 0),
    ("QAR", "634", "Qatari Riyal", "ر.ق", 2),
    ("RON", "946", "Romanian Leu", "lei", 2),
    ("RSD", "941", "Serbian Dinar", "дин", 2),
    ("RUB", "643", "Russian Ruble", "₽", 2),
    ("RWF", "646", "Rwandan Franc", "FRw", 0),
    ("SAR", "682", "Saudi Riyal", "﷼", 2),
    ("SBD", "090", "Solomon Islands Dollar", "SI$", 2),
    ("SCR", "690", "Seychellois Rupee", "Rs", 2),
    ("SDG", "938", "Sudanese Pound", "ج.س.", 2),
    ("SEK", "752", "Swedish Krona", "kr", 2),
    ("SGD", "702", "Singapore Dollar", "S$", 2),
    ("SHP", "654", "St Helena Pound", "£", 2),
    ("SLE", "925", "Sierra Leonean Leone", "Le", 2),
    ("SOS", "706", "Somali Shilling", "Sh", 2),
    ("SRD", "968", "Surinamese Dollar", "$", 2),
    ("SSP", "728", "South Sudanese Pound", "£", 2),
    ("STN", "930", "São Tomé and Príncipe Dobra", "Db", 2),
    ("SVC", "222", "Salvadoran Colón", "₡", 2),
    ("SYP", "760", "Syrian Pound", "S£", 2),
    ("SZL", "748", "Swazi Lilangeni", "L", 2),
    ("THB", "764", "Thai Baht", "฿", 2),
    ("TJS", "972", "Tajikistani Somoni", "ЅМ", 2),
    ("TMT", "934", "Turkmenistani Manat", "m", 2),
    ("TND", "788", "Tunisian Dinar", "د.ت", 3),
    ("TOP", "776", "Tongan Paʻanga", "T$", 2),
    ("TRY", "949", "Turkish Lira", "₺", 2),
    ("TTD", "780", "Trinidad and Tobago Dollar", "TT$", 2),
    ("TWD", "901", "New Taiwan Dollar", "NT$", 2),
    ("TZS", "834", "Tanzanian Shilling", "TSh", 2),
    ("UAH", "980", "Ukrainian Hryvnia", "₴", 2),
    ("UGX", "800", "Ugandan Shilling", "USh", 0),
    ("USD", "840", "US Dollar", "$", 2),
    ("UYU", "858", "Uruguayan Peso", "$U", 2),
    ("UYW", "927", "Uruguay Peso en Unidades Indexadas", "UYW", 0),
    ("UZS", "860", "Uzbekistani Soum", "so'm", 2),
    ("VED", "926", "Venezuelan Bolívar Soberano (funds)", "Bs.S", 2),
    ("VES", "928", "Venezuelan Bolívar Soberano", "Bs.", 2),
    ("VND", "704", "Vietnamese Dong", "₫", 0),
    ("VUV", "548", "Vanuatu Vatu", "Vt", 0),
    ("WST", "882", "Samoan Tala", "T", 2),
    ("XAF", "950", "Central African CFA Franc", "FCFA", 0),
    ("XAG", "961", "Silver", "XAG", 0),
    ("XAU", "959", "Gold", "XAU", 0),
    ("XBA", "955", "Bond Markets Unit European Composite Unit (EURCO)", "XBA", 0),
    ("XBB", "956", "Bond Markets Unit European Monetary Unit (E.M.U.-6)", "XBB", 0),
    ("XBC", "957", "Bond Markets Unit European Unit of Account 9 (E.U.A.-9)", "XBC", 0),
    ("XBD", "958", "Bond Markets Unit European Unit of Account 17 (E.U.A.-17)", "XBD", 0),
    ("XCD", "951", "East Caribbean Dollar", "EC$", 2),
    ("XCG", "532", "Caribbean Guilder", "Cg", 2),
    ("XDR", "960", "Special Drawing Rights", "XDR", 0),
    ("XOF", "952", "West African CFA Franc", "CFA", 0),
    ("XPF", "953", "CFP Franc", "₣", 0),
    ("XTS", "963", "Testing Code", "XTS", 0),
    ("XXX", "999", "No Currency", "XXX", 0),
    ("YER", "886", "Yemeni Rial", "﷼", 2),
    ("ZAR", "710", "South African Rand", "R", 2),
    ("ZMW", "967", "Zambian Kwacha", "ZK", 2),
    ("ZWG", "924", "Zimbabwe Gold", "ZiG", 2),
];

/// The full active ISO 4217 catalogue, in code order.
pub fn builtin_currencies() -> Vec<CurrencyInfo> {
    ISO_4217_CURRENCIES
        .iter()
        .map(|(code, numeric_code, name, symbol, minor_digits)| CurrencyInfo {
            code: (*code).to_string(),
            numeric_code: (*numeric_code).to_string(),
            name: (*name).to_string(),
            symbol: (*symbol).to_string(),
            minor_digits: *minor_digits,
        })
        .collect()
}

/// One currency by alphabetic code (case-insensitive, trimmed),
/// or `None` for an unknown code.
pub fn currency_info(code: &str) -> Option<CurrencyInfo> {
    let code = code.trim().to_uppercase();
    builtin_currencies()
        .into_iter()
        .find(|currency| currency.code == code)
}

/// True when the currency has no minor unit. Unknown codes return
/// `false`: an unrecognised code is never silently treated as whole
/// units. This is the single source the money helpers derive from;
/// see `unified_core::db::money::is_zero_decimal_currency`.
pub fn is_zero_decimal_currency(code: &str) -> bool {
    currency_info(code).is_some_and(|info| info.minor_digits == 0)
}

/// Number of minor-unit digits for a known code, or `None`.
pub fn currency_minor_digits(code: &str) -> Option<u8> {
    currency_info(code).map(|info| info.minor_digits)
}

/// Format an amount with the currency symbol and the currency's minor
/// digits (`Rp15000.00`, `$12.50`). Returns `None` for an unknown code.
pub fn format_currency(code: &str, amount: f64) -> Option<String> {
    let info = currency_info(code)?;
    Some(format!(
        "{}{:.*}",
        info.symbol, info.minor_digits as usize, amount
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_is_the_full_active_iso_4217_set() {
        assert_eq!(ISO_4217_CURRENCIES.len(), 169);
        // Codes are unique and in alphabetical order.
        let mut codes: Vec<&str> = ISO_4217_CURRENCIES.iter().map(|row| row.0).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), ISO_4217_CURRENCIES.len());
        assert!(ISO_4217_CURRENCIES.windows(2).all(|pair| pair[0].0 < pair[1].0));
        // Withdrawn codes stay out; the home currency is present.
        assert!(currency_info("HRK").is_none());
        assert_eq!(currency_info("idr").unwrap().numeric_code, "360");
        assert_eq!(currency_info("USD").unwrap().numeric_code, "840");
    }

    #[test]
    fn zero_decimal_set_is_the_iso_4217_exponent_zero_set() {
        for code in [
            "BIF", "CLP", "DJF", "GNF", "ISK", "JPY", "KMF", "KRW", "PYG", "RWF", "UGX",
            "VND", "VUV", "XAF", "XOF", "XPF",
        ] {
            assert!(is_zero_decimal_currency(code), "{code} must be zero-decimal");
        }
        assert!(!is_zero_decimal_currency("USD"));
        assert!(!is_zero_decimal_currency("IDR"));
        assert!(!is_zero_decimal_currency("MGA"));
        assert!(!is_zero_decimal_currency("XXX-NOPE"));
    }

    #[test]
    fn three_decimal_currencies_keep_three_digits() {
        for code in ["BHD", "IQD", "JOD", "KWD", "LYD", "OMR", "TND"] {
            assert_eq!(currency_minor_digits(code), Some(3), "{code}");
        }
        assert_eq!(format_currency("USD", 12.5).unwrap(), "$12.50");
        assert_eq!(format_currency("JPY", 1500.0).unwrap(), "¥1500");
        assert_eq!(format_currency("BHD", 1.5).unwrap(), ".د.ب1.500");
    }
}
