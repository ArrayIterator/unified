// cards — Validation helpers and small pure utilities.

use super::*;
use unified_core::hooks::HookContext;
use chrono::{DateTime, Datelike, Utc};
use serde::{Deserialize, Serialize};

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
    /// True when the detected standard brand is inactive
    /// (currently only Laser): the number still validates, but
    /// callers should warn instead of treating it as a live
    /// brand. False for active brands, custom rules, and
    /// validations where no brand was detected.
    pub inactive_brand: bool,
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
            inactive_brand: false,
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
                let mut result = CardValidationResult::invalid(
                    masked,
                    format!("standard:{}", brand_match.rule.id),
                    "brand_disabled",
                    Some(brand_match.rule.id.to_string()),
                    Some(brand_match.rule.name.to_string()),
                    false,
                    &test_mode,
                );
                result.inactive_brand = !brand_match.rule.active;
                return result;
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
    // An inactive standard brand (Laser) still validates, but the
    // result must say so; a custom rule replacing it is the
    // effective brand, so it is not inactive.
    let inactive_brand = matches!(&matched, MatchedRule::Standard(rule) if !rule.active);

    let is_test_number = TEST_PANS.contains(&digits.as_str());
    if is_test_number && settings.test_mode == TestMode::Live {
        let mut result = CardValidationResult::invalid(
            masked,
            matched_rule_name,
            "test_number_rejected",
            Some(brand_id),
            Some(brand_name),
            true,
            &test_mode,
        );
        result.inactive_brand = inactive_brand;
        return result;
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
        result.inactive_brand = inactive_brand;
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
        result.inactive_brand = inactive_brand;
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
            result.inactive_brand = inactive_brand;
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
            result.inactive_brand = inactive_brand;
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
        inactive_brand,
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
pub(crate) fn numberless_masked_pan(ctx: &HookContext) -> String {
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
    // Same inactive-brand surfacing as the full validator: a
    // claimed Laser still validates, but the result says so.
    let inactive_brand = !rule.active;
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
        let mut result = CardValidationResult::invalid(
            masked_pan,
            matched_rule_name,
            "brand_disabled",
            Some(brand_id),
            Some(brand_name),
            false,
            &test_mode,
        );
        result.inactive_brand = inactive_brand;
        return result;
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
        result.inactive_brand = inactive_brand;
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
        inactive_brand,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cvv_is_valid_checks_length_and_digits() {
        assert!(cvv_is_valid("123", 3));
        assert!(cvv_is_valid("1234", 4));
        assert!(!cvv_is_valid("1234", 3));
        assert!(!cvv_is_valid("12", 3));
        assert!(!cvv_is_valid("12a", 3));
        assert!(!cvv_is_valid("", 3));
    }

    #[test]
    fn mask_raw_input_drops_illegal_characters() {
        assert_eq!(mask_raw_input("4242.4242"), "********");
        assert_eq!(mask_raw_input("4242424242424242"), "424242******4242");
    }

    #[test]
    fn expiry_is_valid_boundaries() {
        let now = Utc::now();
        assert!(expiry_is_valid("12/40", 20, now));
        assert!(expiry_is_valid("01/2030", 20, now));
        assert!(!expiry_is_valid("13/30", 20, now));
        assert!(!expiry_is_valid("00/30", 20, now));
        assert!(!expiry_is_valid("01/20", 20, now));
        assert!(!expiry_is_valid("12/99", 20, now));
        assert!(!expiry_is_valid("1/30", 20, now));
        assert!(!expiry_is_valid("01-30", 20, now));
        assert!(!expiry_is_valid("01/30/01", 20, now));
        // The current month is still valid; the previous month is not.
        let current = format!("{:02}/{:02}", now.month(), now.year() % 100);
        assert!(expiry_is_valid(&current, 20, now));
        let last_month = if now.month() == 1 {
            format!("12/{:02}", (now.year() - 1) % 100)
        } else {
            format!("{:02}/{:02}", now.month() - 1, now.year() % 100)
        };
        assert!(!expiry_is_valid(&last_month, 20, now));
    }
}
