// Reference data -- ISO 3166-1 country registry (task 361).
//
// The single central country list for the whole platform, with the
// flag file for every country. No module keeps its own country
// list: the geo module only resolves countries from IP lookups and
// admin overrides, it never owned a list, so this registry is new
// and nothing had to be migrated onto it.
//
// Flag files live in `assets/icons/flags/` (served at
// `/assets/icons/flags/...`): both the 4x3 and the 1x1 variant from
// flag-icons (lipis/flag-icons, MIT, see
// `assets/icons/flags/LICENSE.flag-icons`). The registry maps each
// ISO 3166-1 alpha-2 code to both file paths; the 22 extra flag
// files flag-icons ships for non-ISO entities (eu, gb-eng, arab, ...)
// are NOT in this registry -- they exist on disk for direct use
// only. The preview listing is `assets/icons/flags/PREVIEW.md`.
//
// Names are the CLDR display names (short common names such as
// "United States", not the formal "United States of America").

use serde::Serialize;

/// One ISO 3166-1 country: alpha-2 + numeric code, English name,
/// and the paths (relative to the repo root) of both flag variants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CountryInfo {
    pub code: String,
    /// ISO 3166-1 numeric code, as listed by ISO ("840"); small codes
    /// are stored unpadded ("4", "20"), not zero-padded to three digits.
    pub numeric_code: String,
    pub name: String,
    /// 4x3 flag, for example `assets/icons/flags/4x3/us.svg`.
    pub flag_4x3: String,
    /// 1x1 (square) flag, for example `assets/icons/flags/1x1/us.svg`.
    pub flag_1x1: String,
}

/// (alpha-2 code, numeric code, name), alphabetical by code.
pub static ISO_3166_COUNTRIES: &[(&str, &str, &str)] = &[
    ("AD", "20", "Andorra"),
    ("AE", "784", "United Arab Emirates"),
    ("AF", "4", "Afghanistan"),
    ("AG", "28", "Antigua & Barbuda"),
    ("AI", "660", "Anguilla"),
    ("AL", "8", "Albania"),
    ("AM", "51", "Armenia"),
    ("AO", "24", "Angola"),
    ("AQ", "10", "Antarctica"),
    ("AR", "32", "Argentina"),
    ("AS", "16", "American Samoa"),
    ("AT", "40", "Austria"),
    ("AU", "36", "Australia"),
    ("AW", "533", "Aruba"),
    ("AX", "248", "Åland Islands"),
    ("AZ", "31", "Azerbaijan"),
    ("BA", "70", "Bosnia"),
    ("BB", "52", "Barbados"),
    ("BD", "50", "Bangladesh"),
    ("BE", "56", "Belgium"),
    ("BF", "854", "Burkina Faso"),
    ("BG", "100", "Bulgaria"),
    ("BH", "48", "Bahrain"),
    ("BI", "108", "Burundi"),
    ("BJ", "204", "Benin"),
    ("BL", "652", "St. Barthélemy"),
    ("BM", "60", "Bermuda"),
    ("BN", "96", "Brunei"),
    ("BO", "68", "Bolivia"),
    ("BQ", "535", "Caribbean Netherlands"),
    ("BR", "76", "Brazil"),
    ("BS", "44", "Bahamas"),
    ("BT", "64", "Bhutan"),
    ("BV", "74", "Bouvet Island"),
    ("BW", "72", "Botswana"),
    ("BY", "112", "Belarus"),
    ("BZ", "84", "Belize"),
    ("CA", "124", "Canada"),
    ("CC", "166", "Cocos Islands"),
    ("CD", "180", "Congo - Kinshasa"),
    ("CF", "140", "Central African Republic"),
    ("CG", "178", "Congo - Brazzaville"),
    ("CH", "756", "Switzerland"),
    ("CI", "384", "Côte d’Ivoire"),
    ("CK", "184", "Cook Islands"),
    ("CL", "152", "Chile"),
    ("CM", "120", "Cameroon"),
    ("CN", "156", "China"),
    ("CO", "170", "Colombia"),
    ("CR", "188", "Costa Rica"),
    ("CU", "192", "Cuba"),
    ("CV", "132", "Cape Verde"),
    ("CW", "531", "Curaçao"),
    ("CX", "162", "Christmas Island"),
    ("CY", "196", "Cyprus"),
    ("CZ", "203", "Czechia"),
    ("DE", "276", "Germany"),
    ("DJ", "262", "Djibouti"),
    ("DK", "208", "Denmark"),
    ("DM", "212", "Dominica"),
    ("DO", "214", "Dominican Republic"),
    ("DZ", "12", "Algeria"),
    ("EC", "218", "Ecuador"),
    ("EE", "233", "Estonia"),
    ("EG", "818", "Egypt"),
    ("EH", "732", "Western Sahara"),
    ("ER", "232", "Eritrea"),
    ("ES", "724", "Spain"),
    ("ET", "231", "Ethiopia"),
    ("FI", "246", "Finland"),
    ("FJ", "242", "Fiji"),
    ("FK", "238", "Falkland Islands"),
    ("FM", "583", "Micronesia"),
    ("FO", "234", "Faroe Islands"),
    ("FR", "250", "France"),
    ("GA", "266", "Gabon"),
    ("GB", "826", "UK"),
    ("GD", "308", "Grenada"),
    ("GE", "268", "Georgia"),
    ("GF", "254", "French Guiana"),
    ("GG", "831", "Guernsey"),
    ("GH", "288", "Ghana"),
    ("GI", "292", "Gibraltar"),
    ("GL", "304", "Greenland"),
    ("GM", "270", "Gambia"),
    ("GN", "324", "Guinea"),
    ("GP", "312", "Guadeloupe"),
    ("GQ", "226", "Equatorial Guinea"),
    ("GR", "300", "Greece"),
    ("GS", "239", "South Georgia & South Sandwich Islands"),
    ("GT", "320", "Guatemala"),
    ("GU", "316", "Guam"),
    ("GW", "624", "Guinea-Bissau"),
    ("GY", "328", "Guyana"),
    ("HK", "344", "Hong Kong"),
    ("HM", "334", "Heard & McDonald Islands"),
    ("HN", "340", "Honduras"),
    ("HR", "191", "Croatia"),
    ("HT", "332", "Haiti"),
    ("HU", "348", "Hungary"),
    ("ID", "360", "Indonesia"),
    ("IE", "372", "Ireland"),
    ("IL", "376", "Israel"),
    ("IM", "833", "Isle of Man"),
    ("IN", "356", "India"),
    ("IO", "86", "British Indian Ocean Territory"),
    ("IQ", "368", "Iraq"),
    ("IR", "364", "Iran"),
    ("IS", "352", "Iceland"),
    ("IT", "380", "Italy"),
    ("JE", "832", "Jersey"),
    ("JM", "388", "Jamaica"),
    ("JO", "400", "Jordan"),
    ("JP", "392", "Japan"),
    ("KE", "404", "Kenya"),
    ("KG", "417", "Kyrgyzstan"),
    ("KH", "116", "Cambodia"),
    ("KI", "296", "Kiribati"),
    ("KM", "174", "Comoros"),
    ("KN", "659", "St. Kitts & Nevis"),
    ("KP", "408", "North Korea"),
    ("KR", "410", "South Korea"),
    ("KW", "414", "Kuwait"),
    ("KY", "136", "Cayman Islands"),
    ("KZ", "398", "Kazakhstan"),
    ("LA", "418", "Laos"),
    ("LB", "422", "Lebanon"),
    ("LC", "662", "St. Lucia"),
    ("LI", "438", "Liechtenstein"),
    ("LK", "144", "Sri Lanka"),
    ("LR", "430", "Liberia"),
    ("LS", "426", "Lesotho"),
    ("LT", "440", "Lithuania"),
    ("LU", "442", "Luxembourg"),
    ("LV", "428", "Latvia"),
    ("LY", "434", "Libya"),
    ("MA", "504", "Morocco"),
    ("MC", "492", "Monaco"),
    ("MD", "498", "Moldova"),
    ("ME", "499", "Montenegro"),
    ("MF", "663", "St. Martin"),
    ("MG", "450", "Madagascar"),
    ("MH", "584", "Marshall Islands"),
    ("MK", "807", "North Macedonia"),
    ("ML", "466", "Mali"),
    ("MM", "104", "Myanmar"),
    ("MN", "496", "Mongolia"),
    ("MO", "446", "Macao"),
    ("MP", "580", "Northern Mariana Islands"),
    ("MQ", "474", "Martinique"),
    ("MR", "478", "Mauritania"),
    ("MS", "500", "Montserrat"),
    ("MT", "470", "Malta"),
    ("MU", "480", "Mauritius"),
    ("MV", "462", "Maldives"),
    ("MW", "454", "Malawi"),
    ("MX", "484", "Mexico"),
    ("MY", "458", "Malaysia"),
    ("MZ", "508", "Mozambique"),
    ("NA", "516", "Namibia"),
    ("NC", "540", "New Caledonia"),
    ("NE", "562", "Niger"),
    ("NF", "574", "Norfolk Island"),
    ("NG", "566", "Nigeria"),
    ("NI", "558", "Nicaragua"),
    ("NL", "528", "Netherlands"),
    ("NO", "578", "Norway"),
    ("NP", "524", "Nepal"),
    ("NR", "520", "Nauru"),
    ("NU", "570", "Niue"),
    ("NZ", "554", "New Zealand"),
    ("OM", "512", "Oman"),
    ("PA", "591", "Panama"),
    ("PE", "604", "Peru"),
    ("PF", "258", "French Polynesia"),
    ("PG", "598", "Papua New Guinea"),
    ("PH", "608", "Philippines"),
    ("PK", "586", "Pakistan"),
    ("PL", "616", "Poland"),
    ("PM", "666", "St. Pierre & Miquelon"),
    ("PN", "612", "Pitcairn"),
    ("PR", "630", "Puerto Rico"),
    ("PS", "275", "Palestine"),
    ("PT", "620", "Portugal"),
    ("PW", "585", "Palau"),
    ("PY", "600", "Paraguay"),
    ("QA", "634", "Qatar"),
    ("RE", "638", "Réunion"),
    ("RO", "642", "Romania"),
    ("RS", "688", "Serbia"),
    ("RU", "643", "Russia"),
    ("RW", "646", "Rwanda"),
    ("SA", "682", "Saudi Arabia"),
    ("SB", "90", "Solomon Islands"),
    ("SC", "690", "Seychelles"),
    ("SD", "729", "Sudan"),
    ("SE", "752", "Sweden"),
    ("SG", "702", "Singapore"),
    ("SH", "654", "St. Helena"),
    ("SI", "705", "Slovenia"),
    ("SJ", "744", "Svalbard & Jan Mayen"),
    ("SK", "703", "Slovakia"),
    ("SL", "694", "Sierra Leone"),
    ("SM", "674", "San Marino"),
    ("SN", "686", "Senegal"),
    ("SO", "706", "Somalia"),
    ("SR", "740", "Suriname"),
    ("SS", "728", "South Sudan"),
    ("ST", "678", "São Tomé & Príncipe"),
    ("SV", "222", "El Salvador"),
    ("SX", "534", "Sint Maarten"),
    ("SY", "760", "Syria"),
    ("SZ", "748", "Eswatini"),
    ("TC", "796", "Turks & Caicos Islands"),
    ("TD", "148", "Chad"),
    ("TF", "260", "French Southern Territories"),
    ("TG", "768", "Togo"),
    ("TH", "764", "Thailand"),
    ("TJ", "762", "Tajikistan"),
    ("TK", "772", "Tokelau"),
    ("TL", "626", "Timor-Leste"),
    ("TM", "795", "Turkmenistan"),
    ("TN", "788", "Tunisia"),
    ("TO", "776", "Tonga"),
    ("TR", "792", "Türkiye"),
    ("TT", "780", "Trinidad & Tobago"),
    ("TV", "798", "Tuvalu"),
    ("TW", "158", "Taiwan"),
    ("TZ", "834", "Tanzania"),
    ("UA", "804", "Ukraine"),
    ("UG", "800", "Uganda"),
    ("UM", "581", "U.S. Outlying Islands"),
    ("US", "840", "US"),
    ("UY", "858", "Uruguay"),
    ("UZ", "860", "Uzbekistan"),
    ("VA", "336", "Vatican City"),
    ("VC", "670", "St. Vincent & Grenadines"),
    ("VE", "862", "Venezuela"),
    ("VG", "92", "British Virgin Islands"),
    ("VI", "850", "U.S. Virgin Islands"),
    ("VN", "704", "Vietnam"),
    ("VU", "548", "Vanuatu"),
    ("WF", "876", "Wallis & Futuna"),
    ("WS", "882", "Samoa"),
    ("YE", "887", "Yemen"),
    ("YT", "175", "Mayotte"),
    ("ZA", "710", "South Africa"),
    ("ZM", "894", "Zambia"),
    ("ZW", "716", "Zimbabwe"),
];

/// Base directory of the flag files, relative to the repo root.
pub const FLAG_BASE_PATH: &str = "assets/icons/flags";

/// Flag file path for a country code and variant (`"4x3"` or `"1x1"`).
/// Returns `None` for an unknown code or variant.
pub fn flag_path(code: &str, variant: &str) -> Option<String> {
    let code = code.trim().to_lowercase();
    if !ISO_3166_COUNTRIES.iter().any(|(c, _, _)| *c == code.to_uppercase()) {
        return None;
    }
    match variant {
        "4x3" => Some(format!("{FLAG_BASE_PATH}/4x3/{code}.svg")),
        "1x1" => Some(format!("{FLAG_BASE_PATH}/1x1/{code}.svg")),
        _ => None,
    }
}

/// The full ISO 3166-1 country list, in code order.
pub fn builtin_countries() -> Vec<CountryInfo> {
    ISO_3166_COUNTRIES
        .iter()
        .map(|(code, numeric_code, name)| CountryInfo {
            code: (*code).to_string(),
            numeric_code: (*numeric_code).to_string(),
            name: (*name).to_string(),
            flag_4x3: format!("{FLAG_BASE_PATH}/4x3/{}.svg", code.to_lowercase()),
            flag_1x1: format!("{FLAG_BASE_PATH}/1x1/{}.svg", code.to_lowercase()),
        })
        .collect()
}

/// One country by alpha-2 code (case-insensitive, trimmed),
/// or `None` for an unknown code.
pub fn country_info(code: &str) -> Option<CountryInfo> {
    let code = code.trim().to_uppercase();
    builtin_countries()
        .into_iter()
        .find(|country| country.code == code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_the_full_iso_3166_1_set() {
        assert_eq!(ISO_3166_COUNTRIES.len(), 249);
        let mut codes: Vec<&str> = ISO_3166_COUNTRIES.iter().map(|row| row.0).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), ISO_3166_COUNTRIES.len());
        assert!(ISO_3166_COUNTRIES.windows(2).all(|pair| pair[0].0 < pair[1].0));
        assert_eq!(country_info("id").unwrap().name, "Indonesia");
        assert_eq!(country_info("US").unwrap().numeric_code, "840");
        assert_eq!(country_info("xx"), None);
    }

    #[test]
    fn every_country_has_both_flag_paths() {
        for country in builtin_countries() {
            assert_eq!(country.flag_4x3, format!("assets/icons/flags/4x3/{}.svg", country.code.to_lowercase()));
            assert_eq!(country.flag_1x1, format!("assets/icons/flags/1x1/{}.svg", country.code.to_lowercase()));
            assert_eq!(flag_path(&country.code, "4x3").unwrap(), country.flag_4x3);
            assert_eq!(flag_path(&country.code, "1x1").unwrap(), country.flag_1x1);
        }
        assert_eq!(flag_path("US", "3x2"), None);
        assert_eq!(flag_path("XX", "4x3"), None);
    }
}
