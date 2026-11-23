// Privacy module.
//
// Split into submodules; the full module documentation lives in `module.rs`.

pub mod ai_capabilities;
pub mod redaction;
pub mod types;
pub mod state;
mod helpers;
mod handlers_fields;
mod handlers_anonymization;
mod handlers_consent;
mod handlers_erasure;
mod handlers_notices;
pub mod module;
#[cfg(test)]
mod tests;

pub(crate) use self::redaction::{HmacSha256, PII_KEY_MARKERS, email_re, phone_re, long_digits_re,
    looks_like_date_or_time, redact_value_for_key};
pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::redaction::{PERM_PRIVACY_READ, PERM_PRIVACY_WRITE, PERM_PRIVACY_ADMIN,
    EVENT_USER_ANONYMIZED, EVENT_PRIVACY_ANONYMIZED, EVENT_PRIVACY_CONSENT_CHANGED,
    EVENT_PRIVACY_ERASURE_REQUESTED, EVENT_PRIVACY_ERASURE_FULFILLED,
    EVENT_PRIVACY_VISIBILITY_CHANGED, EVENT_PRIVACY_FIELDS_CHANGED, EVENT_PRIVACY_NOTICE_PUBLISHED,
    EVENT_COMPLIANCE_CONSENT_GRANTED, EVENT_COMPLIANCE_CONSENT_WITHDRAWN,
    EVENT_COMPLIANCE_ERASURE_REQUESTED, EVENT_COMPLIANCE_ERASURE_FULFILLED, GENERIC_NOTICE_TEXT,
    BUILTIN_CONSENT_TYPES, redact_email, redact_phone, redact_id_number, redact_name, redact_text,
    is_pii_key, redact_value};
pub use self::types::{Visibility, default_visibility, FieldDeclaration, AnonymizationRecord,
    VisibilityDefault, ConsentTypeDef, ConsentRecord, ConsentHistoryEntry, ErasureRequest,
    PrivacyNotice, PrivacyAuditEntry, CurrentNotice, EffectiveVisibility, RedactInput,
    CreateFieldDeclarationInput, UpdateFieldDeclarationInput, ValidatePayloadInput, AnonymizeInput,
    SetVisibilityInput, CreateConsentTypeInput, GrantConsentInput, CreateErasureInput,
    RejectErasureInput, CreateNoticeInput, CurrentConsentQuery, CurrentNoticeQuery};
pub use self::state::{PrivacyState};
pub(crate) use self::state::{load_anonymization_key, hex_encode};
pub(crate) use self::helpers::{
    Caller, caller, caller_company, check_capability, matches_search, new_id, now, paginate,
    require_permission, require_tenant, require_text, sort_desc_by_created, valid_field_name,
    valid_slug, validate_module_list,
};
pub(crate) use self::handlers_fields::{redact, list_field_declarations, create_field_declaration,
    get_field_declaration, update_field_declaration, delete_field_declaration, validate_payload};
pub(crate) use self::handlers_anonymization::{anonymize_user, list_anonymizations,
    get_anonymization, list_visibility_defaults, set_visibility_default, get_effective_visibility};
pub(crate) use self::handlers_consent::{list_consent_types, create_consent_type,
    record_consent_change, grant_consent, list_consents, get_current_consent, get_consent,
    withdraw_consent, list_consent_history};
pub(crate) use self::handlers_erasure::{create_erasure_request, list_erasure_requests,
    get_erasure_request, approve_erasure_request, fulfill_erasure_request, reject_erasure_request};
pub(crate) use self::handlers_notices::{list_notices, create_notice, get_current_notice, get_notice,
    list_audit_entries};
pub use self::module::{PrivacyModule, register};
