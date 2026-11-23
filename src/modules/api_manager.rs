//! Api Manager module — API Manager module.
//!
//! Split into submodules by concern; the public API is
//! re-exported below. The full route map and module notes
//! live in `api_manager/module.rs`.

pub mod ai_capabilities;
pub mod types;
pub mod state;
mod validation;
mod middleware;
mod handlers_endpoints;
mod handlers_versions;
mod handlers_keys;
pub mod module;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::{ApiEndpoint, CreateApiVersionInput, CreateEndpointInput, CreateKeyInput,
    DeprecateVersionInput, EndpointRule, UpdateApiVersionInput, UpdateEndpointInput,
    UpdatePrefixInput};
pub(crate) use self::types::{API_KEY_HEADER, AccessAuditRow, AccessRuleRow, ApiAccessAuditEntry,
    ApiAccessRule, ApiPrefixSettings, ApiVersion, CreateAccessRuleInput, DEFAULT_ACCESS_RULES,
    HTTP_METHODS, MAX_ACCESS_AUDIT_ENTRIES, MAX_RATE_COUNTER_ENTRIES, RULE_TYPES,
    RegisteredEndpoint, RegistryRow, ResolveQuery, ResolvedEndpoint, UpdateAccessRuleInput,
    UpdateKeyAccessInput, VersionChoice, VersionSupport, default_endpoint_source,
    default_owner_module, default_true};
pub use self::state::{ApiManagerState};
pub(crate) use self::validation::{builtin_collision_error, emit_endpoint_conflict, lookup_field,
    normalize_path_template, parse_version_number, path_template_matches, resolve_input_support,
    sort_version_ids, strip_public_prefix, type_matches, validate_method, validate_path,
    validate_payload, validate_prefix, validate_registry_version_id, validate_rules,
    validate_version, validate_version_support};
pub(crate) use self::middleware::{PublicApiAccessMiddleware, rate_limited_error_response};
pub(crate) use self::handlers_endpoints::{create_endpoint, delete_endpoint, execute_endpoint,
    get_endpoint, get_registry_detail, list_endpoints, list_registry, resolve_handler,
    resolve_public, update_endpoint};
pub(crate) use self::handlers_versions::{create_api_version, delete_api_version,
    deprecate_api_version, get_api_version, get_prefix, get_version_choices, list_api_versions,
    list_versions, update_api_version, update_prefix};
pub(crate) use self::handlers_keys::{create_access_rule, create_key, delete_access_rule,
    get_key_access, list_access_audit, list_access_rules, list_keys, on_apikey_created,
    on_apikey_revoked, on_apikey_rotated, revoke_key, update_access_rule, update_key_access};
pub use self::module::{ApiManagerModule, register};
