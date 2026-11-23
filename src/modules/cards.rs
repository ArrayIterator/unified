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

pub mod handlers;
pub mod module;
mod service;
pub mod state;
#[cfg(test)]
mod tests;
pub mod types;
pub mod validation;

pub use self::handlers::BrandView;
pub use self::handlers::CreateCustomRuleRequest;
pub use self::handlers::DailyCountView;
pub use self::handlers::UpdateCustomRuleRequest;
pub use self::handlers::UpdateSettingsRequest;
pub use self::handlers::ValidateRequest;
pub(crate) use self::handlers::create_custom_rule;
pub(crate) use self::handlers::delete_custom_rule;
pub(crate) use self::handlers::get_brand;
pub(crate) use self::handlers::get_custom_rule;
pub(crate) use self::handlers::get_settings;
pub(crate) use self::handlers::list_brands;
pub(crate) use self::handlers::list_counts;
pub(crate) use self::handlers::list_custom_rules;
pub(crate) use self::handlers::update_custom_rule;
pub(crate) use self::handlers::update_settings;
pub(crate) use self::handlers::validate;
pub use self::module::CardsModule;
pub use self::module::register;
pub(crate) use self::service::caller;
pub(crate) use self::service::matches_search;
pub(crate) use self::service::on_validate_requested;
pub(crate) use self::service::paginate;
pub(crate) use self::service::require_permission;
pub(crate) use self::service::require_text;
pub(crate) use self::service::caller_company;
pub(crate) use self::service::require_company;
pub(crate) use self::service::require_tenant;
pub use self::state::CardsState;
pub use self::types::BrandMatch;
pub use self::types::BrandRule;
pub use self::types::CARDS_CUSTOM_RULE_CHANGED;
pub use self::types::CARDS_VALIDATE_REQUESTED;
pub use self::types::CARDS_VALIDATED;
pub use self::types::CardCustomRule;
pub use self::types::CardsSettings;
pub use self::types::DEFAULT_MAX_EXPIRY_YEARS_AHEAD;
pub use self::types::ELO_PREFIXES;
pub use self::types::HIPERCARD_PREFIXES;
pub use self::types::IinRange;
pub use self::types::IinRangeInput;
pub use self::types::MAX_EXPIRY_YEARS_AHEAD_LIMIT;
pub use self::types::MAX_PAN_LENGTH;
pub use self::types::MAX_RAW_NUMBER_LENGTH;
pub use self::types::MIN_PAN_LENGTH;
pub use self::types::PERM_CARDS_MANAGE;
pub use self::types::PERM_CARDS_READ;
pub use self::types::PERM_CARDS_VALIDATE;
pub use self::types::STANDARD_BRANDS;
pub use self::types::TEST_PANS;
pub use self::types::TestMode;
pub(crate) use self::types::custom_rule_match_len;
pub use self::types::detect_standard_brand;
pub use self::types::known_standard_brand;
pub use self::validation::CardValidationResult;
pub use self::validation::ValidateInput;
pub use self::validation::cvv_is_valid;
pub use self::validation::expiry_is_valid;
pub use self::validation::luhn_checksum;
pub use self::validation::luhn_valid;
pub use self::validation::mask_pan;
pub use self::validation::normalise_pan;
pub(crate) use self::validation::numberless_masked_pan;
pub use self::validation::validate_and_record;
pub use self::validation::validate_card;
pub use self::validation::validate_numberless;
pub use self::validation::validate_numberless_and_record;
