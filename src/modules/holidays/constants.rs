//! Module-wide constants and limits.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, ProductCategory, RouteInfo, RouteVersionSupport};
use unified_core::tenancy::{schema_name_for_str, TenantContext as TenancyContext};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use base64::Engine as _;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use reqwest::header::RETRY_AFTER;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// CONFIG — configurable defaults (operator-overridable without code
// changes; the values below are the shipped defaults, not constants
// of the domain).
// ---------------------------------------------------------------------------

/// Default provider when a config does not name one explicitly.
/// (CONFIG: the seeded provider with this id; see `seed_providers`.)
pub(crate) const DEFAULT_PROVIDER_ID: &str = "nager-date";
/// Default sync schedule expression (interval seconds) applied when
/// a config omits `schedule`. (CONFIG: daily.)
pub(crate) const DEFAULT_SCHEDULE: &str = "86400";
/// Default years synced when a config omits `years`: the current
/// year and the next one. (CONFIG.)
pub(crate) const DEFAULT_YEARS_AHEAD: i32 = 1;
/// Hard bounds for a sync year. (CONFIG.)
pub(crate) const MIN_SYNC_YEAR: i32 = 2000;
pub(crate) const MAX_SYNC_YEAR: i32 = 2100;
/// Maximum years one config may sync. (CONFIG.)
pub(crate) const MAX_YEARS_PER_CONFIG: usize = 5;
/// Maximum countries one config may select. (CONFIG.)
pub(crate) const MAX_COUNTRIES_PER_CONFIG: usize = 60;
/// Maximum fallback providers on one config. (CONFIG.)
pub(crate) const MAX_FALLBACK_PROVIDERS: usize = 4;
/// Maximum days a `non_working_days` range may span. (CONFIG.)
pub(crate) const MAX_NON_WORKING_RANGE_DAYS: i64 = 366;
/// Maximum days a `non_working_days` hook range may span before the
/// handler clamps it. (CONFIG.)
pub(crate) const MAX_HOOK_RANGE_DAYS: i64 = 366;
/// Fetch timeout bounds in seconds. (CONFIG.)
pub(crate) const DEFAULT_FETCH_TIMEOUT_SECS: u64 = 30;
const MIN_FETCH_TIMEOUT_SECS: u64 = 5;
const MAX_FETCH_TIMEOUT_SECS: u64 = 120;
/// Maximum response body accepted from a provider. (CONFIG.)
pub(crate) const MAX_RESPONSE_BYTES: usize = 5 * 1024 * 1024;
/// Maximum redirects followed per fetch. (CONFIG.)
pub(crate) const MAX_REDIRECTS: u32 = 10;
/// Longest `Retry-After` delay honoured from a provider. (CONFIG.)
pub(crate) const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);
/// Maximum fetch attempts per (provider, country, year), including
/// the first attempt. (CONFIG.)
pub(crate) const MAX_FETCH_ATTEMPTS: u32 = 3;
/// Base delay before the first retry; doubled per attempt, capped.
/// (CONFIG.)
pub(crate) const RETRY_BASE_DELAY: Duration = Duration::from_secs(1);
pub(crate) const RETRY_MAX_DELAY: Duration = Duration::from_secs(8);
/// Maximum rate-limit wait before a fetch gives up instead of
/// sleeping longer. (CONFIG.)
pub(crate) const MAX_RATE_LIMIT_WAIT: Duration = Duration::from_secs(60);
/// Scheduler job kinds. Both are in the scheduler's `KNOWN_KINDS`
/// (`scheduler/types.rs`) and dispatched by `scheduler/service.rs`
/// to the `holidays.sync_requested` / `holidays.sync_all_requested`
/// listeners, so `scheduler.enqueue` accepts them. They are the
/// canonical names of the kinds the scheduler dispatches to this
/// module; manual syncs triggered over HTTP run inline and never
/// enqueue them (see `sync_config`), which is why no code in this
/// module references these constants today.
#[allow(dead_code)]
pub(crate) const SCHEDULER_JOB_KIND_SYNC: &str = "holidays.sync";
#[allow(dead_code)]
pub(crate) const SCHEDULER_JOB_KIND_SYNC_ALL: &str = "holidays.sync_all";
/// User-Agent sent with provider requests.
pub(crate) const USER_AGENT: &str = concat!("unified/", env!("CARGO_PKG_VERSION"));

/// ISO 3166-1 alpha-2 country list (code, English short name).
/// DATA: the full standard list, seeded as reference data. An admin
/// can add or disable entries through the countries endpoints; no
/// code change is needed to support a new country.
pub(crate) const ISO_COUNTRIES: &[(&str, &str)] = &[
    ("AD", "Andorra"), ("AE", "United Arab Emirates"), ("AF", "Afghanistan"),
    ("AG", "Antigua and Barbuda"), ("AI", "Anguilla"), ("AL", "Albania"),
    ("AM", "Armenia"), ("AO", "Angola"), ("AQ", "Antarctica"),
    ("AR", "Argentina"), ("AS", "American Samoa"), ("AT", "Austria"),
    ("AU", "Australia"), ("AW", "Aruba"), ("AX", "Aland Islands"),
    ("AZ", "Azerbaijan"), ("BA", "Bosnia and Herzegovina"), ("BB", "Barbados"),
    ("BD", "Bangladesh"), ("BE", "Belgium"), ("BF", "Burkina Faso"),
    ("BG", "Bulgaria"), ("BH", "Bahrain"), ("BI", "Burundi"),
    ("BJ", "Benin"), ("BL", "Saint Barthelemy"), ("BM", "Bermuda"),
    ("BN", "Brunei Darussalam"), ("BO", "Bolivia"),
    ("BQ", "Bonaire Sint Eustatius and Saba"), ("BR", "Brazil"),
    ("BS", "Bahamas"), ("BT", "Bhutan"), ("BV", "Bouvet Island"),
    ("BW", "Botswana"), ("BY", "Belarus"), ("BZ", "Belize"),
    ("CA", "Canada"), ("CC", "Cocos (Keeling) Islands"),
    ("CD", "Congo Democratic Republic of the"), ("CF", "Central African Republic"),
    ("CG", "Congo"), ("CH", "Switzerland"), ("CI", "Cote d'Ivoire"),
    ("CK", "Cook Islands"), ("CL", "Chile"), ("CM", "Cameroon"),
    ("CN", "China"), ("CO", "Colombia"), ("CR", "Costa Rica"),
    ("CU", "Cuba"), ("CV", "Cabo Verde"), ("CW", "Curacao"),
    ("CX", "Christmas Island"), ("CY", "Cyprus"), ("CZ", "Czechia"),
    ("DE", "Germany"), ("DJ", "Djibouti"), ("DK", "Denmark"),
    ("DM", "Dominica"), ("DO", "Dominican Republic"), ("DZ", "Algeria"),
    ("EC", "Ecuador"), ("EE", "Estonia"), ("EG", "Egypt"),
    ("EH", "Western Sahara"), ("ER", "Eritrea"), ("ES", "Spain"),
    ("ET", "Ethiopia"), ("FI", "Finland"), ("FJ", "Fiji"),
    ("FK", "Falkland Islands"), ("FM", "Micronesia"), ("FO", "Faroe Islands"),
    ("FR", "France"), ("GA", "Gabon"), ("GB", "United Kingdom"),
    ("GD", "Grenada"), ("GE", "Georgia"), ("GF", "French Guiana"),
    ("GG", "Guernsey"), ("GH", "Ghana"), ("GI", "Gibraltar"),
    ("GL", "Greenland"), ("GM", "Gambia"), ("GN", "Guinea"),
    ("GP", "Guadeloupe"), ("GQ", "Equatorial Guinea"), ("GR", "Greece"),
    ("GS", "South Georgia and the South Sandwich Islands"), ("GT", "Guatemala"),
    ("GU", "Guam"), ("GW", "Guinea-Bissau"), ("GY", "Guyana"),
    ("HK", "Hong Kong"), ("HM", "Heard Island and McDonald Islands"),
    ("HN", "Honduras"), ("HR", "Croatia"), ("HT", "Haiti"),
    ("HU", "Hungary"), ("ID", "Indonesia"), ("IE", "Ireland"),
    ("IL", "Israel"), ("IM", "Isle of Man"), ("IN", "India"),
    ("IO", "British Indian Ocean Territory"), ("IQ", "Iraq"), ("IR", "Iran"),
    ("IS", "Iceland"), ("IT", "Italy"), ("JE", "Jersey"),
    ("JM", "Jamaica"), ("JO", "Jordan"), ("JP", "Japan"),
    ("KE", "Kenya"), ("KG", "Kyrgyzstan"), ("KH", "Cambodia"),
    ("KI", "Kiribati"), ("KM", "Comoros"), ("KN", "Saint Kitts and Nevis"),
    ("KP", "Korea Democratic People's Republic of"), ("KR", "Korea Republic of"),
    ("KW", "Kuwait"), ("KY", "Cayman Islands"), ("KZ", "Kazakhstan"),
    ("LA", "Lao People's Democratic Republic"), ("LB", "Lebanon"),
    ("LC", "Saint Lucia"), ("LI", "Liechtenstein"), ("LK", "Sri Lanka"),
    ("LR", "Liberia"), ("LS", "Lesotho"), ("LT", "Lithuania"),
    ("LU", "Luxembourg"), ("LV", "Latvia"), ("LY", "Libya"),
    ("MA", "Morocco"), ("MC", "Monaco"), ("MD", "Moldova"),
    ("ME", "Montenegro"), ("MF", "Saint Martin (French part)"),
    ("MG", "Madagascar"), ("MH", "Marshall Islands"), ("MK", "North Macedonia"),
    ("ML", "Mali"), ("MM", "Myanmar"), ("MN", "Mongolia"),
    ("MO", "Macao"), ("MP", "Northern Mariana Islands"), ("MQ", "Martinique"),
    ("MR", "Mauritania"), ("MS", "Montserrat"), ("MT", "Malta"),
    ("MU", "Mauritius"), ("MV", "Maldives"), ("MW", "Malawi"),
    ("MX", "Mexico"), ("MY", "Malaysia"), ("MZ", "Mozambique"),
    ("NA", "Namibia"), ("NC", "New Caledonia"), ("NE", "Niger"),
    ("NF", "Norfolk Island"), ("NG", "Nigeria"), ("NI", "Nicaragua"),
    ("NL", "Netherlands"), ("NO", "Norway"), ("NP", "Nepal"),
    ("NR", "Nauru"), ("NU", "Niue"), ("NZ", "New Zealand"),
    ("OM", "Oman"), ("PA", "Panama"), ("PE", "Peru"),
    ("PF", "French Polynesia"), ("PG", "Papua New Guinea"), ("PH", "Philippines"),
    ("PK", "Pakistan"), ("PL", "Poland"), ("PM", "Saint Pierre and Miquelon"),
    ("PN", "Pitcairn"), ("PR", "Puerto Rico"), ("PS", "Palestine State of"),
    ("PT", "Portugal"), ("PW", "Palau"), ("PY", "Paraguay"),
    ("QA", "Qatar"), ("RE", "Reunion"), ("RO", "Romania"),
    ("RS", "Serbia"), ("RU", "Russian Federation"), ("RW", "Rwanda"),
    ("SA", "Saudi Arabia"), ("SB", "Solomon Islands"), ("SC", "Seychelles"),
    ("SD", "Sudan"), ("SE", "Sweden"), ("SG", "Singapore"),
    ("SH", "Saint Helena Ascension and Tristan da Cunha"), ("SI", "Slovenia"),
    ("SJ", "Svalbard and Jan Mayen"), ("SK", "Slovakia"), ("SL", "Sierra Leone"),
    ("SM", "San Marino"), ("SN", "Senegal"), ("SO", "Somalia"),
    ("SR", "Suriname"), ("SS", "South Sudan"), ("ST", "Sao Tome and Principe"),
    ("SV", "El Salvador"), ("SX", "Sint Maarten (Dutch part)"),
    ("SY", "Syrian Arab Republic"), ("SZ", "Eswatini"),
    ("TC", "Turks and Caicos Islands"), ("TD", "Chad"),
    ("TF", "French Southern Territories"), ("TG", "Togo"), ("TH", "Thailand"),
    ("TJ", "Tajikistan"), ("TK", "Tokelau"), ("TL", "Timor-Leste"),
    ("TM", "Turkmenistan"), ("TN", "Tunisia"), ("TO", "Tonga"),
    ("TR", "Turkiye"), ("TT", "Trinidad and Tobago"), ("TV", "Tuvalu"),
    ("TW", "Taiwan"), ("TZ", "Tanzania"), ("UA", "Ukraine"),
    ("UG", "Uganda"), ("UM", "United States Minor Outlying Islands"),
    ("US", "United States"), ("UY", "Uruguay"), ("UZ", "Uzbekistan"),
    ("VA", "Holy See"), ("VC", "Saint Vincent and the Grenadines"),
    ("VE", "Venezuela"), ("VG", "Virgin Islands British"), ("VI", "Virgin Islands U.S."),
    ("VN", "Viet Nam"), ("VU", "Vanuatu"), ("WF", "Wallis and Futuna"),
    ("WS", "Samoa"), ("YE", "Yemen"), ("YT", "Mayotte"),
    ("ZA", "South Africa"), ("ZM", "Zambia"), ("ZW", "Zimbabwe"),
];
