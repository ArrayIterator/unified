// cards - Shared types: constants, enums, records, and request/response shapes.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// Events and capabilities.

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
/// `matched_rule`, `valid`, `reason_codes`, `is_test_number`, and
/// `inactive_brand`. Never the full number, expiry, or CVV.
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

// Limits.

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

// Standard brand table (sanctioned hard-coded constants - see the
// module header). Lengths are PAN lengths in digits; CVV length is
// 4 for American Express and 3 for every other standard brand.

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
/// Every entry below is a canonical publisher test number: each
/// one matches its brand's IIN table and passes Luhn, so in live
/// mode it reaches the test-number check and is rejected there
/// instead of slipping through as a valid real card.
pub const TEST_PANS: &[&str] = &[
    // Visa.
    "4242424242424242",
    "4111111111111111",
    "4012888888881881",
    "4222222222222",
    // Mastercard.
    "5555555555554444",
    "5500005555555559",
    "2223000048400011",
    // American Express.
    "378282246310005",
    "371449635398431",
    "340000000000009",
    // Discover.
    "6011111111111117",
    "6011000990139424",
    "6011000991300009",
    // Diners Club.
    "3056930009020004",
    "38520000023237",
    "36700102000000",
    // JCB.
    "3530111333300000",
    "3566002020360505",
    // UnionPay.
    "6250941006528599",
];

// Settings (per-tenant, data-driven).

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
/// lives here - never in code - except the standard IIN tables
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

// Custom rules (administrator-defined, stored as data).

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
pub(crate) fn custom_rule_match_len(rule: &CardCustomRule, digits: &str) -> Option<usize> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn rule() -> CardCustomRule {
        CardCustomRule {
            id: Uuid::new_v4(),
            tenant_id: "tenant".to_string(),
            name: "Custom".to_string(),
            brand_name: "Custom Brand".to_string(),
            iin_prefixes: vec!["4".to_string(), "42".to_string()],
            iin_ranges: vec![IinRangeInput {
                start: "51".to_string(),
                end: "55".to_string(),
            }],
            allowed_lengths: vec![16],
            cvv_length: 3,
            luhn_required: true,
            enabled: true,
            overrides_standard_brand: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn custom_rule_match_len_prefers_longest() {
        let rule = rule();
        assert_eq!(custom_rule_match_len(&rule, "4242424242424242"), Some(2));
        assert_eq!(custom_rule_match_len(&rule, "5300000000000000"), Some(2));
        assert_eq!(custom_rule_match_len(&rule, "9999999999999999"), None);
        // A range whose bounds cannot parse never matches.
        let mut broken = rule();
        broken.iin_prefixes = Vec::new();
        broken.iin_ranges = vec![IinRangeInput {
            start: "xx".to_string(),
            end: "yy".to_string(),
        }];
        assert_eq!(custom_rule_match_len(&broken, "9999999999999999"), None);
        // A range with unequal bound lengths never matches.
        broken.iin_ranges = vec![IinRangeInput {
            start: "5".to_string(),
            end: "55".to_string(),
        }];
        assert_eq!(custom_rule_match_len(&broken, "5500000000000000"), None);
    }

    #[test]
    fn known_standard_brand_lookup() {
        assert!(known_standard_brand("visa"));
        assert!(known_standard_brand("mastercard"));
        assert!(!known_standard_brand("bogus-brand"));
        assert!(!known_standard_brand(""));
    }
}
