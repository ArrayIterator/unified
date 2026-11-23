//! Settings - site definitions (WordPress General Settings parity, task 351).
//!
//! Data-driven definitions for the `site` namespace: the fields
//! WordPress shows under Settings > General, as data — not a
//! hard-coded form. The generic KV store, the `settings.merge` /
//! `config.validate` hooks and the GET/PUT/DELETE API stay exactly
//! as they are; these definitions add a discovery + validation
//! layer on top: `GET /api/v1/settings/definitions?namespace=site`
//! lists them with their effective values (stored value, or the
//! compiled-in default when nothing is stored), and a
//! `config.validate` handler refuses values that do not match their
//! definition. Unknown `site/*` keys are NOT rejected — the KV store
//! stays open for modules that keep their own site keys — but a
//! value for a KNOWN key must match its type, its option list, and
//! its shape rule (URL, email, non-empty title).
//!
//! The timezone list is the full IANA list from the system tzdata
//! (420 zones including UTC); the role options mirror the seven
//! static default roles (`roles/seeds.rs`) and the language options
//! are derived live from the built-in language catalogue
//! (`i18n/languages.rs`) — the timezone options likewise from
//! `IANA_TIMEZONES` in this file. Both are generated, never
//! hand-kept literals, so a change to either source list flows
//! straight into validation and the definitions endpoint.

use super::types::SettingsStore;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::OnceLock;

/// One option of a `select` field: the stored value plus the label
/// the settings page shows. The value is a string for every select
/// except `week_starts_on`, whose values are the numbers 0-6.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SiteSettingOption {
    pub value: Value,
    pub label: String,
}

/// One field of the site settings page: key, label, description,
/// value type (`string`, `bool`, `select`), compiled-in default,
/// the option list for selects, and whether a value is required.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SiteSettingDef {
    pub key: String,
    pub label: String,
    pub description: String,
    #[serde(rename = "type")]
    pub value_type: String,
    pub default: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<SiteSettingOption>>,
    #[serde(default)]
    pub required: bool,
}

/// The full IANA timezone list (420 zones, UTC first), from the
/// system tzdata. A static list, not a directory read: the setting
/// must validate the same way on every machine, with or without
/// tzdata installed.
pub static IANA_TIMEZONES: &[&str] = &[
    "UTC",
    "Africa/Abidjan",
    "Africa/Accra",
    "Africa/Addis_Ababa",
    "Africa/Algiers",
    "Africa/Asmara",
    "Africa/Bamako",
    "Africa/Bangui",
    "Africa/Banjul",
    "Africa/Bissau",
    "Africa/Blantyre",
    "Africa/Brazzaville",
    "Africa/Bujumbura",
    "Africa/Cairo",
    "Africa/Casablanca",
    "Africa/Ceuta",
    "Africa/Conakry",
    "Africa/Dakar",
    "Africa/Dar_es_Salaam",
    "Africa/Djibouti",
    "Africa/Douala",
    "Africa/El_Aaiun",
    "Africa/Freetown",
    "Africa/Gaborone",
    "Africa/Harare",
    "Africa/Johannesburg",
    "Africa/Juba",
    "Africa/Kampala",
    "Africa/Khartoum",
    "Africa/Kigali",
    "Africa/Kinshasa",
    "Africa/Lagos",
    "Africa/Libreville",
    "Africa/Lome",
    "Africa/Luanda",
    "Africa/Lubumbashi",
    "Africa/Lusaka",
    "Africa/Malabo",
    "Africa/Maputo",
    "Africa/Maseru",
    "Africa/Mbabane",
    "Africa/Mogadishu",
    "Africa/Monrovia",
    "Africa/Nairobi",
    "Africa/Ndjamena",
    "Africa/Niamey",
    "Africa/Nouakchott",
    "Africa/Ouagadougou",
    "Africa/Porto-Novo",
    "Africa/Sao_Tome",
    "Africa/Tripoli",
    "Africa/Tunis",
    "Africa/Windhoek",
    "America/Adak",
    "America/Anchorage",
    "America/Anguilla",
    "America/Antigua",
    "America/Araguaina",
    "America/Argentina/Buenos_Aires",
    "America/Argentina/Catamarca",
    "America/Argentina/Cordoba",
    "America/Argentina/Jujuy",
    "America/Argentina/La_Rioja",
    "America/Argentina/Mendoza",
    "America/Argentina/Rio_Gallegos",
    "America/Argentina/Salta",
    "America/Argentina/San_Juan",
    "America/Argentina/San_Luis",
    "America/Argentina/Tucuman",
    "America/Argentina/Ushuaia",
    "America/Aruba",
    "America/Asuncion",
    "America/Atikokan",
    "America/Bahia",
    "America/Bahia_Banderas",
    "America/Barbados",
    "America/Belem",
    "America/Belize",
    "America/Blanc-Sablon",
    "America/Boa_Vista",
    "America/Bogota",
    "America/Boise",
    "America/Cambridge_Bay",
    "America/Campo_Grande",
    "America/Cancun",
    "America/Caracas",
    "America/Cayenne",
    "America/Cayman",
    "America/Chicago",
    "America/Chihuahua",
    "America/Ciudad_Juarez",
    "America/Costa_Rica",
    "America/Coyhaique",
    "America/Creston",
    "America/Cuiaba",
    "America/Curacao",
    "America/Danmarkshavn",
    "America/Dawson",
    "America/Dawson_Creek",
    "America/Denver",
    "America/Detroit",
    "America/Dominica",
    "America/Edmonton",
    "America/Eirunepe",
    "America/El_Salvador",
    "America/Fort_Nelson",
    "America/Fortaleza",
    "America/Glace_Bay",
    "America/Goose_Bay",
    "America/Grand_Turk",
    "America/Grenada",
    "America/Guadeloupe",
    "America/Guatemala",
    "America/Guayaquil",
    "America/Guyana",
    "America/Halifax",
    "America/Havana",
    "America/Hermosillo",
    "America/Indiana/Indianapolis",
    "America/Indiana/Knox",
    "America/Indiana/Marengo",
    "America/Indiana/Petersburg",
    "America/Indiana/Tell_City",
    "America/Indiana/Vevay",
    "America/Indiana/Vincennes",
    "America/Indiana/Winamac",
    "America/Inuvik",
    "America/Iqaluit",
    "America/Jamaica",
    "America/Juneau",
    "America/Kentucky/Louisville",
    "America/Kentucky/Monticello",
    "America/La_Paz",
    "America/Lima",
    "America/Los_Angeles",
    "America/Maceio",
    "America/Managua",
    "America/Manaus",
    "America/Martinique",
    "America/Matamoros",
    "America/Mazatlan",
    "America/Menominee",
    "America/Merida",
    "America/Metlakatla",
    "America/Mexico_City",
    "America/Miquelon",
    "America/Moncton",
    "America/Monterrey",
    "America/Montevideo",
    "America/Montserrat",
    "America/Nassau",
    "America/New_York",
    "America/Nome",
    "America/Noronha",
    "America/North_Dakota/Beulah",
    "America/North_Dakota/Center",
    "America/North_Dakota/New_Salem",
    "America/Nuuk",
    "America/Ojinaga",
    "America/Panama",
    "America/Paramaribo",
    "America/Phoenix",
    "America/Port-au-Prince",
    "America/Port_of_Spain",
    "America/Porto_Velho",
    "America/Puerto_Rico",
    "America/Punta_Arenas",
    "America/Rankin_Inlet",
    "America/Recife",
    "America/Regina",
    "America/Resolute",
    "America/Rio_Branco",
    "America/Santarem",
    "America/Santiago",
    "America/Santo_Domingo",
    "America/Sao_Paulo",
    "America/Scoresbysund",
    "America/Sitka",
    "America/St_Johns",
    "America/St_Kitts",
    "America/St_Lucia",
    "America/St_Thomas",
    "America/St_Vincent",
    "America/Swift_Current",
    "America/Tegucigalpa",
    "America/Thule",
    "America/Tijuana",
    "America/Toronto",
    "America/Tortola",
    "America/Vancouver",
    "America/Whitehorse",
    "America/Winnipeg",
    "America/Yakutat",
    "Antarctica/Casey",
    "Antarctica/Davis",
    "Antarctica/DumontDUrville",
    "Antarctica/Macquarie",
    "Antarctica/Mawson",
    "Antarctica/McMurdo",
    "Antarctica/Palmer",
    "Antarctica/Rothera",
    "Antarctica/Syowa",
    "Antarctica/Troll",
    "Antarctica/Vostok",
    "Asia/Aden",
    "Asia/Almaty",
    "Asia/Amman",
    "Asia/Anadyr",
    "Asia/Aqtau",
    "Asia/Aqtobe",
    "Asia/Ashgabat",
    "Asia/Atyrau",
    "Asia/Baghdad",
    "Asia/Bahrain",
    "Asia/Baku",
    "Asia/Bangkok",
    "Asia/Barnaul",
    "Asia/Beirut",
    "Asia/Bishkek",
    "Asia/Brunei",
    "Asia/Chita",
    "Asia/Colombo",
    "Asia/Damascus",
    "Asia/Dhaka",
    "Asia/Dili",
    "Asia/Dubai",
    "Asia/Dushanbe",
    "Asia/Famagusta",
    "Asia/Gaza",
    "Asia/Hebron",
    "Asia/Ho_Chi_Minh",
    "Asia/Hong_Kong",
    "Asia/Hovd",
    "Asia/Irkutsk",
    "Asia/Jakarta",
    "Asia/Jayapura",
    "Asia/Jerusalem",
    "Asia/Kabul",
    "Asia/Kamchatka",
    "Asia/Karachi",
    "Asia/Kathmandu",
    "Asia/Khandyga",
    "Asia/Kolkata",
    "Asia/Krasnoyarsk",
    "Asia/Kuala_Lumpur",
    "Asia/Kuching",
    "Asia/Kuwait",
    "Asia/Macau",
    "Asia/Magadan",
    "Asia/Makassar",
    "Asia/Manila",
    "Asia/Muscat",
    "Asia/Nicosia",
    "Asia/Novokuznetsk",
    "Asia/Novosibirsk",
    "Asia/Omsk",
    "Asia/Oral",
    "Asia/Phnom_Penh",
    "Asia/Pontianak",
    "Asia/Pyongyang",
    "Asia/Qatar",
    "Asia/Qostanay",
    "Asia/Qyzylorda",
    "Asia/Riyadh",
    "Asia/Sakhalin",
    "Asia/Samarkand",
    "Asia/Seoul",
    "Asia/Shanghai",
    "Asia/Singapore",
    "Asia/Srednekolymsk",
    "Asia/Taipei",
    "Asia/Tashkent",
    "Asia/Tbilisi",
    "Asia/Tehran",
    "Asia/Thimphu",
    "Asia/Tokyo",
    "Asia/Tomsk",
    "Asia/Ulaanbaatar",
    "Asia/Urumqi",
    "Asia/Ust-Nera",
    "Asia/Vientiane",
    "Asia/Vladivostok",
    "Asia/Yakutsk",
    "Asia/Yangon",
    "Asia/Yekaterinburg",
    "Asia/Yerevan",
    "Atlantic/Azores",
    "Atlantic/Bermuda",
    "Atlantic/Canary",
    "Atlantic/Cape_Verde",
    "Atlantic/Faroe",
    "Atlantic/Madeira",
    "Atlantic/Reykjavik",
    "Atlantic/South_Georgia",
    "Atlantic/St_Helena",
    "Atlantic/Stanley",
    "Australia/Adelaide",
    "Australia/Brisbane",
    "Australia/Broken_Hill",
    "Australia/Darwin",
    "Australia/Eucla",
    "Australia/Hobart",
    "Australia/Lindeman",
    "Australia/Lord_Howe",
    "Australia/Melbourne",
    "Australia/Perth",
    "Australia/Sydney",
    "CET",
    "CST6CDT",
    "EET",
    "EST",
    "EST5EDT",
    "Europe/Amsterdam",
    "Europe/Andorra",
    "Europe/Astrakhan",
    "Europe/Athens",
    "Europe/Belgrade",
    "Europe/Berlin",
    "Europe/Brussels",
    "Europe/Bucharest",
    "Europe/Budapest",
    "Europe/Chisinau",
    "Europe/Copenhagen",
    "Europe/Dublin",
    "Europe/Gibraltar",
    "Europe/Guernsey",
    "Europe/Helsinki",
    "Europe/Isle_of_Man",
    "Europe/Istanbul",
    "Europe/Jersey",
    "Europe/Kaliningrad",
    "Europe/Kirov",
    "Europe/Kyiv",
    "Europe/Lisbon",
    "Europe/Ljubljana",
    "Europe/London",
    "Europe/Luxembourg",
    "Europe/Madrid",
    "Europe/Malta",
    "Europe/Minsk",
    "Europe/Monaco",
    "Europe/Moscow",
    "Europe/Oslo",
    "Europe/Paris",
    "Europe/Prague",
    "Europe/Riga",
    "Europe/Rome",
    "Europe/Samara",
    "Europe/Sarajevo",
    "Europe/Saratov",
    "Europe/Simferopol",
    "Europe/Skopje",
    "Europe/Sofia",
    "Europe/Stockholm",
    "Europe/Tallinn",
    "Europe/Tirane",
    "Europe/Ulyanovsk",
    "Europe/Vaduz",
    "Europe/Vienna",
    "Europe/Vilnius",
    "Europe/Volgograd",
    "Europe/Warsaw",
    "Europe/Zagreb",
    "Europe/Zurich",
    "Factory",
    "HST",
    "Indian/Antananarivo",
    "Indian/Chagos",
    "Indian/Christmas",
    "Indian/Cocos",
    "Indian/Comoro",
    "Indian/Kerguelen",
    "Indian/Mahe",
    "Indian/Maldives",
    "Indian/Mauritius",
    "Indian/Mayotte",
    "Indian/Reunion",
    "MET",
    "MST",
    "MST7MDT",
    "PST8PDT",
    "Pacific/Apia",
    "Pacific/Auckland",
    "Pacific/Bougainville",
    "Pacific/Chatham",
    "Pacific/Chuuk",
    "Pacific/Easter",
    "Pacific/Efate",
    "Pacific/Fakaofo",
    "Pacific/Fiji",
    "Pacific/Funafuti",
    "Pacific/Galapagos",
    "Pacific/Gambier",
    "Pacific/Guadalcanal",
    "Pacific/Guam",
    "Pacific/Honolulu",
    "Pacific/Kanton",
    "Pacific/Kiritimati",
    "Pacific/Kosrae",
    "Pacific/Kwajalein",
    "Pacific/Majuro",
    "Pacific/Marquesas",
    "Pacific/Midway",
    "Pacific/Nauru",
    "Pacific/Niue",
    "Pacific/Norfolk",
    "Pacific/Noumea",
    "Pacific/Pago_Pago",
    "Pacific/Palau",
    "Pacific/Pitcairn",
    "Pacific/Pohnpei",
    "Pacific/Port_Moresby",
    "Pacific/Rarotonga",
    "Pacific/Saipan",
    "Pacific/Tahiti",
    "Pacific/Tarawa",
    "Pacific/Tongatapu",
    "Pacific/Wake",
    "Pacific/Wallis",
    "WET",
];

/// Option list for the `timezone` setting, derived from the single
/// IANA source (`IANA_TIMEZONES`): value and label are the zone name.
fn timezone_options() -> Vec<SiteSettingOption> {
    IANA_TIMEZONES
        .iter()
        .map(|zone| SiteSettingOption {
            value: json!(zone),
            label: (*zone).to_string(),
        })
        .collect()
}

/// Option list for the `site_language` setting, derived from the
/// single i18n language catalogue (`i18n::languages`). The label is
/// the English name, with the native name in parentheses when it
/// differs ("Indonesian (Bahasa Indonesia)", plain "English").
fn language_options() -> Vec<SiteSettingOption> {
    crate::modules::i18n::languages::builtin_languages()
        .into_iter()
        .map(|language| SiteSettingOption {
            value: json!(language.code),
            label: if language.name == language.native_name {
                language.name
            } else {
                format!("{} ({})", language.name, language.native_name)
            },
        })
        .collect()
}

fn build_site_definitions() -> Vec<SiteSettingDef> {
    let mut definitions = vec![
        SiteSettingDef {
            key: "site_title".to_string(),
            label: "Site title".to_string(),
            description: "The name of the site, shown in the header and the browser title.".to_string(),
            value_type: "string".to_string(),
            default: json!("Unified"),
            options: None,
            required: true
        },
        SiteSettingDef {
            key: "tagline".to_string(),
            label: "Tagline".to_string(),
            description: "A short description of the site, shown under the title where the theme supports it.".to_string(),
            value_type: "string".to_string(),
            default: json!(""),
            options: None,
            required: false,
        },
        SiteSettingDef {
            key: "site_url".to_string(),
            label: "Site URL".to_string(),
            description: "The address of the site (WordPress Address). Must start with http:// or https://.".to_string(),
            value_type: "string".to_string(),
            default: json!(""),
            options: None,
            required: false,
        },
        SiteSettingDef {
            key: "home_url".to_string(),
            label: "Home URL".to_string(),
            description: "The address of the site front page (Site Address). Must start with http:// or https://.".to_string(),
            value_type: "string".to_string(),
            default: json!(""),
            options: None,
            required: false,
        },
        SiteSettingDef {
            key: "admin_email".to_string(),
            label: "Administration email".to_string(),
            description: "The address used for administration notices and password resets.".to_string(),
            value_type: "string".to_string(),
            default: json!(""),
            options: None,
            required: false,
        },
        SiteSettingDef {
            key: "users_can_register".to_string(),
            label: "Anyone can register".to_string(),
            description: "Whether anyone can register an account on the site (membership).".to_string(),
            value_type: "bool".to_string(),
            default: json!(false),
            options: None,
            required: false,
        },
        SiteSettingDef {
            key: "default_role".to_string(),
            label: "New user default role".to_string(),
            description: "The role assigned to a newly registered user. Mirrors the seven static default roles (roles/seeds.rs).".to_string(),
            value_type: "select".to_string(),
            default: json!("member"),
            options: Some(vec![SiteSettingOption { value: json!("admin"), label: "Administrator".to_string() }, SiteSettingOption { value: json!("editor"), label: "Editor".to_string() }, SiteSettingOption { value: json!("author"), label: "Author".to_string() }, SiteSettingOption { value: json!("member"), label: "Member".to_string() }, SiteSettingOption { value: json!("customer"), label: "Customer".to_string() }, SiteSettingOption { value: json!("reseller"), label: "Reseller".to_string() }, SiteSettingOption { value: json!("finance"), label: "Finance".to_string() }]),
            required: false,
        },
        SiteSettingDef {
            key: "site_language".to_string(),
            label: "Site language".to_string(),
            description: "The language of the admin interface and the site. Mirrors the built-in language catalogue (i18n/languages.rs).".to_string(),
            value_type: "select".to_string(),
            default: json!("en"),
            options: None,
            required: false,
        },
        SiteSettingDef {
            key: "timezone".to_string(),
            label: "Timezone".to_string(),
            description: "The site timezone, as a full IANA zone name. Used for every date the site displays.".to_string(),
            value_type: "select".to_string(),
            default: json!("UTC"),
            options: None,
            required: false,
        },
        SiteSettingDef {
            key: "date_format".to_string(),
            label: "Date format".to_string(),
            description: "How dates are displayed, in PHP date-format characters.".to_string(),
            value_type: "select".to_string(),
            default: json!("F j, Y"),
            options: Some(vec![SiteSettingOption { value: json!("F j, Y"), label: "November 23, 2026".to_string() }, SiteSettingOption { value: json!("Y-m-d"), label: "2026-11-23".to_string() }, SiteSettingOption { value: json!("m/d/Y"), label: "11/23/2026".to_string() }, SiteSettingOption { value: json!("d/m/Y"), label: "23/11/2026".to_string() }]),
            required: false,
        },
        SiteSettingDef {
            key: "time_format".to_string(),
            label: "Time format".to_string(),
            description: "How times are displayed, in PHP date-format characters.".to_string(),
            value_type: "select".to_string(),
            default: json!("g:i a"),
            options: Some(vec![SiteSettingOption { value: json!("g:i a"), label: "1:05 pm".to_string() }, SiteSettingOption { value: json!("g:i A"), label: "1:05 PM".to_string() }, SiteSettingOption { value: json!("H:i"), label: "13:05".to_string() }]),
            required: false,
        },
        SiteSettingDef {
            key: "week_starts_on".to_string(),
            label: "Week starts on".to_string(),
            description: "The first day of the week, used by calendars. WordPress default: Monday.".to_string(),
            value_type: "select".to_string(),
            default: json!(1),
            options: Some(vec![SiteSettingOption { value: json!(0), label: "Sunday".to_string() }, SiteSettingOption { value: json!(1), label: "Monday".to_string() }, SiteSettingOption { value: json!(2), label: "Tuesday".to_string() }, SiteSettingOption { value: json!(3), label: "Wednesday".to_string() }, SiteSettingOption { value: json!(4), label: "Thursday".to_string() }, SiteSettingOption { value: json!(5), label: "Friday".to_string() }, SiteSettingOption { value: json!(6), label: "Saturday".to_string() }]),
            required: false,
        },
    ];
    // The timezone and language option lists are derived from their
    // single sources -- the IANA list in this file and the i18n
    // language catalogue -- never hand-kept literals (task 361). The
    // literal above leaves both `options` as `None`; they are filled
    // in here so validation and the definitions endpoint always agree
    // with the source lists.
    for definition in &mut definitions {
        match definition.key.as_str() {
            "timezone" => definition.options = Some(timezone_options()),
            "site_language" => definition.options = Some(language_options()),
            _ => {}
        }
    }
    definitions
}

static SITE_DEFINITIONS: OnceLock<Vec<SiteSettingDef>> = OnceLock::new();

/// The site namespace definitions, in display order.
pub fn site_definitions() -> &'static [SiteSettingDef] {
    SITE_DEFINITIONS.get_or_init(build_site_definitions)
}

/// One definition by key, or `None` for a key this module does not
/// define (the KV store stays open for those).
pub fn site_definition(key: &str) -> Option<&'static SiteSettingDef> {
    site_definitions().iter().find(|def| def.key == key)
}

/// Validate a value for a site key. Unknown keys pass (not our
/// business); known keys must match their type, their option list,
/// and their shape rule. The error names the rule that failed, never
/// the stored value.
pub fn validate_site_value(key: &str, value: &Value) -> Result<(), String> {
    let Some(def) = site_definition(key) else {
        return Ok(());
    };
    match def.value_type.as_str() {
        "string" | "select" => {
            if def.value_type == "string" && !value.is_string() {
                return Err(format!("\"{key}\" must be a string"));
            }
            if def.value_type == "select" && !value.is_string() && !value.is_number() {
                return Err(format!("\"{key}\" must be a string or a number"));
            }
        }
        "bool" => {
            if !value.is_boolean() {
                return Err(format!("\"{key}\" must be true or false"));
            }
        }
        _ => {}
    }
    if let Some(options) = &def.options {
        if !options.iter().any(|option| option.value == *value) {
            return Err(format!("\"{key}\" must be one of the listed options"));
        }
    }
    match key {
        "site_title" => {
            if value.as_str().map(str::trim).unwrap_or("").is_empty() {
                return Err("site_title must not be empty".to_string());
            }
        }
        "site_url" | "home_url" => {
            if let Some(url) = value.as_str().filter(|value| !value.trim().is_empty()) {
                let parsed = url::Url::parse(url).map_err(|_| {
                    format!("\"{key}\" must be a valid URL starting with http:// or https://")
                })?;
                if !matches!(parsed.scheme(), "http" | "https") {
                    return Err(format!("\"{key}\" must start with http:// or https://"));
                }
            }
        }
        "admin_email" => {
            if let Some(email) = value.as_str().filter(|value| !value.trim().is_empty()) {
                let valid = {
                    let parts: Vec<&str> = email.split('@').collect();
                    parts.len() == 2
                        && !parts[0].is_empty()
                        && parts[1].contains('.')
                        && !parts[1].starts_with('.')
                        && !parts[1].ends_with('.')
                };
                if !valid {
                    return Err("admin_email must be a valid email address".to_string());
                }
            }
        }
        _ => {}
    }
    Ok(())
}

/// The site definitions as JSON for the definitions endpoint, each
/// with its effective value: the stored value when one exists, the
/// compiled-in default otherwise.
pub fn site_definitions_json(store: &SettingsStore) -> Value {
    let settings: Vec<Value> = site_definitions()
        .iter()
        .map(|def| {
            let value = store
                .get(&("site".to_string(), def.key.clone()))
                .map(|record| record.value.clone())
                .unwrap_or_else(|| def.default.clone());
            json!({
                "namespace": "site",
                "key": def.key,
                "label": def.label,
                "description": def.description,
                "type": def.value_type,
                "default": def.default,
                "options": def.options,
                "required": def.required,
                "value": value,
            })
        })
        .collect();
    json!({ "namespace": "site", "settings": settings })
}

#[cfg(test)]
mod tests {
    use super::*;
    use dashmap::DashMap;
    use std::sync::Arc;

    #[test]
    fn all_twelve_site_keys_are_defined_once() {
        let defs = site_definitions();
        assert_eq!(defs.len(), 12);
        let mut keys: Vec<&str> = defs.iter().map(|def| def.key.as_str()).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), 12);
        for key in [
            "site_title",
            "tagline",
            "site_url",
            "home_url",
            "admin_email",
            "users_can_register",
            "default_role",
            "site_language",
            "timezone",
            "date_format",
            "time_format",
            "week_starts_on",
        ] {
            assert!(site_definition(key).is_some(), "missing site key {key}");
        }
    }

    #[test]
    fn defaults_are_valid_and_complete() {
        for def in site_definitions() {
            assert!(
                validate_site_value(&def.key, &def.default).is_ok(),
                "default for {} fails validation",
                def.key
            );
        }
        assert_eq!(
            site_definition("site_title").unwrap().default,
            json!("Unified")
        );
        assert_eq!(
            site_definition("default_role").unwrap().default,
            json!("member")
        );
        assert_eq!(
            site_definition("site_language").unwrap().default,
            json!("en")
        );
        assert_eq!(site_definition("timezone").unwrap().default, json!("UTC"));
        assert_eq!(site_definition("week_starts_on").unwrap().default, json!(1));
    }

    #[test]
    fn timezone_list_is_the_full_iana_list() {
        assert_eq!(IANA_TIMEZONES.len(), 420);
        assert!(IANA_TIMEZONES.contains(&"UTC"));
        assert!(IANA_TIMEZONES.contains(&"Asia/Jakarta"));
        assert!(IANA_TIMEZONES.contains(&"America/New_York"));
        assert!(IANA_TIMEZONES.contains(&"Europe/London"));
        assert!(IANA_TIMEZONES.contains(&"Australia/Sydney"));
        let options = site_definition("timezone")
            .unwrap()
            .options
            .as_ref()
            .unwrap();
        assert_eq!(options.len(), 420);
    }

    #[test]
    fn role_and_language_options_mirror_their_sources() {
        let roles = site_definition("default_role")
            .unwrap()
            .options
            .as_ref()
            .unwrap();
        assert_eq!(roles.len(), 7);
        assert!(roles.iter().any(|option| option.value == json!("member")));
        assert!(roles.iter().any(|option| option.value == json!("admin")));
        let languages = site_definition("site_language")
            .unwrap()
            .options
            .as_ref()
            .unwrap();
        // Mirrors the built-in catalogue exactly: same codes, same
        // count, same order. The catalogue grew from 12 to 42
        // languages (task 361); this test pins the derivation, not
        // the number.
        assert_eq!(
            languages.len(),
            crate::modules::i18n::languages::builtin_languages().len()
        );
        for language in crate::modules::i18n::languages::builtin_languages() {
            assert!(
                languages
                    .iter()
                    .any(|option| option.value == json!(language.code.as_str())),
                "language {} missing from site_language options",
                language.code
            );
        }
        assert!(languages.iter().any(|option| option.value == json!("id")));
        assert!(languages
            .iter()
            .any(|option| option.label == "Indonesian (Bahasa Indonesia)"));
    }

    #[test]
    fn validation_rejects_bad_values() {
        assert!(validate_site_value("site_title", &json!("")).is_err());
        assert!(validate_site_value("site_title", &json!(42)).is_err());
        assert!(validate_site_value("site_url", &json!("not a url")).is_err());
        assert!(validate_site_value("site_url", &json!("ftp://example.com")).is_err());
        assert!(validate_site_value("site_url", &json!("https://unified.example")).is_ok());
        assert!(validate_site_value("site_url", &json!("")).is_ok());
        assert!(validate_site_value("admin_email", &json!("no-at-sign")).is_err());
        assert!(validate_site_value("admin_email", &json!("admin@unified.example")).is_ok());
        assert!(validate_site_value("timezone", &json!("Mars/Olympus")).is_err());
        assert!(validate_site_value("timezone", &json!("Asia/Jakarta")).is_ok());
        assert!(validate_site_value("default_role", &json!("superadmin")).is_err());
        assert!(validate_site_value("week_starts_on", &json!(7)).is_err());
        assert!(validate_site_value("week_starts_on", &json!(0)).is_ok());
        assert!(validate_site_value("date_format", &json!("Y")).is_err());
        assert!(validate_site_value("users_can_register", &json!("yes")).is_err());
        assert!(validate_site_value("users_can_register", &json!(true)).is_ok());
        // Unknown site keys pass: the KV store stays open.
        assert!(validate_site_value("some_other_key", &json!("anything")).is_ok());
    }

    #[test]
    fn definitions_json_prefers_the_stored_value() {
        let store: SettingsStore = Arc::new(DashMap::new());
        let doc = site_definitions_json(&store);
        let settings = doc.get("settings").unwrap().as_array().unwrap();
        assert_eq!(settings.len(), 12);
        let title = settings
            .iter()
            .find(|item| item.get("key").unwrap() == "site_title")
            .unwrap();
        assert_eq!(title.get("value").unwrap(), &json!("Unified"));
        store.insert(
            ("site".to_string(), "site_title".to_string()),
            crate::modules::settings::SettingRecord {
                namespace: "site".to_string(),
                key: "site_title".to_string(),
                value_type: "string".to_string(),
                value: json!("My Site"),
                updated_at: chrono::Utc::now(),
            },
        );
        let doc = site_definitions_json(&store);
        let settings = doc.get("settings").unwrap().as_array().unwrap();
        let title = settings
            .iter()
            .find(|item| item.get("key").unwrap() == "site_title")
            .unwrap();
        assert_eq!(title.get("value").unwrap(), &json!("My Site"));
    }
}
