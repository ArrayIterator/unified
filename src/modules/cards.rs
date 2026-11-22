// Card validation module — Luhn checksum, brand detection from
// issuer identification number (IIN) ranges, expiry and CVV checks,
// plus administrator-defined custom rules.
//
// # IIN data source
//
// The standard-brand IIN ranges below come from the Wikipedia
// "Payment card number" issuer identification number table, the
// same table the public brand-factory library the user pointed at
// cites as its IIN source. The two brands that table does not
// cover were verified against that library's brand-factory source
// files, fetched from the public repository on 2026-11-22:
// Elo carries 71 six-digit prefixes (504175, 506699-506778 in the
// listed set, 509000-509882 in the listed set, 636297, 636368) and
// HiperCard carries 9 six-digit prefixes (384100, 384140, 384160,
// 606282, 637095, 637568, 637599, 637609, 637612); both use length
// 16 and the Luhn checksum. No range in this file was invented.
//
// # Sanctioned hard-coded constants
//
// Card-brand IIN ranges, the Luhn checksum, and the per-brand CVV
// lengths are international standards and are the second sanctioned
// hard-code exception in this codebase (the first is paper sizes).
// Everything else is data or configuration: which brands are
// enabled, custom rules, test mode, and the expiry upper bound all
// live in per-tenant settings or in custom-rule rows, never in code.
//
// # Matching precedence (deterministic)
//
// Longest matching prefix wins. On an equal prefix length the more
// specific brand wins (the `specificity` field): Visa Electron over
// Visa, Maestro UK over Maestro over Switch on 6759, Mastercard
// over Diners Club on 55, Discover over Troy and RuPay on a bare
// 65, Maestro over Laser on 6304, RuPay over the overlapping
// general ranges on 60, UnionPay over the overlapping general
// ranges on 62, JCB over RuPay on 353x/356x by longest prefix.
// A bare 65 is genuinely ambiguous (Discover, RuPay, Troy all
// claim it); Discover wins by specificity and the ambiguity is
// documented here, not hidden. GPN is skipped entirely unless
// `gpn_enabled` is set in settings: its ranges overlap many other
// brands and it is not widely used.
//
// # PCI handling
//
// This module is stateless validation: it NEVER persists a PAN or a
// CVV. The only stored rows are custom rules and daily validation
// counts (date, matched rule, result, count — no PANs). Every log,
// error, audit string, and hook payload carries at most the masked
// PAN (first 6 + last 4 digits, middle masked); no full PAN is ever
// logged, returned, or stored. Validation responses name the rule
// that matched and a generic reason code only
// (`invalid_luhn`, `unknown_brand`, `invalid_length`,
// `invalid_expiry`, `invalid_cvv`, `brand_disabled`,
// `test_number_rejected`, `invalid_format`); they never say which
// digit failed.
//
// # Tenant scoping
//
// Custom rules, settings, and counts are tenant business data.
// Every HTTP route resolves the tenant through
// `tenancy::TenantContext::from_http_request` and fails closed
// (401) when no tenant context resolved — never falling back to the
// control-plane schema. The hook answerer takes `tenant_id` from
// the event payload and refuses (without validating) when it is
// missing or empty. Rules are always filtered by tenant id, and a
// rule id from another tenant reads as 404, never 403, so one
// tenant cannot probe another tenant's rule ids.
//
// # Hook contract
//
// Answers `cards.validate_requested` (payload: `tenant_id`,
// `mode` (`"full"` / `"method"` / `"claim"`), `number` — the
// normalised number is passed in the event and never stored —
// plus optional `expiry` and `cvv`). Mode `"full"` requires the
// number; modes `"method"` and `"claim"` carry no number and run
// the brand + expiry checks only, with `masked_pan` /
// `claimed_brand` / `claimed_last4` from the payload. The answer is
// set on the context as `result` and added via `add_result`, and
// `cards.validated` is emitted with brand, masked PAN, and result
// (never the full number). The HTTP validate endpoint runs the
// same validation and emits `cards.validated` itself. Admin
// changes to custom rules emit `cards.custom_rule.changed`
// (create / update / delete) for the audit log.
//
// # Persistence
//
// The live store is in-memory (DashMap), the same precedent as the
// payments provider registry and the holidays module: migration
// `2026-11-22-000099_cards` is the persistence target for custom
// rules and daily counts and has never been executed against a live
// database. The migration is schema-agnostic (the runner sets the
// tenant search path); these tables are created inside the tenant
// schema, never in `public`.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext, HookHandler};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::core::tenancy;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Datelike, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Events and capabilities.
// ---------------------------------------------------------------------------

/// Asked when another module wants a card validated without calling
/// HTTP. Payload: `{"tenant_id", "mode"?, "number"?, "expiry"?,
/// "cvv"?}`. Mode `"full"` (the default when absent) validates a
/// new card and requires `number`; modes `"method"` (stored
/// method) and `"claim"` (claimed brand / last4 / expiry on
/// method onboarding) carry no number and run the brand + expiry
/// checks only, with `masked_pan` / `claimed_brand` /
/// `claimed_last4` from the payload. The answer is set on the
/// context as `result` (and added via `add_result`); `cards.validated`
/// is emitted afterwards.
pub const CARDS_VALIDATE_REQUESTED: &str = "cards.validate_requested";
/// Emitted after every validation (HTTP or hook). Payload carries
/// `tenant_id`, `masked_pan`, `brand_id`, `brand_name`,
/// `matched_rule`, `valid`, `reason_codes`, and `is_test_number`.
/// Never the full number, expiry, or CVV.
pub const CARDS_VALIDATED: &str = "cards.validated";
/// Emitted when an admin creates, updates, or deletes a custom
/// card validation rule. Payload: `tenant_id`, `id` (rule UUID),
/// `action` (`created` / `updated` / `deleted`), `actor_id`,
/// `name`, `brand_name`. The audit log subscribes to this event;
/// it carries no card data.
pub const CARDS_CUSTOM_RULE_CHANGED: &str = "cards.custom_rule.changed";

/// Read brands, settings, custom rules, and daily counts.
pub const PERM_CARDS_READ: &str = "cards:read";
/// Run a validation.
pub const PERM_CARDS_VALIDATE: &str = "cards:validate";
/// Create, update, or delete custom rules and settings.
pub const PERM_CARDS_MANAGE: &str = "cards:manage";

// ---------------------------------------------------------------------------
// Limits.
// ---------------------------------------------------------------------------

/// Shortest and longest PAN accepted (ISO/IEC 7812: 12-19 digits
/// after normalisation; the standard allows 8-19, but no brand in
/// the tables below uses fewer than 12).
pub const MIN_PAN_LENGTH: usize = 12;
/// Longest PAN accepted.
pub const MAX_PAN_LENGTH: usize = 19;
/// Longest raw `number` field accepted before normalisation, so a
/// caller cannot smuggle an unbounded string into validation.
pub const MAX_RAW_NUMBER_LENGTH: usize = 64;
/// Default expiry upper bound: current year + this many years.
pub const DEFAULT_MAX_EXPIRY_YEARS_AHEAD: u32 = 20;
/// Widest expiry upper bound an administrator may configure.
pub const MAX_EXPIRY_YEARS_AHEAD_LIMIT: u32 = 50;

// ---------------------------------------------------------------------------
// Luhn checksum.
// ---------------------------------------------------------------------------

/// Luhn checksum sum of an all-digit string, or `None` when the
/// input is empty or contains a non-digit.
///
/// Hand-checked against the reference values:
/// - `4242424242424242` sums to 80 (valid, 80 % 10 == 0).
/// - `4242424242424241` sums to 79 (invalid).
/// - `378282246310005` sums to 60 (valid).
/// - `378282246310006` sums to 61 (invalid).
pub fn luhn_checksum(digits: &str) -> Option<u32> {
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let mut sum: u32 = 0;
    let mut double = false;
    for ch in digits.chars().rev() {
        let mut value = ch.to_digit(10).unwrap_or(0);
        if double {
            value *= 2;
            if value > 9 {
                value -= 9;
            }
        }
        sum += value;
        double = !double;
    }
    Some(sum)
}

/// True when the number passes the Luhn checksum.
pub fn luhn_valid(digits: &str) -> bool {
    matches!(luhn_checksum(digits), Some(sum) if sum % 10 == 0)
}

// ---------------------------------------------------------------------------
// Normalisation and masking (PCI).
// ---------------------------------------------------------------------------

/// Strip spaces and dashes from a raw PAN. Any other non-digit
/// character is rejected: the caller must not silently accept
/// characters whose meaning is undefined.
pub fn normalise_pan(raw: &str) -> Result<String, ()> {
    let mut digits = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '0'..='9' => digits.push(ch),
            ' ' | '-' => {}
            _ => return Err(()),
        }
    }
    Ok(digits)
}

/// Mask a PAN for logs, errors, audit rows, and hook payloads:
/// first 6 + last 4 digits, the middle replaced by `*`. Numbers
/// shorter than 10 digits mask entirely (they never reach this
/// helper from a valid validation, but the helper stays safe for
/// any input).
pub fn mask_pan(digits: &str) -> String {
    let len = digits.len();
    if len < 10 {
        return "*".repeat(len);
    }
    let mut masked = String::with_capacity(len);
    masked.push_str(&digits[..6]);
    for _ in 0..(len - 10) {
        masked.push('*');
    }
    masked.push_str(&digits[len - 4..]);
    masked
}

/// Mask the digit characters of a raw input that failed
/// normalisation (illegal characters present). The illegal
/// characters themselves are dropped, so the masked value never
/// echoes the caller's exact input back.
fn mask_raw_input(raw: &str) -> String {
    let digits: String = raw.chars().filter(|ch| ch.is_ascii_digit()).collect();
    mask_pan(&digits)
}

// ---------------------------------------------------------------------------
// Expiry and CVV.
// ---------------------------------------------------------------------------

/// Validate an expiry string in `MM/YY` or `MM/YYYY` form against
/// `now`. The card is valid through the last day of its expiry
/// month. The expiry year must not exceed `now` year +
/// `max_years_ahead` (configurable, default 20).
pub fn expiry_is_valid(expiry: &str, max_years_ahead: u32, now: DateTime<Utc>) -> bool {
    let mut parts = expiry.split('/');
    let Some(month_part) = parts.next() else {
        return false;
    };
    let Some(year_part) = parts.next() else {
        return false;
    };
    if parts.next().is_some() {
        return false;
    }
    if month_part.len() != 2 || !month_part.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    if !(year_part.len() == 2 || year_part.len() == 4)
        || !year_part.bytes().all(|byte| byte.is_ascii_digit())
    {
        return false;
    }
    let Ok(month) = month_part.parse::<u32>() else {
        return false;
    };
    if month < 1 || month > 12 {
        return false;
    }
    let Ok(year) = year_part.parse::<i32>() else {
        return false;
    };
    let year = if year_part.len() == 2 { 2000 + year } else { year };
    if year > now.year() + max_years_ahead as i32 {
        return false;
    }
    year > now.year() || (year == now.year() && month >= now.month())
}

/// Validate a CVV against the expected length: all digits and
/// exactly `cvv_length` characters (4 for American Express, 3 for
/// every other brand in the standard tables).
pub fn cvv_is_valid(cvv: &str, cvv_length: u8) -> bool {
    cvv.len() == cvv_length as usize && cvv.bytes().all(|byte| byte.is_ascii_digit())
}

// ---------------------------------------------------------------------------
// Standard brand table (sanctioned hard-coded constants — see the
// module header). Lengths are PAN lengths in digits; CVV length is
// 4 for American Express and 3 for every other standard brand.
// ---------------------------------------------------------------------------

/// One numeric IIN range: the first `digits` digits of the PAN,
/// taken as a number, must fall between `start` and `end`
/// (inclusive). `digits` is the width of the range (for example 4
/// for 3528-3589, 6 for 622126-622925).
#[derive(Debug, Clone, Copy)]
pub struct IinRange {
    pub start: u64,
    pub end: u64,
    pub digits: usize,
}

/// One standard card brand.
#[derive(Debug, Clone, Copy)]
pub struct BrandRule {
    /// Stable lowercase id (for example `visa`, `visa_electron`).
    pub id: &'static str,
    /// Display name.
    pub name: &'static str,
    /// Exact IIN prefixes (for example `34`, `37`, `6759`).
    pub prefixes: &'static [&'static str],
    /// Numeric IIN ranges (for example 51-55, 2221-2720).
    pub ranges: &'static [IinRange],
    /// Valid PAN lengths in digits.
    pub lengths: &'static [u8],
    /// Expected CVV length (4 for American Express, 3 otherwise).
    pub cvv_length: u8,
    /// True when the brand's numbers must pass the Luhn checksum.
    /// Every standard brand requires Luhn.
    pub luhn_required: bool,
    /// False for inactive brands (Laser): still detectable, but
    /// reported as inactive so callers can warn instead of failing.
    pub active: bool,
    /// Tie-break when two brands match with the same prefix
    /// length; higher wins. See the module header for the
    /// precedence rules this encodes.
    pub specificity: u8,
}

impl BrandRule {
    /// Longest IIN prefix of this brand matching `digits`, or
    /// `None` when nothing matches.
    pub fn match_len(&self, digits: &str) -> Option<usize> {
        let mut best: Option<usize> = None;
        for prefix in self.prefixes {
            if digits.starts_with(prefix) {
                let len = prefix.len();
                let better = match best {
                    None => true,
                    Some(current) => len > current,
                };
                if better {
                    best = Some(len);
                }
            }
        }
        for range in self.ranges {
            if digits.len() < range.digits {
                continue;
            }
            let Ok(head) = digits[..range.digits].parse::<u64>() else {
                continue;
            };
            if head >= range.start && head <= range.end {
                let better = match best {
                    None => true,
                    Some(current) => range.digits > current,
                };
                if better {
                    best = Some(range.digits);
                }
            }
        }
        best
    }
}

/// Elo exact IIN prefixes (six digits each), verified against the
/// brand-factory source files on 2026-11-22.
pub const ELO_PREFIXES: &[&str] = &[
    "504175", "506699", "506700", "506704", "506714", "506716", "506718", "506721", "506722",
    "506724", "506725", "506726", "506727", "506728", "506729", "506730", "506731", "506732",
    "506733", "506735", "506741", "506742", "506744", "506748", "506753", "506754", "506755",
    "506758", "506760", "506761", "506762", "506764", "506766", "506767", "506770", "506771",
    "506774", "506775", "506776", "506778", "509000", "509002", "509003", "509008", "509014",
    "509015", "509017", "509018", "509032", "509034", "509041", "509042", "509045", "509048",
    "509051", "509055", "509059", "509061", "509063", "509065", "509067", "509068", "509069",
    "509073", "509091", "509093", "509291", "509728", "509882", "636297", "636368",
];

/// HiperCard exact IIN prefixes (six digits each), verified against
/// the brand-factory source files on 2026-11-22.
pub const HIPERCARD_PREFIXES: &[&str] = &[
    "384100", "384140", "384160", "606282", "637095", "637568", "637599", "637609", "637612",
];

/// All standard brands, in a stable order. Ties beyond
/// (prefix length, specificity) resolve to the earliest entry in
/// this table, so the order is part of the contract.
pub static STANDARD_BRANDS: &[BrandRule] = &[
    BrandRule {
        id: "american_express",
        name: "American Express",
        prefixes: &["34", "37"],
        ranges: &[],
        lengths: &[15],
        cvv_length: 4,
        luhn_required: true,
        active: true,
        specificity: 0,
    },
    BrandRule {
        id: "bankcard",
        name: "BankCard",
        prefixes: &["5610"],
        ranges: &[IinRange { start: 560221, end: 560225, digits: 6 }],
        lengths: &[16],
        cvv_length: 3,
        luhn_required: true,
        active: true,
        specificity: 0,
    },
    BrandRule {
        id: "borica",
        name: "Borica",
        prefixes: &["2205"],
        ranges: &[],
        lengths: &[16],
        cvv_length: 3,
        luhn_required: true,
        active: true,
        specificity: 0,
    },
    BrandRule {
        id: "china_t_union",
        name: "China T-Union",
        prefixes: &["31"],
        ranges: &[],
        lengths: &[19],
        cvv_length: 3,
        luhn_required: true,
        active: true,
        specificity: 0,
    },
    BrandRule {
        id: "dankort",
        name: "Dankort",
        prefixes: &["5019", "4571"],
        ranges: &[],
        lengths: &[16],
        cvv_length: 3,
        luhn_required: true,
        active: true,
        specificity: 0,
    },
    BrandRule {
        id: "diners_club",
        name: "Diners Club",
        prefixes: &["55"],
        ranges: &[],
        lengths: &[16],
        cvv_length: 3,
        luhn_required: true,
        active: true,
        // Mastercard wins the tie on 55 (specificity 10 there).
        specificity: 0,
    },
    BrandRule {
        id: "diners_club_international",
        name: "Diners Club International",
        prefixes: &["30", "36", "38", "39"],
        ranges: &[],
        lengths: &[14, 15, 16, 17, 18, 19],
        cvv_length: 3,
        luhn_required: true,
        active: true,
        specificity: 0,
    },
    BrandRule {
        id: "discover",
        name: "Discover",
        prefixes: &["6011", "65"],
        ranges: &[
            IinRange { start: 644, end: 649, digits: 3 },
            IinRange { start: 622126, end: 622925, digits: 6 },
        ],
        lengths: &[16, 17, 18, 19],
        cvv_length: 3,
        luhn_required: true,
        active: true,
        // Wins the bare-65 tie against RuPay and Troy.
        specificity: 10,
    },
    BrandRule {
        id: "elo",
        name: "Elo",
        prefixes: ELO_PREFIXES,
        ranges: &[],
        lengths: &[16],
        cvv_length: 3,
        luhn_required: true,
        active: true,
        specificity: 0,
    },
    BrandRule {
        id: "hipercard",
        name: "HiperCard",
        prefixes: HIPERCARD_PREFIXES,
        ranges: &[],
        lengths: &[16],
        cvv_length: 3,
        luhn_required: true,
        active: true,
        specificity: 0,
    },
    BrandRule {
        id: "humo",
        name: "Humo",
        prefixes: &["9860"],
        ranges: &[],
        lengths: &[16],
        cvv_length: 3,
        luhn_required: true,
        active: true,
        specificity: 0,
    },
    BrandRule {
        id: "interpayment",
        name: "InterPayment",
        prefixes: &["636"],
        ranges: &[],
        lengths: &[16, 17, 18, 19],
        cvv_length: 3,
        luhn_required: true,
        active: true,
        specificity: 0,
    },
    BrandRule {
        id: "jcb",
        name: "JCB",
        prefixes: &[],
        ranges: &[IinRange { start: 3528, end: 3589, digits: 4 }],
        lengths: &[16, 17, 18, 19],
        cvv_length: 3,
        luhn_required: true,
        active: true,
        specificity: 0,
    },
    BrandRule {
        id: "lankapay",
        name: "LankaPay",
        prefixes: &["357111"],
        ranges: &[],
        lengths: &[16],
        cvv_length: 3,
        luhn_required: true,
        active: true,
        specificity: 0,
    },
    BrandRule {
        id: "laser",
        name: "Laser",
        prefixes: &["6304", "6706", "6771", "6709"],
        ranges: &[],
        lengths: &[16, 17, 18, 19],
        cvv_length: 3,
        luhn_required: true,
        // Inactive brand: still detectable, reported as inactive.
        // Maestro wins the 6304 tie by specificity.
        active: false,
        specificity: 0,
    },
    BrandRule {
        id: "maestro",
        name: "Maestro",
        prefixes: &["5018", "5020", "5038", "5893", "6304", "6759", "6761", "6762", "6763"],
        ranges: &[],
        lengths: &[12, 13, 14, 15, 16, 17, 18, 19],
        cvv_length: 3,
        luhn_required: true,
        active: true,
        specificity: 5,
    },
    BrandRule {
        id: "maestro_uk",
        name: "Maestro UK",
        prefixes: &["6759", "676770", "676774"],
        ranges: &[],
        lengths: &[12, 13, 14, 15, 16, 17, 18, 19],
        cvv_length: 3,
        luhn_required: true,
        active: true,
        // Wins the 6759 tie against Maestro and Switch.
        specificity: 10,
    },
    BrandRule {
        id: "mastercard",
        name: "Mastercard",
        prefixes: &["51", "52", "53", "54", "55"],
        ranges: &[IinRange { start: 2221, end: 2720, digits: 4 }],
        lengths: &[16],
        cvv_length: 3,
        luhn_required: true,
        active: true,
        // Wins the 55 tie against Diners Club.
        specificity: 10,
    },
    BrandRule {
        id: "mir",
        name: "Mir",
        prefixes: &[],
        ranges: &[IinRange { start: 2200, end: 2204, digits: 4 }],
        lengths: &[16, 17, 18, 19],
        cvv_length: 3,
        luhn_required: true,
        active: true,
        specificity: 0,
    },
    BrandRule {
        id: "rupay",
        name: "RuPay",
        prefixes: &["60", "65", "81", "82", "508", "353", "356"],
        ranges: &[],
        lengths: &[16],
        cvv_length: 3,
        luhn_required: true,
        active: true,
        // Beats the overlapping general ranges on 60; loses the
        // bare-65 tie to Discover (specificity 10) and loses
        // 353x/356x to JCB by longest prefix.
        specificity: 5,
    },
    BrandRule {
        id: "solo",
        name: "Solo",
        prefixes: &["6334", "6767"],
        ranges: &[],
        lengths: &[16, 18, 19],
        cvv_length: 3,
        luhn_required: true,
        active: true,
        specificity: 0,
    },
    BrandRule {
        id: "switch",
        name: "Switch",
        prefixes: &["4903", "4905", "4911", "4936", "564182", "633110", "6333", "6759"],
        ranges: &[],
        lengths: &[16, 18, 19],
        cvv_length: 3,
        luhn_required: true,
        active: true,
        specificity: 0,
    },
    BrandRule {
        id: "troy",
        name: "Troy",
        prefixes: &["65", "9792"],
        ranges: &[],
        lengths: &[16],
        cvv_length: 3,
        luhn_required: true,
        active: true,
        // Loses the bare-65 tie to Discover.
        specificity: 0,
    },
    BrandRule {
        id: "unionpay",
        name: "UnionPay",
        prefixes: &["62"],
        ranges: &[],
        lengths: &[16, 17, 18, 19],
        cvv_length: 3,
        luhn_required: true,
        active: true,
        // Beats the overlapping general ranges on 62.
        specificity: 5,
    },
    BrandRule {
        id: "uzcard",
        name: "UzCard",
        prefixes: &["8600", "5614"],
        ranges: &[],
        lengths: &[16],
        cvv_length: 3,
        luhn_required: true,
        active: true,
        specificity: 0,
    },
    BrandRule {
        id: "verve",
        name: "Verve",
        prefixes: &[],
        ranges: &[
            IinRange { start: 506099, end: 506198, digits: 6 },
            IinRange { start: 650002, end: 650027, digits: 6 },
            IinRange { start: 507865, end: 507964, digits: 6 },
        ],
        lengths: &[16, 18, 19],
        cvv_length: 3,
        luhn_required: true,
        active: true,
        specificity: 0,
    },
    BrandRule {
        id: "visa",
        name: "Visa",
        prefixes: &["4"],
        ranges: &[],
        lengths: &[13, 16, 19],
        cvv_length: 3,
        luhn_required: true,
        active: true,
        specificity: 0,
    },
    BrandRule {
        id: "visa_electron",
        name: "Visa Electron",
        prefixes: &["4026", "417500", "4844", "4913", "4917"],
        ranges: &[],
        lengths: &[16],
        cvv_length: 3,
        luhn_required: true,
        active: true,
        // Longer prefixes than Visa's bare `4`, so it wins by
        // longest prefix as well as by specificity.
        specificity: 10,
    },
    BrandRule {
        id: "gpn",
        name: "GPN",
        prefixes: &["1946", "50", "56", "58"],
        ranges: &[IinRange { start: 60, end: 63, digits: 2 }],
        lengths: &[16, 18, 19],
        cvv_length: 3,
        luhn_required: true,
        active: true,
        // Disabled by default (see `CardsSettings::gpn_enabled`):
        // its ranges overlap many other brands and it is not
        // widely used. Skipped entirely while disabled.
        specificity: 0,
    },
];

/// True when `id` names a standard brand.
pub fn known_standard_brand(id: &str) -> bool {
    STANDARD_BRANDS.iter().any(|rule| rule.id == id)
}

/// One standard-brand match: the rule and the length of the IIN
/// prefix that matched.
#[derive(Debug, Clone, Copy)]
pub struct BrandMatch {
    pub rule: BrandRule,
    pub prefix_len: usize,
}

/// Detect the standard brand for normalised `digits` under
/// `settings`: longest matching prefix wins, then highest
/// specificity, then earliest table order. GPN is skipped unless
/// enabled in settings. A brand the administrator disabled is
/// still detected here (so validation can report
/// `brand_disabled`); the disabled check is the validator's job.
pub fn detect_standard_brand(digits: &str, settings: &CardsSettings) -> Option<BrandMatch> {
    let mut best: Option<BrandMatch> = None;
    for rule in STANDARD_BRANDS {
        if rule.id == "gpn" && !settings.gpn_enabled {
            continue;
        }
        let Some(prefix_len) = rule.match_len(digits) else {
            continue;
        };
        let better = match best {
            None => true,
            Some(current) => {
                (prefix_len, rule.specificity) > (current.prefix_len, current.rule.specificity)
            }
        };
        if better {
            best = Some(BrandMatch { rule: *rule, prefix_len });
        }
    }
    best
}

/// Well-known test PANs (Luhn-valid numbers publishers use for
/// sandbox testing). Recognised as test numbers: rejected in live
/// mode, allowed in sandbox mode. Never treated as real cards.
pub const TEST_PANS: &[&str] = &[
    "4242424242424242",
    "5555555555554444",
    "378282246310005",
    "6011111111111117",
    "3056930009020004",
    "3530111333300000",
];

// ---------------------------------------------------------------------------
// Settings (per-tenant, data-driven).
// ---------------------------------------------------------------------------

/// Live or sandbox mode for test-number handling. Default is live:
/// well-known test PANs are rejected unless the tenant explicitly
/// switches to sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestMode {
    Live,
    Sandbox,
}

impl TestMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Sandbox => "sandbox",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "live" | "production" => Some(Self::Live),
            "sandbox" | "test" => Some(Self::Sandbox),
            _ => None,
        }
    }
}

impl Default for TestMode {
    fn default() -> Self {
        Self::Live
    }
}

/// Per-tenant card validation settings. Everything behaviour-level
/// lives here — never in code — except the standard IIN tables
/// themselves (sanctioned constants, see the module header).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardsSettings {
    /// Test-number mode. Default `live`.
    #[serde(default)]
    pub test_mode: TestMode,
    /// Expiry upper bound in years from the current year. Default
    /// 20, configurable 1..=50.
    #[serde(default = "default_max_expiry_years_ahead")]
    pub max_expiry_years_ahead: u32,
    /// Whether the overlapping GPN brand participates in
    /// detection. Default false.
    #[serde(default)]
    pub gpn_enabled: bool,
    /// Standard brand ids the administrator disabled for this
    /// tenant. A number matching one of these validates as
    /// `brand_disabled`.
    #[serde(default)]
    pub disabled_standard_brands: Vec<String>,
}

fn default_max_expiry_years_ahead() -> u32 {
    DEFAULT_MAX_EXPIRY_YEARS_AHEAD
}

impl Default for CardsSettings {
    fn default() -> Self {
        Self {
            test_mode: TestMode::Live,
            max_expiry_years_ahead: DEFAULT_MAX_EXPIRY_YEARS_AHEAD,
            gpn_enabled: false,
            disabled_standard_brands: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Custom rules (administrator-defined, stored as data).
// ---------------------------------------------------------------------------

/// One IIN range of a custom rule, as digit strings of equal
/// length (for example `{"start": "622126", "end": "622925"}`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IinRangeInput {
    pub start: String,
    pub end: String,
}

/// One administrator-defined custom brand rule. Evaluated AFTER
/// the standard tables, unless it declares
/// `overrides_standard_brand`: then it replaces that standard
/// brand's lengths, CVV length, and Luhn requirement whenever the
/// standard brand matches. Custom rules never store a PAN or CVV.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardCustomRule {
    pub id: Uuid,
    pub tenant_id: String,
    pub name: String,
    pub brand_name: String,
    pub iin_prefixes: Vec<String>,
    pub iin_ranges: Vec<IinRangeInput>,
    pub allowed_lengths: Vec<u8>,
    pub cvv_length: u8,
    pub luhn_required: bool,
    pub enabled: bool,
    /// Standard brand id this rule overrides, or `None` for a
    /// standalone custom brand.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overrides_standard_brand: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Longest IIN prefix of a custom rule matching `digits`, or
/// `None` when nothing matches. Malformed ranges (unequal
/// lengths, unparsable bounds) never match; creation validation
/// rejects them, and this defensive check keeps a bad stored row
/// from matching everything.
fn custom_rule_match_len(rule: &CardCustomRule, digits: &str) -> Option<usize> {
    let mut best: Option<usize> = None;
    for prefix in &rule.iin_prefixes {
        if digits.starts_with(prefix.as_str()) {
            let len = prefix.len();
            let better = match best {
                None => true,
                Some(current) => len > current,
            };
            if better {
                best = Some(len);
            }
        }
    }
    for range in &rule.iin_ranges {
        if range.start.len() != range.end.len() || range.start.len() < 2 {
            continue;
        }
        let width = range.start.len();
        if digits.len() < width {
            continue;
        }
        let Ok(start) = range.start.parse::<u64>() else {
            continue;
        };
        let Ok(end) = range.end.parse::<u64>() else {
            continue;
        };
        let Ok(head) = digits[..width].parse::<u64>() else {
            continue;
        };
        if head >= start && head <= end {
            let better = match best {
                None => true,
                Some(current) => width > current,
            };
            if better {
                best = Some(width);
            }
        }
    }
    best
}

// ---------------------------------------------------------------------------
// Module state (in-memory; migration 000099_cards is the
// persistence target — see the module header).
// ---------------------------------------------------------------------------

/// Live module state. Custom rules and daily counts are keyed by
/// tenant id; settings are stored per tenant with defaults when a
/// tenant has none. The hook bus is attached by `init` so hook
/// handlers can emit `cards.validated`.
pub struct CardsState {
    pub rules: DashMap<Uuid, CardCustomRule>,
    pub settings: DashMap<String, CardsSettings>,
    /// (tenant id, UTC date `YYYY-MM-DD`, matched rule, result)
    /// -> count. Result is `valid` or `invalid`. No PANs.
    pub counts: DashMap<(String, String, String, String), u64>,
    pub hooks: RwLock<Option<Arc<HookBus>>>,
}

impl CardsState {
    pub fn new() -> Self {
        Self {
            rules: DashMap::new(),
            settings: DashMap::new(),
            counts: DashMap::new(),
            hooks: RwLock::new(None),
        }
    }

    /// Settings for a tenant, or the defaults when the tenant has
    /// never saved settings.
    pub fn settings_for(&self, tenant_id: &str) -> CardsSettings {
        self.settings
            .get(tenant_id)
            .map(|entry| entry.value().clone())
            .unwrap_or_default()
    }

    /// Enabled custom rules of one tenant, in a deterministic
    /// order (oldest first, then id) so matching ties resolve the
    /// same way on every call.
    fn tenant_rules(&self, tenant_id: &str) -> Vec<CardCustomRule> {
        let mut rules: Vec<CardCustomRule> = self
            .rules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|rule| rule.tenant_id == tenant_id && rule.enabled)
            .collect();
        rules.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then(a.id.to_string().cmp(&b.id.to_string()))
        });
        rules
    }

    /// The enabled override rule for one standard brand id, if the
    /// tenant defined one. Earliest created wins when several
    /// exist for the same brand.
    fn override_rule(&self, tenant_id: &str, brand_id: &str) -> Option<CardCustomRule> {
        self.tenant_rules(tenant_id)
            .into_iter()
            .find(|rule| rule.overrides_standard_brand.as_deref() == Some(brand_id))
    }

    /// Best standalone (non-override) custom rule matching
    /// `digits`: longest prefix wins, then earliest created.
    fn match_custom_rule(
        &self,
        tenant_id: &str,
        digits: &str,
    ) -> Option<(CardCustomRule, usize)> {
        let mut best: Option<(CardCustomRule, usize)> = None;
        for rule in self.tenant_rules(tenant_id) {
            if rule.overrides_standard_brand.is_some() {
                continue;
            }
            let Some(prefix_len) = custom_rule_match_len(&rule, digits) else {
                continue;
            };
            let better = match &best {
                None => true,
                Some((current_rule, current_len)) => {
                    prefix_len > *current_len
                        || (prefix_len == *current_len
                            && (rule.created_at, rule.id.to_string())
                                < (current_rule.created_at, current_rule.id.to_string()))
                }
            };
            if better {
                best = Some((rule, prefix_len));
            }
        }
        best
    }
}

impl Default for CardsState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Validation result and core.
// ---------------------------------------------------------------------------

/// Input to a validation. `number` is the raw PAN as supplied
/// (spaces and dashes allowed); `expiry` is `MM/YY` or `MM/YYYY`;
/// `cvv` is the card security code. Nothing here is stored.
#[derive(Debug, Clone, Deserialize)]
pub struct ValidateInput {
    pub number: String,
    #[serde(default)]
    pub expiry: Option<String>,
    #[serde(default)]
    pub cvv: Option<String>,
}

/// Result of one validation. `reason_codes` holds generic codes
/// only (never which digit failed, never the number). The
/// per-check booleans are `Some` only for checks that actually
/// ran: validation stops at the first failure, in the documented
/// order format -> length -> brand -> test number -> brand length
/// -> Luhn -> expiry -> CVV.
#[derive(Debug, Clone, Serialize)]
pub struct CardValidationResult {
    pub valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brand_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brand_name: Option<String>,
    /// `standard:<brand id>`, `custom:<rule uuid>`, or `none`.
    pub matched_rule: String,
    pub masked_pan: String,
    pub reason_codes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub luhn_valid: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub length_valid: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expiry_valid: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cvv_valid: Option<bool>,
    pub is_test_number: bool,
    pub test_mode: String,
}

impl CardValidationResult {
    fn invalid(
        masked_pan: String,
        matched_rule: String,
        reason: &str,
        brand_id: Option<String>,
        brand_name: Option<String>,
        is_test_number: bool,
        test_mode: &str,
    ) -> Self {
        Self {
            valid: false,
            brand_id,
            brand_name,
            matched_rule,
            masked_pan,
            reason_codes: vec![reason.to_string()],
            luhn_valid: None,
            length_valid: None,
            expiry_valid: None,
            cvv_valid: None,
            is_test_number,
            test_mode: test_mode.to_string(),
        }
    }
}

/// The rule a validation matched: a standard brand, a custom
/// rule, or nothing.
enum MatchedRule {
    Standard(BrandRule),
    Custom(CardCustomRule),
}

impl MatchedRule {
    fn lengths(&self) -> &[u8] {
        match self {
            Self::Standard(rule) => rule.lengths,
            Self::Custom(rule) => &rule.allowed_lengths,
        }
    }

    fn cvv_length(&self) -> u8 {
        match self {
            Self::Standard(rule) => rule.cvv_length,
            Self::Custom(rule) => rule.cvv_length,
        }
    }

    fn luhn_required(&self) -> bool {
        match self {
            Self::Standard(rule) => rule.luhn_required,
            Self::Custom(rule) => rule.luhn_required,
        }
    }

    fn matched_rule_name(&self) -> String {
        match self {
            Self::Standard(rule) => format!("standard:{}", rule.id),
            Self::Custom(rule) => format!("custom:{}", rule.id),
        }
    }

    fn brand_id(&self) -> String {
        match self {
            Self::Standard(rule) => rule.id.to_string(),
            Self::Custom(rule) => format!("custom:{}", rule.id),
        }
    }

    fn brand_name(&self) -> String {
        match self {
            Self::Standard(rule) => rule.name.to_string(),
            Self::Custom(rule) => rule.brand_name.clone(),
        }
    }
}

/// Validate one card. Pure: reads settings and custom rules from
/// `state`, never stores the number, expiry, or CVV, and never
/// logs the full number. Failure order: format -> generic length
/// -> brand -> disabled brand -> test number (live mode) -> brand
/// length -> Luhn -> expiry -> CVV.
pub fn validate_card(
    state: &CardsState,
    tenant_id: &str,
    input: &ValidateInput,
) -> CardValidationResult {
    let settings = state.settings_for(tenant_id);
    let test_mode = settings.test_mode.as_str().to_string();
    let now = Utc::now();

    let digits = match normalise_pan(&input.number) {
        Ok(digits) => digits,
        Err(()) => {
            return CardValidationResult::invalid(
                mask_raw_input(&input.number),
                "none".to_string(),
                "invalid_format",
                None,
                None,
                false,
                &test_mode,
            );
        }
    };
    let masked = mask_pan(&digits);

    if digits.len() < MIN_PAN_LENGTH || digits.len() > MAX_PAN_LENGTH {
        let mut result = CardValidationResult::invalid(
            masked,
            "none".to_string(),
            "invalid_length",
            None,
            None,
            false,
            &test_mode,
        );
        result.length_valid = Some(false);
        return result;
    }

    // Brand detection: standard tables first; an override custom
    // rule replaces the matched standard brand; otherwise a
    // standalone custom rule is tried when no standard brand
    // matches.
    let matched: Option<MatchedRule> = match detect_standard_brand(&digits, &settings) {
        Some(brand_match) => {
            if settings
                .disabled_standard_brands
                .iter()
                .any(|id| id == brand_match.rule.id)
            {
                return CardValidationResult::invalid(
                    masked,
                    format!("standard:{}", brand_match.rule.id),
                    "brand_disabled",
                    Some(brand_match.rule.id.to_string()),
                    Some(brand_match.rule.name.to_string()),
                    false,
                    &test_mode,
                );
            }
            match state.override_rule(tenant_id, brand_match.rule.id) {
                Some(rule) => Some(MatchedRule::Custom(rule)),
                None => Some(MatchedRule::Standard(brand_match.rule)),
            }
        }
        None => match state.match_custom_rule(tenant_id, &digits) {
            Some((rule, _prefix_len)) => Some(MatchedRule::Custom(rule)),
            None => None,
        },
    };
    let Some(matched) = matched else {
        return CardValidationResult::invalid(
            masked,
            "none".to_string(),
            "unknown_brand",
            None,
            None,
            false,
            &test_mode,
        );
    };
    let matched_rule_name = matched.matched_rule_name();
    let brand_id = matched.brand_id();
    let brand_name = matched.brand_name();

    let is_test_number = TEST_PANS.contains(&digits.as_str());
    if is_test_number && settings.test_mode == TestMode::Live {
        return CardValidationResult::invalid(
            masked,
            matched_rule_name,
            "test_number_rejected",
            Some(brand_id),
            Some(brand_name),
            true,
            &test_mode,
        );
    }

    if !matched.lengths().contains(&(digits.len() as u8)) {
        let mut result = CardValidationResult::invalid(
            masked,
            matched_rule_name,
            "invalid_length",
            Some(brand_id),
            Some(brand_name),
            is_test_number,
            &test_mode,
        );
        result.length_valid = Some(false);
        return result;
    }

    if matched.luhn_required() && !luhn_valid(&digits) {
        let mut result = CardValidationResult::invalid(
            masked,
            matched_rule_name,
            "invalid_luhn",
            Some(brand_id),
            Some(brand_name),
            is_test_number,
            &test_mode,
        );
        result.length_valid = Some(true);
        result.luhn_valid = Some(false);
        return result;
    }

    if let Some(expiry) = &input.expiry {
        if !expiry_is_valid(expiry, settings.max_expiry_years_ahead, now) {
            let mut result = CardValidationResult::invalid(
                masked,
                matched_rule_name,
                "invalid_expiry",
                Some(brand_id),
                Some(brand_name),
                is_test_number,
                &test_mode,
            );
            result.length_valid = Some(true);
            result.luhn_valid = if matched.luhn_required() {
                Some(true)
            } else {
                None
            };
            result.expiry_valid = Some(false);
            return result;
        }
    }

    if let Some(cvv) = &input.cvv {
        if !cvv_is_valid(cvv, matched.cvv_length()) {
            let mut result = CardValidationResult::invalid(
                masked,
                matched_rule_name,
                "invalid_cvv",
                Some(brand_id),
                Some(brand_name),
                is_test_number,
                &test_mode,
            );
            result.length_valid = Some(true);
            result.luhn_valid = if matched.luhn_required() {
                Some(true)
            } else {
                None
            };
            result.expiry_valid = input.expiry.as_ref().map(|_| true);
            result.cvv_valid = Some(false);
            return result;
        }
    }

    CardValidationResult {
        valid: true,
        brand_id: Some(brand_id),
        brand_name: Some(brand_name),
        matched_rule: matched_rule_name,
        masked_pan: masked,
        reason_codes: Vec::new(),
        luhn_valid: if matched.luhn_required() {
            Some(true)
        } else {
            None
        },
        length_valid: Some(true),
        expiry_valid: input.expiry.as_ref().map(|_| true),
        cvv_valid: input.cvv.as_ref().map(|_| true),
        is_test_number,
        test_mode,
    }
}

/// Record one validation outcome in the daily counts (tenant,
/// UTC date, matched rule, valid/invalid — no PANs). Shared by
/// the full and the number-less validation paths, so counts stay
/// consistent.
fn record_validation(state: &CardsState, tenant_id: &str, result: &CardValidationResult) {
    let today = Utc::now().format("%Y-%m-%d").to_string();
    let outcome = if result.valid { "valid" } else { "invalid" };
    let key = (
        tenant_id.to_string(),
        today,
        result.matched_rule.clone(),
        outcome.to_string(),
    );
    state
        .counts
        .entry(key)
        .and_modify(|count| *count += 1)
        .or_insert(1);
}

/// Validate and record the outcome in the daily counts
/// (tenant, UTC date, matched rule, valid/invalid — no PANs).
/// This is the single entry point the HTTP handler and the hook
/// answerer both use, so counts stay consistent.
pub fn validate_and_record(
    state: &CardsState,
    tenant_id: &str,
    input: &ValidateInput,
) -> CardValidationResult {
    let result = validate_card(state, tenant_id, input);
    record_validation(state, tenant_id, &result);
    result
}

/// Find a standard brand by the caller's claim: the brand slug
/// first (`visa`), then the display name case-insensitively
/// (`Visa`). The gateway sends the slug the stored method was
/// onboarded with; the name fallback covers callers that carry
/// the display name instead. Returns the rule, never a custom
/// rule: without a PAN there is no IIN to match a custom rule
/// against.
fn find_standard_brand(claimed: &str) -> Option<BrandRule> {
    let wanted = claimed.trim();
    if wanted.is_empty() {
        return None;
    }
    STANDARD_BRANDS
        .iter()
        .find(|rule| rule.id == wanted)
        .or_else(|| {
            STANDARD_BRANDS
                .iter()
                .find(|rule| rule.name.eq_ignore_ascii_case(wanted))
        })
        .copied()
}

/// Masked PAN for a number-less validation, from the payload only.
/// The gateway sends `******{last4}` and `claimed_last4` together;
/// the last4 wins when it is exactly four digits, so the two can
/// never disagree. A caller-supplied `masked_pan` is accepted only
/// when it is already a masked value (stars and digits only, at
/// least one star, at most one PAN length): a bare digit string
/// would be a full PAN smuggled into the audited `cards.validated`
/// event. Anything else yields an empty masked PAN rather than an
/// invented one.
fn numberless_masked_pan(ctx: &HookContext) -> String {
    let last4 = ctx
        .get("claimed_last4")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .unwrap_or("");
    if last4.len() == 4 && last4.bytes().all(|byte| byte.is_ascii_digit()) {
        return format!("******{last4}");
    }
    let supplied = ctx
        .get("masked_pan")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .unwrap_or("");
    if !supplied.is_empty()
        && supplied.len() <= MAX_PAN_LENGTH
        && supplied.contains('*')
        && supplied
            .chars()
            .all(|ch| ch == '*' || ch.is_ascii_digit())
    {
        return supplied.to_string();
    }
    String::new()
}

/// Validate a stored method / claimed card without a PAN
/// (gateway modes `"method"` and `"claim"`). Only the brand and
/// the expiry can be checked without a number, so only those run:
/// the claimed brand must be a known standard brand that is
/// enabled for this tenant (GPN counts as disabled while
/// `gpn_enabled` is false), and the expiry must be present, not
/// past, and within the configured upper bound. The result has
/// the same shape as a full validation (`valid`, `reason_codes`,
/// `brand_id` / `brand_name`, `matched_rule`, `masked_pan` from
/// the payload) so the gateway reads it through the same answer
/// contract — but `valid: true` here means "brand known and
/// expiry in range", never "this is a new, fully validated card":
/// mode `"full"` still requires the number, and the hook answerer
/// refuses a number-less payload in that mode. No Luhn, length,
/// CVV, or test-number check can run without the number, so those
/// per-check booleans stay `None`.
pub fn validate_numberless(
    state: &CardsState,
    tenant_id: &str,
    claimed_brand: Option<&str>,
    expiry: Option<&str>,
    masked_pan: String,
) -> CardValidationResult {
    let settings = state.settings_for(tenant_id);
    let test_mode = settings.test_mode.as_str().to_string();
    let Some(rule) = claimed_brand.and_then(find_standard_brand) else {
        return CardValidationResult::invalid(
            masked_pan,
            "none".to_string(),
            "unknown_brand",
            None,
            None,
            false,
            &test_mode,
        );
    };
    let matched_rule_name = format!("standard:{}", rule.id);
    let brand_id = rule.id.to_string();
    let brand_name = rule.name.to_string();
    // GPN is skipped by detection while disabled, and a disabled
    // brand is still reported by the full validator as
    // `brand_disabled`: the number-less path reports both the
    // same way.
    if (rule.id == "gpn" && !settings.gpn_enabled)
        || settings
            .disabled_standard_brands
            .iter()
            .any(|id| id == rule.id)
    {
        return CardValidationResult::invalid(
            masked_pan,
            matched_rule_name,
            "brand_disabled",
            Some(brand_id),
            Some(brand_name),
            false,
            &test_mode,
        );
    }
    let now = Utc::now();
    let expiry_valid = expiry
        .map(|expiry| expiry_is_valid(expiry, settings.max_expiry_years_ahead, now))
        .unwrap_or(false);
    if !expiry_valid {
        let mut result = CardValidationResult::invalid(
            masked_pan,
            matched_rule_name,
            "invalid_expiry",
            Some(brand_id),
            Some(brand_name),
            false,
            &test_mode,
        );
        result.expiry_valid = Some(false);
        return result;
    }
    CardValidationResult {
        valid: true,
        brand_id: Some(brand_id),
        brand_name: Some(brand_name),
        matched_rule: matched_rule_name,
        masked_pan,
        reason_codes: Vec::new(),
        luhn_valid: None,
        length_valid: None,
        expiry_valid: Some(true),
        cvv_valid: None,
        is_test_number: false,
        test_mode,
    }
}

/// Validate without a PAN and record the outcome in the daily
/// counts, mirroring `validate_and_record` for the number-less
/// path.
pub fn validate_numberless_and_record(
    state: &CardsState,
    tenant_id: &str,
    claimed_brand: Option<&str>,
    expiry: Option<&str>,
    masked_pan: String,
) -> CardValidationResult {
    let result = validate_numberless(state, tenant_id, claimed_brand, expiry, masked_pan);
    record_validation(state, tenant_id, &result);
    result
}

// ---------------------------------------------------------------------------
// Caller identity and capability checks (same shape as the other
// modules: `X-User-Id` identifies the actor, `X-Role` carries the
// role slug the capability check runs against, `role.check`
// answers; no answer means not allowed — fail closed).
// ---------------------------------------------------------------------------

struct Caller {
    actor: String,
    role: String,
}

fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let actor = req
        .headers()
        .get("x-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-User-Id header"))?;
    let role = req
        .headers()
        .get("x-role")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-Role header"))?;
    Ok(Caller { actor, role })
}

fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

/// Resolve the tenant for a request, failing closed when no tenant
/// context resolved. Never falls back to the control-plane schema.
fn tenant_id(req: &HttpRequest) -> UnifiedResult<String> {
    let tenant = tenancy::TenantContext::from_http_request(req)?;
    Ok(tenant.tenant_id.to_string())
}

fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

// ---------------------------------------------------------------------------
// Hook answerer.
// ---------------------------------------------------------------------------

/// Answer `cards.validate_requested`: validate the number from the
/// event payload (never stored), set the result on the context, and
/// emit `cards.validated` with the masked PAN and the result. The
/// payload must carry `tenant_id`; without it the request is
/// refused outright (fail closed, no validation, no emit). A
/// number-less payload is answered only in modes `"method"` and
/// `"claim"` (stored method / claimed brand + expiry); mode
/// `"full"` — and any unknown or absent mode — still requires the
/// number, so a number-less payload is never accepted as a fully
/// validated new card.
fn on_validate_requested(state: Arc<CardsState>, ctx: &mut HookContext) {
    let tenant_id = ctx
        .get("tenant_id")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    if tenant_id.is_empty() {
        ctx.set(
            "result",
            json!({
                "valid": false,
                "matched_rule": "none",
                "masked_pan": "",
                "reason_codes": ["missing_tenant_context"],
                "is_test_number": false,
            }),
        );
        ctx.set(
            "error",
            json!("missing tenant_id: validation refused without a tenant context"),
        );
        return;
    }
    let mode = ctx
        .get("mode")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .unwrap_or("full");
    let expiry = ctx
        .get("expiry")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let cvv = ctx
        .get("cvv")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let number = ctx
        .get("number")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let result = match number {
        Some(number) => {
            let input = ValidateInput { number, expiry, cvv };
            validate_and_record(&state, &tenant_id, &input)
        }
        None => {
            // Number-less payload: only the stored-method and
            // claim modes may proceed, and only with the brand +
            // expiry checks. Mode `full` (and any unknown mode, or
            // a payload with no mode at all) still requires the
            // number — a number-less payload is never accepted as
            // a fully validated new card.
            if mode != "method" && mode != "claim" {
                ctx.set(
                    "result",
                    json!({
                        "valid": false,
                        "matched_rule": "none",
                        "masked_pan": "",
                        "reason_codes": ["invalid_format"],
                        "is_test_number": false,
                    }),
                );
                ctx.set("error", json!("missing number in cards.validate_requested"));
                return;
            }
            let claimed_brand = ctx
                .get("claimed_brand")
                .and_then(|value| value.as_str())
                .or_else(|| ctx.get("brand").and_then(|value| value.as_str()));
            let masked_pan = numberless_masked_pan(ctx);
            validate_numberless_and_record(
                &state,
                &tenant_id,
                claimed_brand,
                expiry.as_deref(),
                masked_pan,
            )
        }
    };
    let payload = serde_json::to_value(&result).unwrap_or_else(|_| {
        json!({
            "valid": false,
            "matched_rule": "none",
            "masked_pan": "",
            "reason_codes": ["invalid_format"],
            "is_test_number": false,
        })
    });
    ctx.set("result", payload.clone());
    ctx.add_result(payload);
    if let Some(bus) = state.hooks.read().expect("hooks lock poisoned").clone() {
        bus.emit(
            CARDS_VALIDATED,
            json!({
                "tenant_id": tenant_id,
                "masked_pan": result.masked_pan,
                "brand_id": result.brand_id,
                "brand_name": result.brand_name,
                "matched_rule": result.matched_rule,
                "valid": result.valid,
                "reason_codes": result.reason_codes,
                "is_test_number": result.is_test_number,
            }),
        );
    }
}

// ---------------------------------------------------------------------------
// Request/response shapes.
// ---------------------------------------------------------------------------

/// Read view of one standard brand, with its effective enabled
/// state for the requesting tenant.
#[derive(Debug, Clone, Serialize)]
pub struct BrandView {
    pub id: String,
    pub name: String,
    pub lengths: Vec<u8>,
    pub cvv_length: u8,
    pub luhn_required: bool,
    pub active: bool,
    pub enabled: bool,
}

fn brand_view(rule: &BrandRule, settings: &CardsSettings) -> BrandView {
    let enabled = (rule.id != "gpn" || settings.gpn_enabled)
        && !settings
            .disabled_standard_brands
            .iter()
            .any(|id| id == rule.id);
    BrandView {
        id: rule.id.to_string(),
        name: rule.name.to_string(),
        lengths: rule.lengths.to_vec(),
        cvv_length: rule.cvv_length,
        luhn_required: rule.luhn_required,
        active: rule.active,
        enabled,
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ValidateRequest {
    pub number: String,
    #[serde(default)]
    pub expiry: Option<String>,
    #[serde(default)]
    pub cvv: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateSettingsRequest {
    #[serde(default)]
    pub test_mode: Option<String>,
    #[serde(default)]
    pub max_expiry_years_ahead: Option<u32>,
    #[serde(default)]
    pub gpn_enabled: Option<bool>,
    #[serde(default)]
    pub disabled_standard_brands: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateCustomRuleRequest {
    pub name: String,
    pub brand_name: String,
    #[serde(default)]
    pub iin_prefixes: Vec<String>,
    #[serde(default)]
    pub iin_ranges: Vec<IinRangeInput>,
    pub allowed_lengths: Vec<u8>,
    #[serde(default)]
    pub cvv_length: Option<u8>,
    #[serde(default)]
    pub luhn_required: Option<bool>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub overrides_standard_brand: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateCustomRuleRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub brand_name: Option<String>,
    #[serde(default)]
    pub iin_prefixes: Option<Vec<String>>,
    #[serde(default)]
    pub iin_ranges: Option<Vec<IinRangeInput>>,
    #[serde(default)]
    pub allowed_lengths: Option<Vec<u8>>,
    #[serde(default)]
    pub cvv_length: Option<u8>,
    #[serde(default)]
    pub luhn_required: Option<bool>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub overrides_standard_brand: Option<String>,
}

/// One daily validation count row (no PANs).
#[derive(Debug, Clone, Serialize)]
pub struct DailyCountView {
    pub date: String,
    pub matched_rule: String,
    pub result: String,
    pub count: u64,
}

// ---------------------------------------------------------------------------
// Custom-rule input validation.
// ---------------------------------------------------------------------------

struct ValidatedRuleFields {
    name: String,
    brand_name: String,
    iin_prefixes: Vec<String>,
    iin_ranges: Vec<IinRangeInput>,
    allowed_lengths: Vec<u8>,
    cvv_length: u8,
    overrides_standard_brand: Option<String>,
}

fn validate_rule_fields(
    name: String,
    brand_name: String,
    mut iin_prefixes: Vec<String>,
    mut iin_ranges: Vec<IinRangeInput>,
    mut allowed_lengths: Vec<u8>,
    cvv_length: u8,
    overrides_standard_brand: Option<String>,
) -> UnifiedResult<ValidatedRuleFields> {
    let name = require_text(&name, "name", 100)?;
    let brand_name = require_text(&brand_name, "brand_name", 100)?;

    let mut prefixes: Vec<String> = Vec::new();
    for prefix in iin_prefixes.drain(..) {
        let prefix = prefix.trim().to_string();
        if prefix.is_empty()
            || prefix.len() > 8
            || !prefix.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(UnifiedError::bad_request(
                "iin_prefixes must be digit strings of 1-8 digits",
            ));
        }
        if !prefixes.contains(&prefix) {
            prefixes.push(prefix);
        }
    }
    prefixes.sort();

    let mut ranges: Vec<IinRangeInput> = Vec::new();
    for range in iin_ranges.drain(..) {
        let start = range.start.trim().to_string();
        let end = range.end.trim().to_string();
        if start.len() != end.len()
            || start.len() < 2
            || start.len() > 8
            || !start.bytes().all(|byte| byte.is_ascii_digit())
            || !end.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(UnifiedError::bad_request(
                "iin_ranges start and end must be digit strings of equal length (2-8 digits)",
            ));
        }
        let start_value = start.parse::<u64>().map_err(|_| {
            UnifiedError::bad_request("iin_ranges start is not a valid number")
        })?;
        let end_value = end
            .parse::<u64>()
            .map_err(|_| UnifiedError::bad_request("iin_ranges end is not a valid number"))?;
        if start_value > end_value {
            return Err(UnifiedError::bad_request(
                "iin_ranges start must not be greater than end",
            ));
        }
        if !ranges
            .iter()
            .any(|kept| kept.start == start && kept.end == end)
        {
            ranges.push(IinRangeInput { start, end });
        }
    }
    ranges.sort_by(|a, b| a.start.cmp(&b.start).then(a.end.cmp(&b.end)));

    if prefixes.is_empty() && ranges.is_empty() {
        return Err(UnifiedError::bad_request(
            "a custom rule needs at least one iin_prefix or iin_range",
        ));
    }

    allowed_lengths.sort_unstable();
    allowed_lengths.dedup();
    if allowed_lengths.is_empty()
        || allowed_lengths
            .iter()
            .any(|length| *length < MIN_PAN_LENGTH as u8 || *length > MAX_PAN_LENGTH as u8)
    {
        return Err(UnifiedError::bad_request(
            "allowed_lengths must be non-empty with every length between 12 and 19",
        ));
    }

    if cvv_length != 3 && cvv_length != 4 {
        return Err(UnifiedError::bad_request(
            "cvv_length must be 3 or 4",
        ));
    }

    let overrides_standard_brand = match overrides_standard_brand {
        Some(value) => {
            let value = value.trim().to_string();
            if value.is_empty() {
                None
            } else if !known_standard_brand(&value) {
                return Err(UnifiedError::bad_request(format!(
                    "overrides_standard_brand \"{value}\" is not a known standard brand"
                )));
            } else {
                Some(value)
            }
        }
        None => None,
    };

    Ok(ValidatedRuleFields {
        name,
        brand_name,
        iin_prefixes: prefixes,
        iin_ranges: ranges,
        allowed_lengths,
        cvv_length,
        overrides_standard_brand,
    })
}

// ---------------------------------------------------------------------------
// HTTP handlers.
// ---------------------------------------------------------------------------

async fn validate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CardsState>>,
    req: HttpRequest,
    body: web::Json<ValidateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CardValidationResult> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CARDS_VALIDATE)?;
        let tenant = tenant_id(&req)?;
        let input = body.into_inner();
        if input.number.trim().is_empty() {
            return Err(UnifiedError::bad_request("number must not be empty"));
        }
        if input.number.len() > MAX_RAW_NUMBER_LENGTH {
            return Err(UnifiedError::bad_request(
                "number is too long (at most 64 characters)",
            ));
        }
        if let Some(expiry) = &input.expiry {
            if expiry.len() > 10 {
                return Err(UnifiedError::bad_request(
                    "expiry is too long (expected MM/YY or MM/YYYY)",
                ));
            }
        }
        if let Some(cvv) = &input.cvv {
            if cvv.len() > 16 {
                return Err(UnifiedError::bad_request("cvv is too long"));
            }
        }
        let outcome = validate_and_record(&state, &tenant, &ValidateInput {
            number: input.number,
            expiry: input.expiry,
            cvv: input.cvv,
        });
        ctx.hooks.emit(
            CARDS_VALIDATED,
            json!({
                "tenant_id": tenant,
                "masked_pan": outcome.masked_pan,
                "brand_id": outcome.brand_id,
                "brand_name": outcome.brand_name,
                "matched_rule": outcome.matched_rule,
                "valid": outcome.valid,
                "reason_codes": outcome.reason_codes,
                "is_test_number": outcome.is_test_number,
            }),
        );
        Ok(outcome)
    })();
    json::respond(result, debug)
}

async fn list_brands(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CardsState>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vec<BrandView>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CARDS_READ)?;
        let tenant = tenant_id(&req)?;
        let settings = state.settings_for(&tenant);
        Ok(STANDARD_BRANDS
            .iter()
            .map(|rule| brand_view(rule, &settings))
            .collect())
    })();
    json::respond(result, debug)
}

async fn get_brand(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CardsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BrandView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CARDS_READ)?;
        let tenant = tenant_id(&req)?;
        let settings = state.settings_for(&tenant);
        let id = path.into_inner();
        let rule = STANDARD_BRANDS
            .iter()
            .find(|rule| rule.id == id)
            .ok_or_else(|| UnifiedError::not_found(format!("Brand \"{id}\" not found")))?;
        Ok(brand_view(rule, &settings))
    })();
    json::respond(result, debug)
}

async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CardsState>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CardsSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CARDS_READ)?;
        let tenant = tenant_id(&req)?;
        Ok(state.settings_for(&tenant))
    })();
    json::respond(result, debug)
}

async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CardsState>>,
    req: HttpRequest,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CardsSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CARDS_MANAGE)?;
        let tenant = tenant_id(&req)?;
        let input = body.into_inner();
        let mut settings = state.settings_for(&tenant);
        if let Some(test_mode) = input.test_mode {
            settings.test_mode = TestMode::from_str(test_mode.trim()).ok_or_else(|| {
                UnifiedError::bad_request("test_mode must be \"live\" or \"sandbox\"")
            })?;
        }
        if let Some(max_years) = input.max_expiry_years_ahead {
            if max_years < 1 || max_years > MAX_EXPIRY_YEARS_AHEAD_LIMIT {
                return Err(UnifiedError::bad_request(
                    "max_expiry_years_ahead must be between 1 and 50",
                ));
            }
            settings.max_expiry_years_ahead = max_years;
        }
        if let Some(gpn_enabled) = input.gpn_enabled {
            settings.gpn_enabled = gpn_enabled;
        }
        if let Some(disabled) = input.disabled_standard_brands {
            let mut cleaned: Vec<String> = Vec::new();
            for id in disabled {
                let id = id.trim().to_string();
                if id.is_empty() {
                    continue;
                }
                if !known_standard_brand(&id) {
                    return Err(UnifiedError::bad_request(format!(
                        "disabled_standard_brands contains unknown brand \"{id}\""
                    )));
                }
                if !cleaned.contains(&id) {
                    cleaned.push(id);
                }
            }
            cleaned.sort();
            settings.disabled_standard_brands = cleaned;
        }
        state.settings.insert(tenant, settings.clone());
        Ok(settings)
    })();
    json::respond(result, debug)
}

async fn list_custom_rules(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CardsState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<CardCustomRule>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CARDS_READ)?;
        let tenant = tenant_id(&req)?;
        let params = query.into_inner();
        let mut items: Vec<CardCustomRule> = state
            .rules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|rule| rule.tenant_id == tenant)
            .filter(|rule| {
                matches_search(&format!("{} {}", rule.name, rule.brand_name), &params)
                    && params
                        .filters
                        .get("enabled")
                        .is_none_or(|wanted| {
                            rule.enabled == (*wanted == "true" || *wanted == "1")
                        })
                    && params
                        .filters
                        .get("overrides_standard_brand")
                        .is_none_or(|wanted| {
                            rule.overrides_standard_brand.as_deref() == Some(wanted.as_str())
                        })
            })
            .collect();
        let sort = params.allowed_sort(&["name", "brand_name", "created_at"], "created_at");
        items.sort_by(|a, b| match sort.as_str() {
            "name" => a.name.cmp(&b.name).then(a.id.to_string().cmp(&b.id.to_string())),
            "brand_name" => a
                .brand_name
                .cmp(&b.brand_name)
                .then(a.id.to_string().cmp(&b.id.to_string())),
            _ => a
                .created_at
                .cmp(&b.created_at)
                .then(a.id.to_string().cmp(&b.id.to_string())),
        });
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn create_custom_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CardsState>>,
    req: HttpRequest,
    body: web::Json<CreateCustomRuleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CardCustomRule> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CARDS_MANAGE)?;
        let tenant = tenant_id(&req)?;
        let input = body.into_inner();
        let fields = validate_rule_fields(
            input.name,
            input.brand_name,
            input.iin_prefixes,
            input.iin_ranges,
            input.allowed_lengths,
            input.cvv_length.unwrap_or(3),
            input.overrides_standard_brand,
        )?;
        let now = Utc::now();
        let rule = CardCustomRule {
            id: Uuid::now_v7(),
            tenant_id: tenant,
            name: fields.name,
            brand_name: fields.brand_name,
            iin_prefixes: fields.iin_prefixes,
            iin_ranges: fields.iin_ranges,
            allowed_lengths: fields.allowed_lengths,
            cvv_length: fields.cvv_length,
            luhn_required: input.luhn_required.unwrap_or(true),
            enabled: input.enabled.unwrap_or(true),
            overrides_standard_brand: fields.overrides_standard_brand,
            created_at: now,
            updated_at: now,
        };
        state.rules.insert(rule.id, rule.clone());
        ctx.hooks.emit(
            CARDS_CUSTOM_RULE_CHANGED,
            json!({
                "tenant_id": rule.tenant_id.clone(),
                "id": rule.id.to_string(),
                "action": "created",
                "actor_id": caller.actor.clone(),
                "name": rule.name.clone(),
                "brand_name": rule.brand_name.clone(),
            }),
        );
        Ok(rule)
    })();
    match result {
        Ok(rule) => json::success_response(rule, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_custom_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CardsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CardCustomRule> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CARDS_READ)?;
        let tenant = tenant_id(&req)?;
        let id = Uuid::parse_str(path.into_inner().trim())
            .map_err(|_| UnifiedError::bad_request("Invalid custom rule id: expected a UUID"))?;
        let rule = state
            .rules
            .get(&id)
            .map(|entry| entry.value().clone())
            .filter(|rule| rule.tenant_id == tenant)
            .ok_or_else(|| UnifiedError::not_found("Custom rule not found"))?;
        Ok(rule)
    })();
    json::respond(result, debug)
}

async fn update_custom_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CardsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateCustomRuleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CardCustomRule> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CARDS_MANAGE)?;
        let tenant = tenant_id(&req)?;
        let id = Uuid::parse_str(path.into_inner().trim())
            .map_err(|_| UnifiedError::bad_request("Invalid custom rule id: expected a UUID"))?;
        let existing = state
            .rules
            .get(&id)
            .map(|entry| entry.value().clone())
            .filter(|rule| rule.tenant_id == tenant)
            .ok_or_else(|| UnifiedError::not_found("Custom rule not found"))?;
        let input = body.into_inner();
        let fields = validate_rule_fields(
            input.name.unwrap_or(existing.name.clone()),
            input.brand_name.unwrap_or(existing.brand_name.clone()),
            input.iin_prefixes.unwrap_or(existing.iin_prefixes.clone()),
            input.iin_ranges.unwrap_or(existing.iin_ranges.clone()),
            input.allowed_lengths.unwrap_or(existing.allowed_lengths.clone()),
            input.cvv_length.unwrap_or(existing.cvv_length),
            input
                .overrides_standard_brand
                .or(existing.overrides_standard_brand.clone()),
        )?;
        let updated = CardCustomRule {
            id: existing.id,
            tenant_id: existing.tenant_id,
            name: fields.name,
            brand_name: fields.brand_name,
            iin_prefixes: fields.iin_prefixes,
            iin_ranges: fields.iin_ranges,
            allowed_lengths: fields.allowed_lengths,
            cvv_length: fields.cvv_length,
            luhn_required: input.luhn_required.unwrap_or(existing.luhn_required),
            enabled: input.enabled.unwrap_or(existing.enabled),
            overrides_standard_brand: fields.overrides_standard_brand,
            created_at: existing.created_at,
            updated_at: Utc::now(),
        };
        state.rules.insert(updated.id, updated.clone());
        ctx.hooks.emit(
            CARDS_CUSTOM_RULE_CHANGED,
            json!({
                "tenant_id": updated.tenant_id.clone(),
                "id": updated.id.to_string(),
                "action": "updated",
                "actor_id": caller.actor.clone(),
                "name": updated.name.clone(),
                "brand_name": updated.brand_name.clone(),
            }),
        );
        Ok(updated)
    })();
    json::respond(result, debug)
}

async fn delete_custom_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CardsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CARDS_MANAGE)?;
        let tenant = tenant_id(&req)?;
        let id = Uuid::parse_str(path.into_inner().trim())
            .map_err(|_| UnifiedError::bad_request("Invalid custom rule id: expected a UUID"))?;
        let removed = state
            .rules
            .get(&id)
            .map(|entry| entry.value().clone())
            .filter(|rule| rule.tenant_id == tenant)
            .ok_or_else(|| UnifiedError::not_found("Custom rule not found"))?;
        state.rules.remove(&id);
        ctx.hooks.emit(
            CARDS_CUSTOM_RULE_CHANGED,
            json!({
                "tenant_id": removed.tenant_id,
                "id": removed.id.to_string(),
                "action": "deleted",
                "actor_id": caller.actor.clone(),
                "name": removed.name,
                "brand_name": removed.brand_name,
            }),
        );
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

async fn list_counts(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CardsState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<DailyCountView>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CARDS_READ)?;
        let tenant = tenant_id(&req)?;
        let params = query.into_inner();
        let mut items: Vec<DailyCountView> = state
            .counts
            .iter()
            .filter(|entry| entry.key().0 == tenant)
            .map(|entry| DailyCountView {
                date: entry.key().1.clone(),
                matched_rule: entry.key().2.clone(),
                result: entry.key().3.clone(),
                count: *entry.value(),
            })
            .filter(|item| {
                params
                    .filters
                    .get("date")
                    .is_none_or(|wanted| &item.date == wanted)
                    && params
                        .filters
                        .get("matched_rule")
                        .is_none_or(|wanted| &item.matched_rule == wanted)
                    && params
                        .filters
                        .get("result")
                        .is_none_or(|wanted| &item.result == wanted)
            })
            .collect();
        let sort = params.allowed_sort(&["date", "matched_rule", "result", "count"], "date");
        items.sort_by(|a, b| match sort.as_str() {
            "matched_rule" => a.matched_rule.cmp(&b.matched_rule),
            "result" => a.result.cmp(&b.result),
            "count" => a.count.cmp(&b.count),
            _ => a.date.cmp(&b.date),
        });
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Module.
// ---------------------------------------------------------------------------

pub struct CardsModule {
    state: Arc<CardsState>,
}

impl CardsModule {
    pub fn new() -> Self {
        Self { state: Arc::new(CardsState::new()) }
    }
}

impl Default for CardsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for CardsModule {
    fn id(&self) -> &'static str {
        "cards"
    }

    fn name(&self) -> &'static str {
        "Card Validation"
    }

    fn description(&self) -> &'static str {
        "Card validation: Luhn checksum, brand detection from standard IIN ranges, expiry and CVV checks, administrator-defined custom brand rules, test-number handling (live/sandbox), and daily validation counts. Stateless: never stores a PAN or CVV; only masked PANs leave the module."
    }

    fn available_as_product(&self) -> bool {
        false
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(ctx.hooks.clone());
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_validate_requested(state.clone(), ctx));
        bus.register(CARDS_VALIDATE_REQUESTED, "cards", handler);
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/cards")
                // Literal segments first: they must win over `/{id}`.
                .route("/validate", web::post().to(validate))
                .route("/brands", web::get().to(list_brands))
                .route("/brands/{id}", web::get().to(get_brand))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings))
                .route("/custom-rules", web::get().to(list_custom_rules))
                .route("/custom-rules", web::post().to(create_custom_rule))
                .route("/custom-rules/{id}", web::get().to(get_custom_rule))
                .route("/custom-rules/{id}", web::put().to(update_custom_rule))
                .route("/custom-rules/{id}", web::delete().to(delete_custom_rule))
                .route("/counts", web::get().to(list_counts)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "POST", path: "/api/v1/cards/validate", owner: "cards", description: "Validate a card number (Luhn, brand, length, expiry, CVV, test-number mode); returns the masked PAN and generic reason codes only." },
            RouteInfo { method: "GET", path: "/api/v1/cards/brands", owner: "cards", description: "List standard card brands with lengths, CVV length, active and enabled state for this tenant." },
            RouteInfo { method: "GET", path: "/api/v1/cards/brands/{id}", owner: "cards", description: "Get one standard card brand." },
            RouteInfo { method: "GET", path: "/api/v1/cards/settings", owner: "cards", description: "Get the tenant card validation settings (test mode, expiry bound, GPN, disabled brands)." },
            RouteInfo { method: "PUT", path: "/api/v1/cards/settings", owner: "cards", description: "Update the tenant card validation settings." },
            RouteInfo { method: "GET", path: "/api/v1/cards/custom-rules", owner: "cards", description: "List custom brand rules (paginated; filter by enabled, overrides_standard_brand; search by name or brand)." },
            RouteInfo { method: "POST", path: "/api/v1/cards/custom-rules", owner: "cards", description: "Create a custom brand rule (IIN prefixes/ranges, lengths, CVV length, optional standard-brand override)." },
            RouteInfo { method: "GET", path: "/api/v1/cards/custom-rules/{id}", owner: "cards", description: "Get one custom brand rule." },
            RouteInfo { method: "PUT", path: "/api/v1/cards/custom-rules/{id}", owner: "cards", description: "Update a custom brand rule." },
            RouteInfo { method: "DELETE", path: "/api/v1/cards/custom-rules/{id}", owner: "cards", description: "Delete a custom brand rule." },
            RouteInfo { method: "GET", path: "/api/v1/cards/counts", owner: "cards", description: "List daily validation counts (paginated; filter by date, matched_rule, result). No PANs." },
        ]
    }
}

/// Register the module. The integrator wires this into
/// `modules::register_all` (this file only defines the module; it
/// does not edit `mod.rs`).
pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(CardsModule::new()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> CardsState {
        CardsState::new()
    }

    fn tenant() -> &'static str {
        "00000000-0000-7000-8000-000000000001"
    }

    fn validate(state: &CardsState, number: &str) -> CardValidationResult {
        validate_card(
            state,
            tenant(),
            &ValidateInput { number: number.to_string(), expiry: None, cvv: None },
        )
    }

    #[test]
    fn luhn_hand_check() {
        // Hand-checked sums: 80 valid, 79 invalid, 60 valid, 61 invalid.
        assert_eq!(luhn_checksum("4242424242424242"), Some(80));
        assert!(luhn_valid("4242424242424242"));
        assert_eq!(luhn_checksum("4242424242424241"), Some(79));
        assert!(!luhn_valid("4242424242424241"));
        assert_eq!(luhn_checksum("378282246310005"), Some(60));
        assert!(luhn_valid("378282246310005"));
        assert_eq!(luhn_checksum("378282246310006"), Some(61));
        assert!(!luhn_valid("378282246310006"));
        assert!(luhn_valid("5555555555554444"));
        assert!(luhn_valid("6011111111111117"));
        assert!(luhn_valid("3056930009020004"));
        assert!(luhn_valid("3530111333300000"));
        assert_eq!(luhn_checksum(""), None);
        assert_eq!(luhn_checksum("4242x"), None);
    }

    #[test]
    fn masking_never_shows_middle_digits() {
        assert_eq!(mask_pan("4242424242424242"), "424242******4242");
        assert_eq!(mask_pan("378282246310005"), "378282*****0005");
        assert_eq!(mask_pan("12345"), "*****");
    }

    #[test]
    fn normalisation_strips_spaces_and_dashes_only() {
        assert_eq!(
            normalise_pan("4242 4242-4242 4242").unwrap(),
            "4242424242424242"
        );
        assert!(normalise_pan("4242.4242").is_err());
        assert!(normalise_pan("4242x").is_err());
    }

    #[test]
    fn brand_detection_precedence() {
        let settings = CardsSettings::default();
        // Visa Electron beats Visa on its prefixes.
        assert_eq!(
            detect_standard_brand("4026123456789012", &settings).unwrap().rule.id,
            "visa_electron"
        );
        assert_eq!(
            detect_standard_brand("4242424242424242", &settings).unwrap().rule.id,
            "visa"
        );
        // Mastercard beats Diners Club on 55.
        assert_eq!(
            detect_standard_brand("5555555555554444", &settings).unwrap().rule.id,
            "mastercard"
        );
        // Maestro UK beats Maestro and Switch on 6759.
        assert_eq!(
            detect_standard_brand("6759641234567890", &settings).unwrap().rule.id,
            "maestro_uk"
        );
        // Discover beats Troy and RuPay on a bare 65.
        assert_eq!(
            detect_standard_brand("6500000000000002", &settings).unwrap().rule.id,
            "discover"
        );
        // LankaPay beats JCB by longest prefix.
        assert_eq!(
            detect_standard_brand("3571111234567890", &settings).unwrap().rule.id,
            "lankapay"
        );
        // Elo and HiperCard resolve from their verified prefixes.
        assert_eq!(
            detect_standard_brand("5066991234567890", &settings).unwrap().rule.id,
            "elo"
        );
        assert_eq!(
            detect_standard_brand("3841001234567890", &settings).unwrap().rule.id,
            "hipercard"
        );
        // GPN is skipped while disabled.
        assert!(detect_standard_brand("5012345678901234", &settings).is_none());
        let mut gpn_settings = CardsSettings::default();
        gpn_settings.gpn_enabled = true;
        assert_eq!(
            detect_standard_brand("5012345678901234", &gpn_settings)
                .unwrap()
                .rule
                .id,
            "gpn"
        );
        // Nothing matches.
        assert!(detect_standard_brand("9999999999999999", &settings).is_none());
    }

    #[test]
    fn validation_flow_and_test_numbers() {
        let state = state();
        // Live mode rejects the well-known test PANs.
        let result = validate(&state, "4242424242424242");
        assert!(!result.valid);
        assert_eq!(result.reason_codes, vec!["test_number_rejected"]);
        assert_eq!(result.masked_pan, "424242******4242");
        assert_eq!(result.matched_rule, "standard:visa");
        // Sandbox mode accepts the same number.
        let mut settings = CardsSettings::default();
        settings.test_mode = TestMode::Sandbox;
        state.settings.insert(tenant().to_string(), settings);
        let result = validate(&state, "4242 4242 4242 4242");
        assert!(result.valid);
        assert_eq!(result.brand_id.as_deref(), Some("visa"));
        assert!(result.reason_codes.is_empty());
        // A bad Luhn digit fails with the generic code only.
        let result = validate(&state, "4242424242424241");
        assert!(!result.valid);
        assert_eq!(result.reason_codes, vec!["invalid_luhn"]);
        // Unknown brand.
        let result = validate(&state, "9999999999999999");
        assert!(!result.valid);
        assert_eq!(result.reason_codes, vec!["unknown_brand"]);
        // Wrong generic length.
        let result = validate(&state, "4242424242");
        assert!(!result.valid);
        assert_eq!(result.reason_codes, vec!["invalid_length"]);
        // Illegal characters.
        let result = validate(&state, "4242.4242.4242.4242");
        assert!(!result.valid);
        assert_eq!(result.reason_codes, vec!["invalid_format"]);
        // Amex needs a 4-digit CVV.
        let amex = validate_card(
            &state,
            tenant(),
            &ValidateInput {
                number: "378282246310005".to_string(),
                expiry: None,
                cvv: Some("123".to_string()),
            },
        );
        assert!(!amex.valid);
        assert_eq!(amex.reason_codes, vec!["invalid_cvv"]);
        let amex = validate_card(
            &state,
            tenant(),
            &ValidateInput {
                number: "378282246310005".to_string(),
                expiry: None,
                cvv: Some("1234".to_string()),
            },
        );
        assert!(amex.valid);
        assert_eq!(amex.brand_id.as_deref(), Some("american_express"));
    }

    #[test]
    fn expiry_validation() {
        let now = Utc::now();
        assert!(expiry_is_valid("12/40", 20, now));
        assert!(!expiry_is_valid("12/99", 20, now));
        assert!(!expiry_is_valid("13/30", 20, now));
        assert!(!expiry_is_valid("00/30", 20, now));
        assert!(!expiry_is_valid("01/20", 20, now));
        assert!(!expiry_is_valid("01/99", 0, now));
        assert!(!expiry_is_valid("1/30", 20, now));
        assert!(!expiry_is_valid("01-30", 20, now));
        assert!(expiry_is_valid("01/2030", 20, now));
    }

    #[test]
    fn custom_rule_override_replaces_standard_lengths() {
        let state = state();
        let now = Utc::now();
        let rule = CardCustomRule {
            id: Uuid::now_v7(),
            tenant_id: tenant().to_string(),
            name: "Strict Visa".to_string(),
            brand_name: "Visa (custom)".to_string(),
            iin_prefixes: vec!["4".to_string()],
            iin_ranges: Vec::new(),
            allowed_lengths: vec![16],
            cvv_length: 3,
            luhn_required: true,
            enabled: true,
            overrides_standard_brand: Some("visa".to_string()),
            created_at: now,
            updated_at: now,
        };
        state.rules.insert(rule.id, rule);
        // Standard Visa allows 13, 16, 19; the override allows 16 only.
        let short = validate(&state, "4222222222222");
        assert!(!short.valid);
        assert_eq!(short.reason_codes, vec!["invalid_length"]);
        assert!(short.matched_rule.starts_with("custom:"));
    }
}
