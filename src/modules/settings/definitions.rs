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
//! mirror the built-in language catalogue (`i18n/languages.rs`).
//! If those lists change, the definitions here must change with
//! them — they are data, kept in sync by hand, not read live.

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

fn build_site_definitions() -> Vec<SiteSettingDef> {
    vec![
        SiteSettingDef {
            key: "site_title".to_string(),
            label: "Site title".to_string(),
            description: "The name of the site, shown in the header and the browser title.".to_string(),
            value_type: "string".to_string(),
            default: json!("Unified"),,
            required: true
        },
        SiteSettingDef {
            key: "tagline".to_string(),
            label: "Tagline".to_string(),
            description: "A short description of the site, shown under the title where the theme supports it.".to_string(),
            value_type: "string".to_string(),
            default: json!(""),
        },
        SiteSettingDef {
            key: "site_url".to_string(),
            label: "Site URL".to_string(),
            description: "The address of the site (WordPress Address). Must start with http:// or https://.".to_string(),
            value_type: "string".to_string(),
            default: json!(""),
        },
        SiteSettingDef {
            key: "home_url".to_string(),
            label: "Home URL".to_string(),
            description: "The address of the site front page (Site Address). Must start with http:// or https://.".to_string(),
            value_type: "string".to_string(),
            default: json!(""),
        },
        SiteSettingDef {
            key: "admin_email".to_string(),
            label: "Administration email".to_string(),
            description: "The address used for administration notices and password resets.".to_string(),
            value_type: "string".to_string(),
            default: json!(""),
        },
        SiteSettingDef {
            key: "users_can_register".to_string(),
            label: "Anyone can register".to_string(),
            description: "Whether anyone can register an account on the site (membership).".to_string(),
            value_type: "bool".to_string(),
            default: json!(false),
        },
        SiteSettingDef {
            key: "default_role".to_string(),
            label: "New user default role".to_string(),
            description: "The role assigned to a newly registered user. Mirrors the seven static default roles (roles/seeds.rs).".to_string(),
            value_type: "select".to_string(),
            default: json!("member"),,
            options: Some(vec![SiteSettingOption { value: json!("admin"), label: "Administrator".to_string() }, SiteSettingOption { value: json!("editor"), label: "Editor".to_string() }, SiteSettingOption { value: json!("author"), label: "Author".to_string() }, SiteSettingOption { value: json!("member"), label: "Member".to_string() }, SiteSettingOption { value: json!("customer"), label: "Customer".to_string() }, SiteSettingOption { value: json!("reseller"), label: "Reseller".to_string() }, SiteSettingOption { value: json!("finance"), label: "Finance".to_string() }]),
        },
        SiteSettingDef {
            key: "site_language".to_string(),
            label: "Site language".to_string(),
            description: "The language of the admin interface and the site. Mirrors the built-in language catalogue (i18n/languages.rs).".to_string(),
            value_type: "select".to_string(),
            default: json!("en"),,
            options: Some(vec![SiteSettingOption { value: json!("en"), label: "English".to_string() }, SiteSettingOption { value: json!("id"), label: "Indonesian (Bahasa Indonesia)".to_string() }, SiteSettingOption { value: json!("ar"), label: "Arabic".to_string() }, SiteSettingOption { value: json!("zh"), label: "Chinese".to_string() }, SiteSettingOption { value: json!("es"), label: "Spanish".to_string() }, SiteSettingOption { value: json!("fr"), label: "French".to_string() }, SiteSettingOption { value: json!("de"), label: "German".to_string() }, SiteSettingOption { value: json!("pt"), label: "Portuguese".to_string() }, SiteSettingOption { value: json!("ja"), label: "Japanese".to_string() }, SiteSettingOption { value: json!("ko"), label: "Korean".to_string() }, SiteSettingOption { value: json!("nl"), label: "Dutch".to_string() }, SiteSettingOption { value: json!("ru"), label: "Russian".to_string() }]),
        },
        SiteSettingDef {
            key: "timezone".to_string(),
            label: "Timezone".to_string(),
            description: "The site timezone, as a full IANA zone name. Used for every date the site displays.".to_string(),
            value_type: "select".to_string(),
            default: json!("UTC"),,
            options: Some(vec![SiteSettingOption { value: json!("UTC"), label: "UTC".to_string() }, SiteSettingOption { value: json!("Africa/Abidjan"), label: "Africa/Abidjan".to_string() }, SiteSettingOption { value: json!("Africa/Accra"), label: "Africa/Accra".to_string() }, SiteSettingOption { value: json!("Africa/Addis_Ababa"), label: "Africa/Addis_Ababa".to_string() }, SiteSettingOption { value: json!("Africa/Algiers"), label: "Africa/Algiers".to_string() }, SiteSettingOption { value: json!("Africa/Asmara"), label: "Africa/Asmara".to_string() }, SiteSettingOption { value: json!("Africa/Bamako"), label: "Africa/Bamako".to_string() }, SiteSettingOption { value: json!("Africa/Bangui"), label: "Africa/Bangui".to_string() }, SiteSettingOption { value: json!("Africa/Banjul"), label: "Africa/Banjul".to_string() }, SiteSettingOption { value: json!("Africa/Bissau"), label: "Africa/Bissau".to_string() }, SiteSettingOption { value: json!("Africa/Blantyre"), label: "Africa/Blantyre".to_string() }, SiteSettingOption { value: json!("Africa/Brazzaville"), label: "Africa/Brazzaville".to_string() }, SiteSettingOption { value: json!("Africa/Bujumbura"), label: "Africa/Bujumbura".to_string() }, SiteSettingOption { value: json!("Africa/Cairo"), label: "Africa/Cairo".to_string() }, SiteSettingOption { value: json!("Africa/Casablanca"), label: "Africa/Casablanca".to_string() }, SiteSettingOption { value: json!("Africa/Ceuta"), label: "Africa/Ceuta".to_string() }, SiteSettingOption { value: json!("Africa/Conakry"), label: "Africa/Conakry".to_string() }, SiteSettingOption { value: json!("Africa/Dakar"), label: "Africa/Dakar".to_string() }, SiteSettingOption { value: json!("Africa/Dar_es_Salaam"), label: "Africa/Dar_es_Salaam".to_string() }, SiteSettingOption { value: json!("Africa/Djibouti"), label: "Africa/Djibouti".to_string() }, SiteSettingOption { value: json!("Africa/Douala"), label: "Africa/Douala".to_string() }, SiteSettingOption { value: json!("Africa/El_Aaiun"), label: "Africa/El_Aaiun".to_string() }, SiteSettingOption { value: json!("Africa/Freetown"), label: "Africa/Freetown".to_string() }, SiteSettingOption { value: json!("Africa/Gaborone"), label: "Africa/Gaborone".to_string() }, SiteSettingOption { value: json!("Africa/Harare"), label: "Africa/Harare".to_string() }, SiteSettingOption { value: json!("Africa/Johannesburg"), label: "Africa/Johannesburg".to_string() }, SiteSettingOption { value: json!("Africa/Juba"), label: "Africa/Juba".to_string() }, SiteSettingOption { value: json!("Africa/Kampala"), label: "Africa/Kampala".to_string() }, SiteSettingOption { value: json!("Africa/Khartoum"), label: "Africa/Khartoum".to_string() }, SiteSettingOption { value: json!("Africa/Kigali"), label: "Africa/Kigali".to_string() }, SiteSettingOption { value: json!("Africa/Kinshasa"), label: "Africa/Kinshasa".to_string() }, SiteSettingOption { value: json!("Africa/Lagos"), label: "Africa/Lagos".to_string() }, SiteSettingOption { value: json!("Africa/Libreville"), label: "Africa/Libreville".to_string() }, SiteSettingOption { value: json!("Africa/Lome"), label: "Africa/Lome".to_string() }, SiteSettingOption { value: json!("Africa/Luanda"), label: "Africa/Luanda".to_string() }, SiteSettingOption { value: json!("Africa/Lubumbashi"), label: "Africa/Lubumbashi".to_string() }, SiteSettingOption { value: json!("Africa/Lusaka"), label: "Africa/Lusaka".to_string() }, SiteSettingOption { value: json!("Africa/Malabo"), label: "Africa/Malabo".to_string() }, SiteSettingOption { value: json!("Africa/Maputo"), label: "Africa/Maputo".to_string() }, SiteSettingOption { value: json!("Africa/Maseru"), label: "Africa/Maseru".to_string() }, SiteSettingOption { value: json!("Africa/Mbabane"), label: "Africa/Mbabane".to_string() }, SiteSettingOption { value: json!("Africa/Mogadishu"), label: "Africa/Mogadishu".to_string() }, SiteSettingOption { value: json!("Africa/Monrovia"), label: "Africa/Monrovia".to_string() }, SiteSettingOption { value: json!("Africa/Nairobi"), label: "Africa/Nairobi".to_string() }, SiteSettingOption { value: json!("Africa/Ndjamena"), label: "Africa/Ndjamena".to_string() }, SiteSettingOption { value: json!("Africa/Niamey"), label: "Africa/Niamey".to_string() }, SiteSettingOption { value: json!("Africa/Nouakchott"), label: "Africa/Nouakchott".to_string() }, SiteSettingOption { value: json!("Africa/Ouagadougou"), label: "Africa/Ouagadougou".to_string() }, SiteSettingOption { value: json!("Africa/Porto-Novo"), label: "Africa/Porto-Novo".to_string() }, SiteSettingOption { value: json!("Africa/Sao_Tome"), label: "Africa/Sao_Tome".to_string() }, SiteSettingOption { value: json!("Africa/Tripoli"), label: "Africa/Tripoli".to_string() }, SiteSettingOption { value: json!("Africa/Tunis"), label: "Africa/Tunis".to_string() }, SiteSettingOption { value: json!("Africa/Windhoek"), label: "Africa/Windhoek".to_string() }, SiteSettingOption { value: json!("America/Adak"), label: "America/Adak".to_string() }, SiteSettingOption { value: json!("America/Anchorage"), label: "America/Anchorage".to_string() }, SiteSettingOption { value: json!("America/Anguilla"), label: "America/Anguilla".to_string() }, SiteSettingOption { value: json!("America/Antigua"), label: "America/Antigua".to_string() }, SiteSettingOption { value: json!("America/Araguaina"), label: "America/Araguaina".to_string() }, SiteSettingOption { value: json!("America/Argentina/Buenos_Aires"), label: "America/Argentina/Buenos_Aires".to_string() }, SiteSettingOption { value: json!("America/Argentina/Catamarca"), label: "America/Argentina/Catamarca".to_string() }, SiteSettingOption { value: json!("America/Argentina/Cordoba"), label: "America/Argentina/Cordoba".to_string() }, SiteSettingOption { value: json!("America/Argentina/Jujuy"), label: "America/Argentina/Jujuy".to_string() }, SiteSettingOption { value: json!("America/Argentina/La_Rioja"), label: "America/Argentina/La_Rioja".to_string() }, SiteSettingOption { value: json!("America/Argentina/Mendoza"), label: "America/Argentina/Mendoza".to_string() }, SiteSettingOption { value: json!("America/Argentina/Rio_Gallegos"), label: "America/Argentina/Rio_Gallegos".to_string() }, SiteSettingOption { value: json!("America/Argentina/Salta"), label: "America/Argentina/Salta".to_string() }, SiteSettingOption { value: json!("America/Argentina/San_Juan"), label: "America/Argentina/San_Juan".to_string() }, SiteSettingOption { value: json!("America/Argentina/San_Luis"), label: "America/Argentina/San_Luis".to_string() }, SiteSettingOption { value: json!("America/Argentina/Tucuman"), label: "America/Argentina/Tucuman".to_string() }, SiteSettingOption { value: json!("America/Argentina/Ushuaia"), label: "America/Argentina/Ushuaia".to_string() }, SiteSettingOption { value: json!("America/Aruba"), label: "America/Aruba".to_string() }, SiteSettingOption { value: json!("America/Asuncion"), label: "America/Asuncion".to_string() }, SiteSettingOption { value: json!("America/Atikokan"), label: "America/Atikokan".to_string() }, SiteSettingOption { value: json!("America/Bahia"), label: "America/Bahia".to_string() }, SiteSettingOption { value: json!("America/Bahia_Banderas"), label: "America/Bahia_Banderas".to_string() }, SiteSettingOption { value: json!("America/Barbados"), label: "America/Barbados".to_string() }, SiteSettingOption { value: json!("America/Belem"), label: "America/Belem".to_string() }, SiteSettingOption { value: json!("America/Belize"), label: "America/Belize".to_string() }, SiteSettingOption { value: json!("America/Blanc-Sablon"), label: "America/Blanc-Sablon".to_string() }, SiteSettingOption { value: json!("America/Boa_Vista"), label: "America/Boa_Vista".to_string() }, SiteSettingOption { value: json!("America/Bogota"), label: "America/Bogota".to_string() }, SiteSettingOption { value: json!("America/Boise"), label: "America/Boise".to_string() }, SiteSettingOption { value: json!("America/Cambridge_Bay"), label: "America/Cambridge_Bay".to_string() }, SiteSettingOption { value: json!("America/Campo_Grande"), label: "America/Campo_Grande".to_string() }, SiteSettingOption { value: json!("America/Cancun"), label: "America/Cancun".to_string() }, SiteSettingOption { value: json!("America/Caracas"), label: "America/Caracas".to_string() }, SiteSettingOption { value: json!("America/Cayenne"), label: "America/Cayenne".to_string() }, SiteSettingOption { value: json!("America/Cayman"), label: "America/Cayman".to_string() }, SiteSettingOption { value: json!("America/Chicago"), label: "America/Chicago".to_string() }, SiteSettingOption { value: json!("America/Chihuahua"), label: "America/Chihuahua".to_string() }, SiteSettingOption { value: json!("America/Ciudad_Juarez"), label: "America/Ciudad_Juarez".to_string() }, SiteSettingOption { value: json!("America/Costa_Rica"), label: "America/Costa_Rica".to_string() }, SiteSettingOption { value: json!("America/Coyhaique"), label: "America/Coyhaique".to_string() }, SiteSettingOption { value: json!("America/Creston"), label: "America/Creston".to_string() }, SiteSettingOption { value: json!("America/Cuiaba"), label: "America/Cuiaba".to_string() }, SiteSettingOption { value: json!("America/Curacao"), label: "America/Curacao".to_string() }, SiteSettingOption { value: json!("America/Danmarkshavn"), label: "America/Danmarkshavn".to_string() }, SiteSettingOption { value: json!("America/Dawson"), label: "America/Dawson".to_string() }, SiteSettingOption { value: json!("America/Dawson_Creek"), label: "America/Dawson_Creek".to_string() }, SiteSettingOption { value: json!("America/Denver"), label: "America/Denver".to_string() }, SiteSettingOption { value: json!("America/Detroit"), label: "America/Detroit".to_string() }, SiteSettingOption { value: json!("America/Dominica"), label: "America/Dominica".to_string() }, SiteSettingOption { value: json!("America/Edmonton"), label: "America/Edmonton".to_string() }, SiteSettingOption { value: json!("America/Eirunepe"), label: "America/Eirunepe".to_string() }, SiteSettingOption { value: json!("America/El_Salvador"), label: "America/El_Salvador".to_string() }, SiteSettingOption { value: json!("America/Fort_Nelson"), label: "America/Fort_Nelson".to_string() }, SiteSettingOption { value: json!("America/Fortaleza"), label: "America/Fortaleza".to_string() }, SiteSettingOption { value: json!("America/Glace_Bay"), label: "America/Glace_Bay".to_string() }, SiteSettingOption { value: json!("America/Goose_Bay"), label: "America/Goose_Bay".to_string() }, SiteSettingOption { value: json!("America/Grand_Turk"), label: "America/Grand_Turk".to_string() }, SiteSettingOption { value: json!("America/Grenada"), label: "America/Grenada".to_string() }, SiteSettingOption { value: json!("America/Guadeloupe"), label: "America/Guadeloupe".to_string() }, SiteSettingOption { value: json!("America/Guatemala"), label: "America/Guatemala".to_string() }, SiteSettingOption { value: json!("America/Guayaquil"), label: "America/Guayaquil".to_string() }, SiteSettingOption { value: json!("America/Guyana"), label: "America/Guyana".to_string() }, SiteSettingOption { value: json!("America/Halifax"), label: "America/Halifax".to_string() }, SiteSettingOption { value: json!("America/Havana"), label: "America/Havana".to_string() }, SiteSettingOption { value: json!("America/Hermosillo"), label: "America/Hermosillo".to_string() }, SiteSettingOption { value: json!("America/Indiana/Indianapolis"), label: "America/Indiana/Indianapolis".to_string() }, SiteSettingOption { value: json!("America/Indiana/Knox"), label: "America/Indiana/Knox".to_string() }, SiteSettingOption { value: json!("America/Indiana/Marengo"), label: "America/Indiana/Marengo".to_string() }, SiteSettingOption { value: json!("America/Indiana/Petersburg"), label: "America/Indiana/Petersburg".to_string() }, SiteSettingOption { value: json!("America/Indiana/Tell_City"), label: "America/Indiana/Tell_City".to_string() }, SiteSettingOption { value: json!("America/Indiana/Vevay"), label: "America/Indiana/Vevay".to_string() }, SiteSettingOption { value: json!("America/Indiana/Vincennes"), label: "America/Indiana/Vincennes".to_string() }, SiteSettingOption { value: json!("America/Indiana/Winamac"), label: "America/Indiana/Winamac".to_string() }, SiteSettingOption { value: json!("America/Inuvik"), label: "America/Inuvik".to_string() }, SiteSettingOption { value: json!("America/Iqaluit"), label: "America/Iqaluit".to_string() }, SiteSettingOption { value: json!("America/Jamaica"), label: "America/Jamaica".to_string() }, SiteSettingOption { value: json!("America/Juneau"), label: "America/Juneau".to_string() }, SiteSettingOption { value: json!("America/Kentucky/Louisville"), label: "America/Kentucky/Louisville".to_string() }, SiteSettingOption { value: json!("America/Kentucky/Monticello"), label: "America/Kentucky/Monticello".to_string() }, SiteSettingOption { value: json!("America/La_Paz"), label: "America/La_Paz".to_string() }, SiteSettingOption { value: json!("America/Lima"), label: "America/Lima".to_string() }, SiteSettingOption { value: json!("America/Los_Angeles"), label: "America/Los_Angeles".to_string() }, SiteSettingOption { value: json!("America/Maceio"), label: "America/Maceio".to_string() }, SiteSettingOption { value: json!("America/Managua"), label: "America/Managua".to_string() }, SiteSettingOption { value: json!("America/Manaus"), label: "America/Manaus".to_string() }, SiteSettingOption { value: json!("America/Martinique"), label: "America/Martinique".to_string() }, SiteSettingOption { value: json!("America/Matamoros"), label: "America/Matamoros".to_string() }, SiteSettingOption { value: json!("America/Mazatlan"), label: "America/Mazatlan".to_string() }, SiteSettingOption { value: json!("America/Menominee"), label: "America/Menominee".to_string() }, SiteSettingOption { value: json!("America/Merida"), label: "America/Merida".to_string() }, SiteSettingOption { value: json!("America/Metlakatla"), label: "America/Metlakatla".to_string() }, SiteSettingOption { value: json!("America/Mexico_City"), label: "America/Mexico_City".to_string() }, SiteSettingOption { value: json!("America/Miquelon"), label: "America/Miquelon".to_string() }, SiteSettingOption { value: json!("America/Moncton"), label: "America/Moncton".to_string() }, SiteSettingOption { value: json!("America/Monterrey"), label: "America/Monterrey".to_string() }, SiteSettingOption { value: json!("America/Montevideo"), label: "America/Montevideo".to_string() }, SiteSettingOption { value: json!("America/Montserrat"), label: "America/Montserrat".to_string() }, SiteSettingOption { value: json!("America/Nassau"), label: "America/Nassau".to_string() }, SiteSettingOption { value: json!("America/New_York"), label: "America/New_York".to_string() }, SiteSettingOption { value: json!("America/Nome"), label: "America/Nome".to_string() }, SiteSettingOption { value: json!("America/Noronha"), label: "America/Noronha".to_string() }, SiteSettingOption { value: json!("America/North_Dakota/Beulah"), label: "America/North_Dakota/Beulah".to_string() }, SiteSettingOption { value: json!("America/North_Dakota/Center"), label: "America/North_Dakota/Center".to_string() }, SiteSettingOption { value: json!("America/North_Dakota/New_Salem"), label: "America/North_Dakota/New_Salem".to_string() }, SiteSettingOption { value: json!("America/Nuuk"), label: "America/Nuuk".to_string() }, SiteSettingOption { value: json!("America/Ojinaga"), label: "America/Ojinaga".to_string() }, SiteSettingOption { value: json!("America/Panama"), label: "America/Panama".to_string() }, SiteSettingOption { value: json!("America/Paramaribo"), label: "America/Paramaribo".to_string() }, SiteSettingOption { value: json!("America/Phoenix"), label: "America/Phoenix".to_string() }, SiteSettingOption { value: json!("America/Port-au-Prince"), label: "America/Port-au-Prince".to_string() }, SiteSettingOption { value: json!("America/Port_of_Spain"), label: "America/Port_of_Spain".to_string() }, SiteSettingOption { value: json!("America/Porto_Velho"), label: "America/Porto_Velho".to_string() }, SiteSettingOption { value: json!("America/Puerto_Rico"), label: "America/Puerto_Rico".to_string() }, SiteSettingOption { value: json!("America/Punta_Arenas"), label: "America/Punta_Arenas".to_string() }, SiteSettingOption { value: json!("America/Rankin_Inlet"), label: "America/Rankin_Inlet".to_string() }, SiteSettingOption { value: json!("America/Recife"), label: "America/Recife".to_string() }, SiteSettingOption { value: json!("America/Regina"), label: "America/Regina".to_string() }, SiteSettingOption { value: json!("America/Resolute"), label: "America/Resolute".to_string() }, SiteSettingOption { value: json!("America/Rio_Branco"), label: "America/Rio_Branco".to_string() }, SiteSettingOption { value: json!("America/Santarem"), label: "America/Santarem".to_string() }, SiteSettingOption { value: json!("America/Santiago"), label: "America/Santiago".to_string() }, SiteSettingOption { value: json!("America/Santo_Domingo"), label: "America/Santo_Domingo".to_string() }, SiteSettingOption { value: json!("America/Sao_Paulo"), label: "America/Sao_Paulo".to_string() }, SiteSettingOption { value: json!("America/Scoresbysund"), label: "America/Scoresbysund".to_string() }, SiteSettingOption { value: json!("America/Sitka"), label: "America/Sitka".to_string() }, SiteSettingOption { value: json!("America/St_Johns"), label: "America/St_Johns".to_string() }, SiteSettingOption { value: json!("America/St_Kitts"), label: "America/St_Kitts".to_string() }, SiteSettingOption { value: json!("America/St_Lucia"), label: "America/St_Lucia".to_string() }, SiteSettingOption { value: json!("America/St_Thomas"), label: "America/St_Thomas".to_string() }, SiteSettingOption { value: json!("America/St_Vincent"), label: "America/St_Vincent".to_string() }, SiteSettingOption { value: json!("America/Swift_Current"), label: "America/Swift_Current".to_string() }, SiteSettingOption { value: json!("America/Tegucigalpa"), label: "America/Tegucigalpa".to_string() }, SiteSettingOption { value: json!("America/Thule"), label: "America/Thule".to_string() }, SiteSettingOption { value: json!("America/Tijuana"), label: "America/Tijuana".to_string() }, SiteSettingOption { value: json!("America/Toronto"), label: "America/Toronto".to_string() }, SiteSettingOption { value: json!("America/Tortola"), label: "America/Tortola".to_string() }, SiteSettingOption { value: json!("America/Vancouver"), label: "America/Vancouver".to_string() }, SiteSettingOption { value: json!("America/Whitehorse"), label: "America/Whitehorse".to_string() }, SiteSettingOption { value: json!("America/Winnipeg"), label: "America/Winnipeg".to_string() }, SiteSettingOption { value: json!("America/Yakutat"), label: "America/Yakutat".to_string() }, SiteSettingOption { value: json!("Antarctica/Casey"), label: "Antarctica/Casey".to_string() }, SiteSettingOption { value: json!("Antarctica/Davis"), label: "Antarctica/Davis".to_string() }, SiteSettingOption { value: json!("Antarctica/DumontDUrville"), label: "Antarctica/DumontDUrville".to_string() }, SiteSettingOption { value: json!("Antarctica/Macquarie"), label: "Antarctica/Macquarie".to_string() }, SiteSettingOption { value: json!("Antarctica/Mawson"), label: "Antarctica/Mawson".to_string() }, SiteSettingOption { value: json!("Antarctica/McMurdo"), label: "Antarctica/McMurdo".to_string() }, SiteSettingOption { value: json!("Antarctica/Palmer"), label: "Antarctica/Palmer".to_string() }, SiteSettingOption { value: json!("Antarctica/Rothera"), label: "Antarctica/Rothera".to_string() }, SiteSettingOption { value: json!("Antarctica/Syowa"), label: "Antarctica/Syowa".to_string() }, SiteSettingOption { value: json!("Antarctica/Troll"), label: "Antarctica/Troll".to_string() }, SiteSettingOption { value: json!("Antarctica/Vostok"), label: "Antarctica/Vostok".to_string() }, SiteSettingOption { value: json!("Asia/Aden"), label: "Asia/Aden".to_string() }, SiteSettingOption { value: json!("Asia/Almaty"), label: "Asia/Almaty".to_string() }, SiteSettingOption { value: json!("Asia/Amman"), label: "Asia/Amman".to_string() }, SiteSettingOption { value: json!("Asia/Anadyr"), label: "Asia/Anadyr".to_string() }, SiteSettingOption { value: json!("Asia/Aqtau"), label: "Asia/Aqtau".to_string() }, SiteSettingOption { value: json!("Asia/Aqtobe"), label: "Asia/Aqtobe".to_string() }, SiteSettingOption { value: json!("Asia/Ashgabat"), label: "Asia/Ashgabat".to_string() }, SiteSettingOption { value: json!("Asia/Atyrau"), label: "Asia/Atyrau".to_string() }, SiteSettingOption { value: json!("Asia/Baghdad"), label: "Asia/Baghdad".to_string() }, SiteSettingOption { value: json!("Asia/Bahrain"), label: "Asia/Bahrain".to_string() }, SiteSettingOption { value: json!("Asia/Baku"), label: "Asia/Baku".to_string() }, SiteSettingOption { value: json!("Asia/Bangkok"), label: "Asia/Bangkok".to_string() }, SiteSettingOption { value: json!("Asia/Barnaul"), label: "Asia/Barnaul".to_string() }, SiteSettingOption { value: json!("Asia/Beirut"), label: "Asia/Beirut".to_string() }, SiteSettingOption { value: json!("Asia/Bishkek"), label: "Asia/Bishkek".to_string() }, SiteSettingOption { value: json!("Asia/Brunei"), label: "Asia/Brunei".to_string() }, SiteSettingOption { value: json!("Asia/Chita"), label: "Asia/Chita".to_string() }, SiteSettingOption { value: json!("Asia/Colombo"), label: "Asia/Colombo".to_string() }, SiteSettingOption { value: json!("Asia/Damascus"), label: "Asia/Damascus".to_string() }, SiteSettingOption { value: json!("Asia/Dhaka"), label: "Asia/Dhaka".to_string() }, SiteSettingOption { value: json!("Asia/Dili"), label: "Asia/Dili".to_string() }, SiteSettingOption { value: json!("Asia/Dubai"), label: "Asia/Dubai".to_string() }, SiteSettingOption { value: json!("Asia/Dushanbe"), label: "Asia/Dushanbe".to_string() }, SiteSettingOption { value: json!("Asia/Famagusta"), label: "Asia/Famagusta".to_string() }, SiteSettingOption { value: json!("Asia/Gaza"), label: "Asia/Gaza".to_string() }, SiteSettingOption { value: json!("Asia/Hebron"), label: "Asia/Hebron".to_string() }, SiteSettingOption { value: json!("Asia/Ho_Chi_Minh"), label: "Asia/Ho_Chi_Minh".to_string() }, SiteSettingOption { value: json!("Asia/Hong_Kong"), label: "Asia/Hong_Kong".to_string() }, SiteSettingOption { value: json!("Asia/Hovd"), label: "Asia/Hovd".to_string() }, SiteSettingOption { value: json!("Asia/Irkutsk"), label: "Asia/Irkutsk".to_string() }, SiteSettingOption { value: json!("Asia/Jakarta"), label: "Asia/Jakarta".to_string() }, SiteSettingOption { value: json!("Asia/Jayapura"), label: "Asia/Jayapura".to_string() }, SiteSettingOption { value: json!("Asia/Jerusalem"), label: "Asia/Jerusalem".to_string() }, SiteSettingOption { value: json!("Asia/Kabul"), label: "Asia/Kabul".to_string() }, SiteSettingOption { value: json!("Asia/Kamchatka"), label: "Asia/Kamchatka".to_string() }, SiteSettingOption { value: json!("Asia/Karachi"), label: "Asia/Karachi".to_string() }, SiteSettingOption { value: json!("Asia/Kathmandu"), label: "Asia/Kathmandu".to_string() }, SiteSettingOption { value: json!("Asia/Khandyga"), label: "Asia/Khandyga".to_string() }, SiteSettingOption { value: json!("Asia/Kolkata"), label: "Asia/Kolkata".to_string() }, SiteSettingOption { value: json!("Asia/Krasnoyarsk"), label: "Asia/Krasnoyarsk".to_string() }, SiteSettingOption { value: json!("Asia/Kuala_Lumpur"), label: "Asia/Kuala_Lumpur".to_string() }, SiteSettingOption { value: json!("Asia/Kuching"), label: "Asia/Kuching".to_string() }, SiteSettingOption { value: json!("Asia/Kuwait"), label: "Asia/Kuwait".to_string() }, SiteSettingOption { value: json!("Asia/Macau"), label: "Asia/Macau".to_string() }, SiteSettingOption { value: json!("Asia/Magadan"), label: "Asia/Magadan".to_string() }, SiteSettingOption { value: json!("Asia/Makassar"), label: "Asia/Makassar".to_string() }, SiteSettingOption { value: json!("Asia/Manila"), label: "Asia/Manila".to_string() }, SiteSettingOption { value: json!("Asia/Muscat"), label: "Asia/Muscat".to_string() }, SiteSettingOption { value: json!("Asia/Nicosia"), label: "Asia/Nicosia".to_string() }, SiteSettingOption { value: json!("Asia/Novokuznetsk"), label: "Asia/Novokuznetsk".to_string() }, SiteSettingOption { value: json!("Asia/Novosibirsk"), label: "Asia/Novosibirsk".to_string() }, SiteSettingOption { value: json!("Asia/Omsk"), label: "Asia/Omsk".to_string() }, SiteSettingOption { value: json!("Asia/Oral"), label: "Asia/Oral".to_string() }, SiteSettingOption { value: json!("Asia/Phnom_Penh"), label: "Asia/Phnom_Penh".to_string() }, SiteSettingOption { value: json!("Asia/Pontianak"), label: "Asia/Pontianak".to_string() }, SiteSettingOption { value: json!("Asia/Pyongyang"), label: "Asia/Pyongyang".to_string() }, SiteSettingOption { value: json!("Asia/Qatar"), label: "Asia/Qatar".to_string() }, SiteSettingOption { value: json!("Asia/Qostanay"), label: "Asia/Qostanay".to_string() }, SiteSettingOption { value: json!("Asia/Qyzylorda"), label: "Asia/Qyzylorda".to_string() }, SiteSettingOption { value: json!("Asia/Riyadh"), label: "Asia/Riyadh".to_string() }, SiteSettingOption { value: json!("Asia/Sakhalin"), label: "Asia/Sakhalin".to_string() }, SiteSettingOption { value: json!("Asia/Samarkand"), label: "Asia/Samarkand".to_string() }, SiteSettingOption { value: json!("Asia/Seoul"), label: "Asia/Seoul".to_string() }, SiteSettingOption { value: json!("Asia/Shanghai"), label: "Asia/Shanghai".to_string() }, SiteSettingOption { value: json!("Asia/Singapore"), label: "Asia/Singapore".to_string() }, SiteSettingOption { value: json!("Asia/Srednekolymsk"), label: "Asia/Srednekolymsk".to_string() }, SiteSettingOption { value: json!("Asia/Taipei"), label: "Asia/Taipei".to_string() }, SiteSettingOption { value: json!("Asia/Tashkent"), label: "Asia/Tashkent".to_string() }, SiteSettingOption { value: json!("Asia/Tbilisi"), label: "Asia/Tbilisi".to_string() }, SiteSettingOption { value: json!("Asia/Tehran"), label: "Asia/Tehran".to_string() }, SiteSettingOption { value: json!("Asia/Thimphu"), label: "Asia/Thimphu".to_string() }, SiteSettingOption { value: json!("Asia/Tokyo"), label: "Asia/Tokyo".to_string() }, SiteSettingOption { value: json!("Asia/Tomsk"), label: "Asia/Tomsk".to_string() }, SiteSettingOption { value: json!("Asia/Ulaanbaatar"), label: "Asia/Ulaanbaatar".to_string() }, SiteSettingOption { value: json!("Asia/Urumqi"), label: "Asia/Urumqi".to_string() }, SiteSettingOption { value: json!("Asia/Ust-Nera"), label: "Asia/Ust-Nera".to_string() }, SiteSettingOption { value: json!("Asia/Vientiane"), label: "Asia/Vientiane".to_string() }, SiteSettingOption { value: json!("Asia/Vladivostok"), label: "Asia/Vladivostok".to_string() }, SiteSettingOption { value: json!("Asia/Yakutsk"), label: "Asia/Yakutsk".to_string() }, SiteSettingOption { value: json!("Asia/Yangon"), label: "Asia/Yangon".to_string() }, SiteSettingOption { value: json!("Asia/Yekaterinburg"), label: "Asia/Yekaterinburg".to_string() }, SiteSettingOption { value: json!("Asia/Yerevan"), label: "Asia/Yerevan".to_string() }, SiteSettingOption { value: json!("Atlantic/Azores"), label: "Atlantic/Azores".to_string() }, SiteSettingOption { value: json!("Atlantic/Bermuda"), label: "Atlantic/Bermuda".to_string() }, SiteSettingOption { value: json!("Atlantic/Canary"), label: "Atlantic/Canary".to_string() }, SiteSettingOption { value: json!("Atlantic/Cape_Verde"), label: "Atlantic/Cape_Verde".to_string() }, SiteSettingOption { value: json!("Atlantic/Faroe"), label: "Atlantic/Faroe".to_string() }, SiteSettingOption { value: json!("Atlantic/Madeira"), label: "Atlantic/Madeira".to_string() }, SiteSettingOption { value: json!("Atlantic/Reykjavik"), label: "Atlantic/Reykjavik".to_string() }, SiteSettingOption { value: json!("Atlantic/South_Georgia"), label: "Atlantic/South_Georgia".to_string() }, SiteSettingOption { value: json!("Atlantic/St_Helena"), label: "Atlantic/St_Helena".to_string() }, SiteSettingOption { value: json!("Atlantic/Stanley"), label: "Atlantic/Stanley".to_string() }, SiteSettingOption { value: json!("Australia/Adelaide"), label: "Australia/Adelaide".to_string() }, SiteSettingOption { value: json!("Australia/Brisbane"), label: "Australia/Brisbane".to_string() }, SiteSettingOption { value: json!("Australia/Broken_Hill"), label: "Australia/Broken_Hill".to_string() }, SiteSettingOption { value: json!("Australia/Darwin"), label: "Australia/Darwin".to_string() }, SiteSettingOption { value: json!("Australia/Eucla"), label: "Australia/Eucla".to_string() }, SiteSettingOption { value: json!("Australia/Hobart"), label: "Australia/Hobart".to_string() }, SiteSettingOption { value: json!("Australia/Lindeman"), label: "Australia/Lindeman".to_string() }, SiteSettingOption { value: json!("Australia/Lord_Howe"), label: "Australia/Lord_Howe".to_string() }, SiteSettingOption { value: json!("Australia/Melbourne"), label: "Australia/Melbourne".to_string() }, SiteSettingOption { value: json!("Australia/Perth"), label: "Australia/Perth".to_string() }, SiteSettingOption { value: json!("Australia/Sydney"), label: "Australia/Sydney".to_string() }, SiteSettingOption { value: json!("CET"), label: "CET".to_string() }, SiteSettingOption { value: json!("CST6CDT"), label: "CST6CDT".to_string() }, SiteSettingOption { value: json!("EET"), label: "EET".to_string() }, SiteSettingOption { value: json!("EST"), label: "EST".to_string() }, SiteSettingOption { value: json!("EST5EDT"), label: "EST5EDT".to_string() }, SiteSettingOption { value: json!("Europe/Amsterdam"), label: "Europe/Amsterdam".to_string() }, SiteSettingOption { value: json!("Europe/Andorra"), label: "Europe/Andorra".to_string() }, SiteSettingOption { value: json!("Europe/Astrakhan"), label: "Europe/Astrakhan".to_string() }, SiteSettingOption { value: json!("Europe/Athens"), label: "Europe/Athens".to_string() }, SiteSettingOption { value: json!("Europe/Belgrade"), label: "Europe/Belgrade".to_string() }, SiteSettingOption { value: json!("Europe/Berlin"), label: "Europe/Berlin".to_string() }, SiteSettingOption { value: json!("Europe/Brussels"), label: "Europe/Brussels".to_string() }, SiteSettingOption { value: json!("Europe/Bucharest"), label: "Europe/Bucharest".to_string() }, SiteSettingOption { value: json!("Europe/Budapest"), label: "Europe/Budapest".to_string() }, SiteSettingOption { value: json!("Europe/Chisinau"), label: "Europe/Chisinau".to_string() }, SiteSettingOption { value: json!("Europe/Copenhagen"), label: "Europe/Copenhagen".to_string() }, SiteSettingOption { value: json!("Europe/Dublin"), label: "Europe/Dublin".to_string() }, SiteSettingOption { value: json!("Europe/Gibraltar"), label: "Europe/Gibraltar".to_string() }, SiteSettingOption { value: json!("Europe/Guernsey"), label: "Europe/Guernsey".to_string() }, SiteSettingOption { value: json!("Europe/Helsinki"), label: "Europe/Helsinki".to_string() }, SiteSettingOption { value: json!("Europe/Isle_of_Man"), label: "Europe/Isle_of_Man".to_string() }, SiteSettingOption { value: json!("Europe/Istanbul"), label: "Europe/Istanbul".to_string() }, SiteSettingOption { value: json!("Europe/Jersey"), label: "Europe/Jersey".to_string() }, SiteSettingOption { value: json!("Europe/Kaliningrad"), label: "Europe/Kaliningrad".to_string() }, SiteSettingOption { value: json!("Europe/Kirov"), label: "Europe/Kirov".to_string() }, SiteSettingOption { value: json!("Europe/Kyiv"), label: "Europe/Kyiv".to_string() }, SiteSettingOption { value: json!("Europe/Lisbon"), label: "Europe/Lisbon".to_string() }, SiteSettingOption { value: json!("Europe/Ljubljana"), label: "Europe/Ljubljana".to_string() }, SiteSettingOption { value: json!("Europe/London"), label: "Europe/London".to_string() }, SiteSettingOption { value: json!("Europe/Luxembourg"), label: "Europe/Luxembourg".to_string() }, SiteSettingOption { value: json!("Europe/Madrid"), label: "Europe/Madrid".to_string() }, SiteSettingOption { value: json!("Europe/Malta"), label: "Europe/Malta".to_string() }, SiteSettingOption { value: json!("Europe/Minsk"), label: "Europe/Minsk".to_string() }, SiteSettingOption { value: json!("Europe/Monaco"), label: "Europe/Monaco".to_string() }, SiteSettingOption { value: json!("Europe/Moscow"), label: "Europe/Moscow".to_string() }, SiteSettingOption { value: json!("Europe/Oslo"), label: "Europe/Oslo".to_string() }, SiteSettingOption { value: json!("Europe/Paris"), label: "Europe/Paris".to_string() }, SiteSettingOption { value: json!("Europe/Prague"), label: "Europe/Prague".to_string() }, SiteSettingOption { value: json!("Europe/Riga"), label: "Europe/Riga".to_string() }, SiteSettingOption { value: json!("Europe/Rome"), label: "Europe/Rome".to_string() }, SiteSettingOption { value: json!("Europe/Samara"), label: "Europe/Samara".to_string() }, SiteSettingOption { value: json!("Europe/Sarajevo"), label: "Europe/Sarajevo".to_string() }, SiteSettingOption { value: json!("Europe/Saratov"), label: "Europe/Saratov".to_string() }, SiteSettingOption { value: json!("Europe/Simferopol"), label: "Europe/Simferopol".to_string() }, SiteSettingOption { value: json!("Europe/Skopje"), label: "Europe/Skopje".to_string() }, SiteSettingOption { value: json!("Europe/Sofia"), label: "Europe/Sofia".to_string() }, SiteSettingOption { value: json!("Europe/Stockholm"), label: "Europe/Stockholm".to_string() }, SiteSettingOption { value: json!("Europe/Tallinn"), label: "Europe/Tallinn".to_string() }, SiteSettingOption { value: json!("Europe/Tirane"), label: "Europe/Tirane".to_string() }, SiteSettingOption { value: json!("Europe/Ulyanovsk"), label: "Europe/Ulyanovsk".to_string() }, SiteSettingOption { value: json!("Europe/Vaduz"), label: "Europe/Vaduz".to_string() }, SiteSettingOption { value: json!("Europe/Vienna"), label: "Europe/Vienna".to_string() }, SiteSettingOption { value: json!("Europe/Vilnius"), label: "Europe/Vilnius".to_string() }, SiteSettingOption { value: json!("Europe/Volgograd"), label: "Europe/Volgograd".to_string() }, SiteSettingOption { value: json!("Europe/Warsaw"), label: "Europe/Warsaw".to_string() }, SiteSettingOption { value: json!("Europe/Zagreb"), label: "Europe/Zagreb".to_string() }, SiteSettingOption { value: json!("Europe/Zurich"), label: "Europe/Zurich".to_string() }, SiteSettingOption { value: json!("Factory"), label: "Factory".to_string() }, SiteSettingOption { value: json!("HST"), label: "HST".to_string() }, SiteSettingOption { value: json!("Indian/Antananarivo"), label: "Indian/Antananarivo".to_string() }, SiteSettingOption { value: json!("Indian/Chagos"), label: "Indian/Chagos".to_string() }, SiteSettingOption { value: json!("Indian/Christmas"), label: "Indian/Christmas".to_string() }, SiteSettingOption { value: json!("Indian/Cocos"), label: "Indian/Cocos".to_string() }, SiteSettingOption { value: json!("Indian/Comoro"), label: "Indian/Comoro".to_string() }, SiteSettingOption { value: json!("Indian/Kerguelen"), label: "Indian/Kerguelen".to_string() }, SiteSettingOption { value: json!("Indian/Mahe"), label: "Indian/Mahe".to_string() }, SiteSettingOption { value: json!("Indian/Maldives"), label: "Indian/Maldives".to_string() }, SiteSettingOption { value: json!("Indian/Mauritius"), label: "Indian/Mauritius".to_string() }, SiteSettingOption { value: json!("Indian/Mayotte"), label: "Indian/Mayotte".to_string() }, SiteSettingOption { value: json!("Indian/Reunion"), label: "Indian/Reunion".to_string() }, SiteSettingOption { value: json!("MET"), label: "MET".to_string() }, SiteSettingOption { value: json!("MST"), label: "MST".to_string() }, SiteSettingOption { value: json!("MST7MDT"), label: "MST7MDT".to_string() }, SiteSettingOption { value: json!("PST8PDT"), label: "PST8PDT".to_string() }, SiteSettingOption { value: json!("Pacific/Apia"), label: "Pacific/Apia".to_string() }, SiteSettingOption { value: json!("Pacific/Auckland"), label: "Pacific/Auckland".to_string() }, SiteSettingOption { value: json!("Pacific/Bougainville"), label: "Pacific/Bougainville".to_string() }, SiteSettingOption { value: json!("Pacific/Chatham"), label: "Pacific/Chatham".to_string() }, SiteSettingOption { value: json!("Pacific/Chuuk"), label: "Pacific/Chuuk".to_string() }, SiteSettingOption { value: json!("Pacific/Easter"), label: "Pacific/Easter".to_string() }, SiteSettingOption { value: json!("Pacific/Efate"), label: "Pacific/Efate".to_string() }, SiteSettingOption { value: json!("Pacific/Fakaofo"), label: "Pacific/Fakaofo".to_string() }, SiteSettingOption { value: json!("Pacific/Fiji"), label: "Pacific/Fiji".to_string() }, SiteSettingOption { value: json!("Pacific/Funafuti"), label: "Pacific/Funafuti".to_string() }, SiteSettingOption { value: json!("Pacific/Galapagos"), label: "Pacific/Galapagos".to_string() }, SiteSettingOption { value: json!("Pacific/Gambier"), label: "Pacific/Gambier".to_string() }, SiteSettingOption { value: json!("Pacific/Guadalcanal"), label: "Pacific/Guadalcanal".to_string() }, SiteSettingOption { value: json!("Pacific/Guam"), label: "Pacific/Guam".to_string() }, SiteSettingOption { value: json!("Pacific/Honolulu"), label: "Pacific/Honolulu".to_string() }, SiteSettingOption { value: json!("Pacific/Kanton"), label: "Pacific/Kanton".to_string() }, SiteSettingOption { value: json!("Pacific/Kiritimati"), label: "Pacific/Kiritimati".to_string() }, SiteSettingOption { value: json!("Pacific/Kosrae"), label: "Pacific/Kosrae".to_string() }, SiteSettingOption { value: json!("Pacific/Kwajalein"), label: "Pacific/Kwajalein".to_string() }, SiteSettingOption { value: json!("Pacific/Majuro"), label: "Pacific/Majuro".to_string() }, SiteSettingOption { value: json!("Pacific/Marquesas"), label: "Pacific/Marquesas".to_string() }, SiteSettingOption { value: json!("Pacific/Midway"), label: "Pacific/Midway".to_string() }, SiteSettingOption { value: json!("Pacific/Nauru"), label: "Pacific/Nauru".to_string() }, SiteSettingOption { value: json!("Pacific/Niue"), label: "Pacific/Niue".to_string() }, SiteSettingOption { value: json!("Pacific/Norfolk"), label: "Pacific/Norfolk".to_string() }, SiteSettingOption { value: json!("Pacific/Noumea"), label: "Pacific/Noumea".to_string() }, SiteSettingOption { value: json!("Pacific/Pago_Pago"), label: "Pacific/Pago_Pago".to_string() }, SiteSettingOption { value: json!("Pacific/Palau"), label: "Pacific/Palau".to_string() }, SiteSettingOption { value: json!("Pacific/Pitcairn"), label: "Pacific/Pitcairn".to_string() }, SiteSettingOption { value: json!("Pacific/Pohnpei"), label: "Pacific/Pohnpei".to_string() }, SiteSettingOption { value: json!("Pacific/Port_Moresby"), label: "Pacific/Port_Moresby".to_string() }, SiteSettingOption { value: json!("Pacific/Rarotonga"), label: "Pacific/Rarotonga".to_string() }, SiteSettingOption { value: json!("Pacific/Saipan"), label: "Pacific/Saipan".to_string() }, SiteSettingOption { value: json!("Pacific/Tahiti"), label: "Pacific/Tahiti".to_string() }, SiteSettingOption { value: json!("Pacific/Tarawa"), label: "Pacific/Tarawa".to_string() }, SiteSettingOption { value: json!("Pacific/Tongatapu"), label: "Pacific/Tongatapu".to_string() }, SiteSettingOption { value: json!("Pacific/Wake"), label: "Pacific/Wake".to_string() }, SiteSettingOption { value: json!("Pacific/Wallis"), label: "Pacific/Wallis".to_string() }, SiteSettingOption { value: json!("WET"), label: "WET".to_string() }]),
        },
        SiteSettingDef {
            key: "date_format".to_string(),
            label: "Date format".to_string(),
            description: "How dates are displayed, in PHP date-format characters.".to_string(),
            value_type: "select".to_string(),
            default: json!("F j, Y"),,
            options: Some(vec![SiteSettingOption { value: json!("F j, Y"), label: "November 23, 2026".to_string() }, SiteSettingOption { value: json!("Y-m-d"), label: "2026-11-23".to_string() }, SiteSettingOption { value: json!("m/d/Y"), label: "11/23/2026".to_string() }, SiteSettingOption { value: json!("d/m/Y"), label: "23/11/2026".to_string() }]),
        },
        SiteSettingDef {
            key: "time_format".to_string(),
            label: "Time format".to_string(),
            description: "How times are displayed, in PHP date-format characters.".to_string(),
            value_type: "select".to_string(),
            default: json!("g:i a"),,
            options: Some(vec![SiteSettingOption { value: json!("g:i a"), label: "1:05 pm".to_string() }, SiteSettingOption { value: json!("g:i A"), label: "1:05 PM".to_string() }, SiteSettingOption { value: json!("H:i"), label: "13:05".to_string() }]),
        },
        SiteSettingDef {
            key: "week_starts_on".to_string(),
            label: "Week starts on".to_string(),
            description: "The first day of the week, used by calendars. WordPress default: Monday.".to_string(),
            value_type: "select".to_string(),
            default: json!(1),,
            options: Some(vec![SiteSettingOption { value: json!(0), label: "Sunday".to_string() }, SiteSettingOption { value: json!(1), label: "Monday".to_string() }, SiteSettingOption { value: json!(2), label: "Tuesday".to_string() }, SiteSettingOption { value: json!(3), label: "Wednesday".to_string() }, SiteSettingOption { value: json!(4), label: "Thursday".to_string() }, SiteSettingOption { value: json!(5), label: "Friday".to_string() }, SiteSettingOption { value: json!(6), label: "Saturday".to_string() }]),
        },
    ]
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
        assert_eq!(languages.len(), 12);
        assert!(languages.iter().any(|option| option.value == json!("id")));
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
