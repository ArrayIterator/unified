// cards - Unit tests.

use super::*;
use unified_core::hooks::HookContext;
use serde_json::json;
use std::sync::Arc;

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
    // (4000000000006 is a Luhn-valid 13-digit Visa, not a test PAN.)
    let short = validate(&state, "4000000000006");
    assert!(!short.valid);
    assert_eq!(short.reason_codes, vec!["invalid_length"]);
    assert!(short.matched_rule.starts_with("custom:"));
}

#[test]
fn canonical_publisher_test_pans_rejected_in_live_mode() {
    // The four canonical test PANs from the inspection report:
    // Luhn sums hand-checked as 30, 40, 80, and 50.
    assert_eq!(luhn_checksum("4111111111111111"), Some(30));
    assert_eq!(luhn_checksum("5500005555555559"), Some(40));
    assert_eq!(luhn_checksum("371449635398431"), Some(80));
    assert_eq!(luhn_checksum("6011000990139424"), Some(50));
    for pan in [
        "4111111111111111",
        "5500005555555559",
        "371449635398431",
        "6011000990139424",
    ] {
        assert!(
            TEST_PANS.contains(&pan),
            "{pan} must be listed as a test PAN"
        );
    }
    // Every listed test PAN is rejected in live mode (the default)
    // and accepted in sandbox mode - none slips through as a
    // valid real card.
    let state = state();
    for pan in TEST_PANS {
        let result = validate(&state, pan);
        assert!(!result.valid, "{pan} must be rejected in live mode");
        assert_eq!(result.reason_codes, vec!["test_number_rejected"]);
        assert!(result.is_test_number);
        assert_ne!(result.masked_pan, *pan, "the full PAN must never leak");
    }
    let mut settings = CardsSettings::default();
    settings.test_mode = TestMode::Sandbox;
    state.settings.insert(tenant().to_string(), settings);
    for pan in TEST_PANS {
        let result = validate(&state, pan);
        assert!(result.valid, "{pan} must validate in sandbox mode");
        assert!(result.is_test_number);
    }
}

#[test]
fn inactive_laser_brand_is_flagged() {
    let state = state();
    // 6706000000000004: Luhn-valid (sum 20), Laser prefix 6706,
    // length 16 - a Laser prefix no active brand outranks
    // (Maestro wins only the 6304 tie).
    assert_eq!(luhn_checksum("6706000000000004"), Some(20));
    let result = validate(&state, "6706000000000004");
    assert!(result.valid);
    assert_eq!(result.brand_id.as_deref(), Some("laser"));
    assert_eq!(result.matched_rule, "standard:laser");
    assert!(result.inactive_brand);
    assert_eq!(result.masked_pan, "670600******0004");
    // The flag also survives a later failure (bad CVV here).
    let bad_cvv = validate_card(
        &state,
        tenant(),
        &ValidateInput {
            number: "6706000000000004".to_string(),
            expiry: None,
            cvv: Some("12".to_string()),
        },
    );
    assert!(!bad_cvv.valid);
    assert_eq!(bad_cvv.reason_codes, vec!["invalid_cvv"]);
    assert!(bad_cvv.inactive_brand);
    // An active brand is not flagged, in either validation path.
    let mut settings = CardsSettings::default();
    settings.test_mode = TestMode::Sandbox;
    state.settings.insert(tenant().to_string(), settings);
    let active = validate(&state, "5555555555554444");
    assert!(active.valid);
    assert_eq!(active.brand_id.as_deref(), Some("mastercard"));
    assert!(!active.inactive_brand);
    let numberless = validate_numberless(
        &state,
        tenant(),
        Some("laser"),
        Some("12/40"),
        "******0004".to_string(),
    );
    assert!(numberless.valid);
    assert!(numberless.inactive_brand);
    let numberless_active = validate_numberless(
        &state,
        tenant(),
        Some("visa"),
        Some("12/40"),
        "******4242".to_string(),
    );
    assert!(numberless_active.valid);
    assert!(!numberless_active.inactive_brand);
}

#[test]
fn update_custom_rule_override_can_be_cleared() {
    // Deserialisation: absent means leave unchanged, explicit
    // null means clear, a value means set.
    let absent: UpdateCustomRuleRequest =
        serde_json::from_str("{}").expect("empty update deserialises");
    assert_eq!(absent.overrides_standard_brand, None);
    let cleared: UpdateCustomRuleRequest =
        serde_json::from_str(r#"{"overrides_standard_brand": null}"#)
            .expect("explicit null deserialises");
    assert_eq!(cleared.overrides_standard_brand, Some(None));
    let set: UpdateCustomRuleRequest =
        serde_json::from_str(r#"{"overrides_standard_brand": "visa"}"#)
            .expect("a brand id deserialises");
    assert_eq!(
        set.overrides_standard_brand,
        Some(Some("visa".to_string()))
    );
    // Resolution: absent keeps the existing override, explicit
    // null clears it (the old `.or(existing)` fallback could not
    // do this), a value replaces it.
    let existing = Some("visa".to_string());
    assert_eq!(
        super::handlers::resolve_overrides_standard_brand(None, existing.clone()),
        Some("visa".to_string())
    );
    assert_eq!(
        super::handlers::resolve_overrides_standard_brand(Some(None), existing.clone()),
        None
    );
    assert_eq!(
        super::handlers::resolve_overrides_standard_brand(
            Some(Some("mastercard".to_string())),
            existing.clone(),
        ),
        Some("mastercard".to_string())
    );
    assert_eq!(
        super::handlers::resolve_overrides_standard_brand(Some(None), None),
        None
    );
}

#[test]
fn hook_path_enforces_max_raw_number_length() {
    let state = Arc::new(state());
    // 65 characters: one over the limit. Refused at the gate -
    // no validation, no recording, and the input never echoed.
    let long = "4".repeat(MAX_RAW_NUMBER_LENGTH + 1);
    let mut ctx = HookContext::new(
        CARDS_VALIDATE_REQUESTED,
        json!({ "tenant_id": tenant(), "number": long }),
    );
    on_validate_requested(state.clone(), &mut ctx);
    let result = ctx.get("result").expect("hook sets a result");
    assert_eq!(result["valid"], json!(false));
    assert_eq!(result["reason_codes"], json!(["invalid_length"]));
    assert_eq!(result["masked_pan"], json!(""));
    assert!(ctx.get("error").is_some());
    assert!(state.counts.is_empty(), "a refused input records nothing");
    // Boundary: exactly 64 characters reaches validation instead
    // (and fails there on PAN length, with a masked PAN).
    let boundary = "4".repeat(MAX_RAW_NUMBER_LENGTH);
    let mut ctx = HookContext::new(
        CARDS_VALIDATE_REQUESTED,
        json!({ "tenant_id": tenant(), "number": boundary }),
    );
    on_validate_requested(state.clone(), &mut ctx);
    let result = ctx.get("result").expect("hook sets a result");
    assert!(ctx.get("error").is_none());
    assert_eq!(result["reason_codes"], json!(["invalid_length"]));
    assert_ne!(result["masked_pan"], json!(""));
    assert_eq!(state.counts.len(), 1);
}
