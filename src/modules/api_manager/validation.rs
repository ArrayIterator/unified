// Api Manager - Endpoint, version, path and payload validation.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use actix_web::HttpRequest;
use regex::Regex;
use serde_json::{json, Value};
use std::sync::OnceLock;

use super::{ApiManagerState, EndpointRule, HTTP_METHODS, RULE_TYPES, RegisteredEndpoint,
    VersionSupport};

/// Permissions guarding the admin surface (`/api/v1/api-manager/`,
/// except the public gateway `resolve_public`). Reads require
/// `PERM_API_MANAGER_READ`; creates, updates, deletes, key
/// operations and `execute` require `PERM_API_MANAGER_MANAGE`.
/// The roles module must seed these capabilities for admin roles;
/// until then `role.check` answers nothing and every admin call
/// fails closed with 403.
pub(crate) const PERM_API_MANAGER_READ: &str = "api-manager:read";
pub(crate) const PERM_API_MANAGER_MANAGE: &str = "api-manager:manage";

// Caller identity and capability checks (same shape as the cards /
// apikeys modules: `X-User-Id` identifies the actor, `X-Role`
// carries the role slug the capability check runs against,
// `role.check` answers; no answer means not allowed -- fail closed).

/// The authenticated admin caller, resolved from request headers.
pub(crate) struct Caller {
    pub(crate) actor: String,
    pub(crate) role: String,
}

pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
pub(crate) fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub(crate) fn require_permission(
    ctx: &AppContext,
    role: &str,
    permission: &str,
) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

/// Resolve the caller from the request headers and require
/// `permission` for them. Use `PERM_API_MANAGER_READ` for reads and
/// `PERM_API_MANAGER_MANAGE` for mutations.
pub(crate) fn require_admin(
    ctx: &AppContext,
    req: &HttpRequest,
    permission: &str,
) -> UnifiedResult<Caller> {
    let found = caller(req)?;
    require_permission(ctx, &found.role, permission)?;
    Ok(found)
}

pub(crate) fn validate_method(method: &str) -> Result<String, UnifiedError> {
    let method = method.trim().to_uppercase();
    if !HTTP_METHODS.contains(&method.as_str()) {
        return Err(UnifiedError::bad_request(format!(
            "method must be one of {}",
            HTTP_METHODS.join(", ")
        )));
    }
    Ok(method)
}

pub(crate) fn validate_path(path: &str) -> Result<String, UnifiedError> {
    let path = path.trim();
    if !path.starts_with('/') {
        return Err(UnifiedError::bad_request("path must start with '/'"));
    }
    if path.contains(char::is_whitespace) {
        return Err(UnifiedError::bad_request("path must not contain whitespace"));
    }
    if path.chars().count() > 500 {
        return Err(UnifiedError::bad_request(
            "path must be at most 500 characters",
        ));
    }
    Ok(path.to_string())
}

/// Fixed validation patterns, compiled once and shared by every call.
/// (The user-supplied `pattern` in `validate_rules` / `validate_payload`
/// is still compiled per call: it is caller-supplied text, not a fixed
/// pattern, so it cannot live in a static.)
fn version_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"^v\d+(\.\d+)*$|^\d+\.\d+\.\d+$").expect("static regex is valid")
    })
}

fn segment_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"^[A-Za-z0-9_-]+$").expect("static regex is valid")
    })
}

fn field_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"^[a-zA-Z0-9_]+(\.[a-zA-Z0-9_]+)*$").expect("static regex is valid")
    })
}

pub(crate) fn validate_version(version: &str) -> Result<String, UnifiedError> {
    let version = version.trim();
    // Accept `v1`, `v1.2`, or a plain semver string such as `1.2.3`.
    let version_pattern = version_pattern();
    if !version_pattern.is_match(version) {
        return Err(UnifiedError::bad_request(
            "version must look like \"v1\", \"v1.2\", or a semver string such as \"1.2.3\"",
        ));
    }
    Ok(version.to_string())
}

/// Numeric part of a registry version id (`v10` -> 10). Returns
/// `None` for anything that is not `v` followed by a positive
/// number. Legacy endpoint versions such as `v1.2` or `1.2.3` are
/// not registry ids and return `None` here on purpose.
pub(crate) fn parse_version_number(version: &str) -> Option<u32> {
    let number = version.strip_prefix('v')?;
    if number.is_empty() || !number.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let parsed: u32 = number.parse().ok()?;
    if parsed == 0 {
        None
    } else {
        Some(parsed)
    }
}

/// Sort version ids (`v1`, `v2`, `v10`) by version number, so `v10`
/// sorts after `v2` instead of before it.
pub(crate) fn sort_version_ids(ids: &mut Vec<String>) {
    ids.sort_by_key(|id| parse_version_number(id).unwrap_or(0));
}

/// Validate a version-registry id: strictly `v` plus a positive
/// number (`v1`, `v2`, `v10`). This is stricter than the legacy
/// endpoint `version` field above, which also accepts `v1.2` and
/// semver strings for backward compatibility.
pub(crate) fn validate_registry_version_id(version: &str) -> Result<String, UnifiedError> {
    let version = version.trim();
    if parse_version_number(version).is_none() {
        return Err(UnifiedError::bad_request(
            "version id must look like \"v1\", \"v2\", or \"v10\" (\"v\" followed by a positive number)",
        ));
    }
    Ok(version.to_string())
}

/// Validate the global public API prefix: one or more segments of
/// letters, digits, `-` or `_`, joined by `/`, with no leading or
/// trailing slash and no whitespace. Examples: `"v"`, `"api"`,
/// `"api/v2"`. An empty prefix is rejected: it would produce
/// ambiguous `//v1/...` URLs.
pub(crate) fn validate_prefix(prefix: &str) -> Result<String, UnifiedError> {
    let prefix = prefix.trim();
    if prefix.is_empty() {
        return Err(UnifiedError::bad_request("prefix must not be empty"));
    }
    if prefix.starts_with('/') || prefix.ends_with('/') {
        return Err(UnifiedError::bad_request(
            "prefix must not start or end with '/'",
        ));
    }
    if prefix.contains(char::is_whitespace) {
        return Err(UnifiedError::bad_request("prefix must not contain whitespace"));
    }
    if prefix.chars().count() > 100 {
        return Err(UnifiedError::bad_request(
            "prefix must be at most 100 characters",
        ));
    }
    let segment_pattern = segment_pattern();
    for segment in prefix.split('/') {
        if !segment_pattern.is_match(segment) {
            return Err(UnifiedError::bad_request(format!(
                "prefix segment \"{segment}\" is invalid: use only letters, digits, '-' or '_'"
            )));
        }
    }
    Ok(prefix.to_string())
}

/// Normalize a path template for comparison: `{id}` placeholders
/// become `{}`, and a trailing slash (other than the root) is
/// dropped, so `/vcs/providers/{provider}/` and
/// `/vcs/providers/{id}` compare equal.
pub(crate) fn normalize_path_template(path: &str) -> String {
    let trimmed = path.trim();
    let trimmed = trimmed.strip_suffix('/').filter(|_| trimmed.len() > 1).unwrap_or(trimmed);
    trimmed
        .split('/')
        .map(|segment| {
            if segment.starts_with('{') && segment.ends_with('}') {
                "{}"
            } else {
                segment
            }
        })
        .collect::<Vec<&str>>()
        .join("/")
}

/// True when a concrete request path matches a path template:
/// `{placeholder}` segments match any single segment.
pub(crate) fn path_template_matches(template: &str, path: &str) -> bool {
    let template_segments: Vec<&str> = template.split('/').collect();
    let path_segments: Vec<&str> = path.split('/').collect();
    if template_segments.len() != path_segments.len() {
        return false;
    }
    template_segments.iter().zip(path_segments.iter()).all(|(template, actual)| {
        (template.starts_with('{') && template.ends_with('}')) || template == actual
    })
}

/// Strip a public or internal prefix from a request path and return
/// the version-relative path the resolver works with, plus the
/// version it carried when one was present. Accepts:
/// `/{prefix}/{version}/...` (public), `/api/v1/...` (internal,
/// which carries version `v1` explicitly in its path), and bare
/// version-relative paths (`/...`).
///
/// `None` means the path carries no parseable version at all. Callers
/// must NOT silently default that case to `"v1"`: an unprefixed path
/// would then resolve against the wrong version. `check_public_access`
/// rejects a version-less path instead; `resolve_handler` falls back
/// to its explicit `version` query parameter, which is a caller
/// supplied version, not a silent default.
pub(crate) fn strip_public_prefix(state: &ApiManagerState, raw_path: &str) -> (Option<String>, String) {
    let path = raw_path.trim();
    let prefix = state
        .prefix
        .read()
        .map(|settings| settings.prefix.clone())
        .unwrap_or_else(|_| "v".to_string());
    let public_root = format!("/{prefix}/");
    if let Some(rest) = path.strip_prefix(&public_root) {
        let mut parts = rest.splitn(2, '/');
        let version = parts.next().unwrap_or("");
        let tail = parts.next().unwrap_or("");
        if parse_version_number(version).is_some() {
            return (Some(version.to_string()), format!("/{tail}"));
        }
    }
    if let Some(rest) = path.strip_prefix("/api/v1") {
        // The `v1` segment is an explicit version, not a default.
        return (Some("v1".to_string()), rest.to_string());
    }
    (None, path.to_string())
}

/// Validate a `VersionSupport` declaration against the registry:
/// every referenced version must be a strict id that exists in the
/// registry, lists must be non-empty, and ranges must be ordered.
/// Lists are normalized (de-duplicated, sorted by version number).
pub(crate) fn validate_version_support(
    state: &ApiManagerState,
    support: VersionSupport,
) -> Result<VersionSupport, UnifiedError> {
    let known = state.version_ids();
    let require_known = |version: &str| -> Result<String, UnifiedError> {
        let version = validate_registry_version_id(version)?;
        if !known.iter().any(|entry| entry == &version) {
            return Err(UnifiedError::bad_request(format!(
                "version \"{version}\" does not exist in the API version registry"
            ))
            .with_metadata("unknown_version", json!(version))
            .with_metadata("available_versions", json!(known)));
        }
        Ok(version)
    };
    match support {
        VersionSupport::All => Ok(VersionSupport::All),
        VersionSupport::Min { min_version } => Ok(VersionSupport::Min {
            min_version: require_known(&min_version)?,
        }),
        VersionSupport::Max { max_version } => Ok(VersionSupport::Max {
            max_version: require_known(&max_version)?,
        }),
        VersionSupport::Range { min_version, max_version } => {
            let min_version = require_known(&min_version)?;
            let max_version = require_known(&max_version)?;
            let min_number = parse_version_number(&min_version).unwrap_or(0);
            let max_number = parse_version_number(&max_version).unwrap_or(0);
            if min_number > max_number {
                return Err(UnifiedError::bad_request(
                    "min_version must not be newer than max_version",
                ));
            }
            Ok(VersionSupport::Range { min_version, max_version })
        }
        VersionSupport::List { versions } => {
            if versions.is_empty() {
                return Err(UnifiedError::bad_request(
                    "versions must contain at least one version",
                ));
            }
            if versions.len() > 50 {
                return Err(UnifiedError::bad_request(
                    "versions must contain at most 50 versions",
                ));
            }
            let mut normalized: Vec<String> = Vec::new();
            for version in &versions {
                let version = require_known(version)?;
                if !normalized.contains(&version) {
                    normalized.push(version);
                }
            }
            sort_version_ids(&mut normalized);
            Ok(VersionSupport::List { versions: normalized })
        }
    }
}

/// Resolve the version support for a create/update endpoint input.
/// Precedence: `version_support` > `versions` > legacy `version`.
/// The legacy `version` field keeps its old validation; when it is
/// a strict registry id it must also exist in the registry, while a
/// legacy `v1.2` / semver value is stored as-is for backward
/// compatibility and is not required to exist in the registry.
pub(crate) fn resolve_input_support(
    state: &ApiManagerState,
    version: &Option<String>,
    versions: &Option<Vec<String>>,
    version_support: &Option<VersionSupport>,
) -> Result<VersionSupport, UnifiedError> {
    if let Some(support) = version_support {
        return validate_version_support(state, support.clone());
    }
    if let Some(versions) = versions {
        return validate_version_support(
            state,
            VersionSupport::List { versions: versions.clone() },
        );
    }
    match version {
        Some(version) => {
            let version = validate_version(version)?;
            if parse_version_number(&version).is_some() {
                let known = state.version_ids();
                if !known.iter().any(|entry| entry == &version) {
                    return Err(UnifiedError::bad_request(format!(
                        "version \"{version}\" does not exist in the API version registry"
                    ))
                    .with_metadata("unknown_version", json!(version))
                    .with_metadata("available_versions", json!(known)));
                }
            }
            Ok(VersionSupport::List { versions: vec![version] })
        }
        None => Err(UnifiedError::bad_request(
            "provide one of: version, versions, or version_support",
        )),
    }
}

/// Build the 422 collision error for a custom endpoint that
/// collides with a built-in endpoint. The conflicting endpoint and
/// its owner module are named, and the overlapping versions are in
/// the metadata.
pub(crate) fn builtin_collision_error(
    method: &str,
    path: &str,
    conflicting: &RegisteredEndpoint,
    overlapping: &[String],
) -> UnifiedError {
    UnifiedError::unprocessable(format!(
        "endpoint {method} {path} already exists as a built-in endpoint owned by module \"{}\" ({} {})",
        conflicting.owner_module, conflicting.method, conflicting.path
    ))
    .with_metadata(
        "conflicting_endpoint",
        json!(format!("{} {}", conflicting.method, conflicting.path)),
    )
    .with_metadata("owner_module", json!(conflicting.owner_module))
    .with_metadata("versions", json!(overlapping))
}

/// Emit `api.endpoint.conflict` so the admin UI and audit log can
/// show a rejected custom-endpoint creation.
pub(crate) fn emit_endpoint_conflict(
    ctx: &AppContext,
    method: &str,
    path: &str,
    conflicting: &RegisteredEndpoint,
    overlapping: &[String],
) {
    ctx.hooks.emit(
        "api.endpoint.conflict",
        json!({
            "method": method,
            "path": path,
            "conflicting_endpoint": format!("{} {}", conflicting.method, conflicting.path),
            "owner_module": conflicting.owner_module,
            "versions": overlapping,
        }),
    );
}

pub(crate) fn validate_rules(rules: &[EndpointRule]) -> Result<(), UnifiedError> {
    let field_pattern = field_pattern();
    for rule in rules {
        if !field_pattern.is_match(&rule.field) {
            return Err(UnifiedError::bad_request(format!(
                "rule field \"{}\" is not a valid dotted field name",
                rule.field
            )));
        }
        if !RULE_TYPES.contains(&rule.field_type.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "rule field \"{}\": type must be one of {}",
                rule.field,
                RULE_TYPES.join(", ")
            )));
        }
        if let (Some(min), Some(max)) = (rule.min_length, rule.max_length) {
            if min > max {
                return Err(UnifiedError::bad_request(format!(
                    "rule field \"{}\": min_length must not exceed max_length",
                    rule.field
                )));
            }
        }
        if let (Some(min), Some(max)) = (rule.min_value, rule.max_value) {
            if min > max {
                return Err(UnifiedError::bad_request(format!(
                    "rule field \"{}\": min_value must not exceed max_value",
                    rule.field
                )));
            }
        }
        if let Some(pattern) = &rule.pattern {
            Regex::new(pattern).map_err(|_| {
                UnifiedError::bad_request(format!(
                    "rule field \"{}\": pattern is not a valid regular expression",
                    rule.field
                ))
            })?;
        }
    }
    Ok(())
}

/// Look a dotted field up in a JSON object payload.
pub(crate) fn lookup_field<'a>(payload: &'a Value, field: &str) -> Option<&'a Value> {
    let mut current = payload;
    for segment in field.split('.') {
        current = current.get(segment)?;
    }
    Some(current)
}

pub(crate) fn type_matches(value: &Value, field_type: &str) -> bool {
    match field_type {
        "string" => value.is_string(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        _ => false,
    }
}

/// Validate a payload against the endpoint rules. Returns the list of
/// problems; an empty list means the payload is valid.
pub(crate) fn validate_payload(payload: &Value, rules: &[EndpointRule]) -> Vec<String> {
    let mut errors: Vec<String> = Vec::new();
    if !payload.is_object() {
        errors.push("payload must be a JSON object".to_string());
        return errors;
    }
    for rule in rules {
        let value = lookup_field(payload, &rule.field);
        let Some(value) = value else {
            if rule.required {
                errors.push(format!("field \"{}\" is required", rule.field));
            }
            continue;
        };
        if value.is_null() {
            if rule.required {
                errors.push(format!("field \"{}\" must not be null", rule.field));
            }
            continue;
        }
        if !type_matches(value, &rule.field_type) {
            errors.push(format!(
                "field \"{}\" must be of type {}",
                rule.field, rule.field_type
            ));
            continue;
        }
        if let Some(text) = value.as_str() {
            let len = text.chars().count();
            if let Some(min) = rule.min_length {
                if len < min {
                    errors.push(format!(
                        "field \"{}\" must be at least {min} characters long",
                        rule.field
                    ));
                }
            }
            if let Some(max) = rule.max_length {
                if len > max {
                    errors.push(format!(
                        "field \"{}\" must be at most {max} characters long",
                        rule.field
                    ));
                }
            }
            if let Some(pattern) = &rule.pattern {
                if let Ok(regex) = Regex::new(pattern) {
                    if !regex.is_match(text) {
                        errors.push(format!(
                            "field \"{}\" does not match the required pattern",
                            rule.field
                        ));
                    }
                }
            }
        }
        if let Some(number) = value.as_f64() {
            if let Some(min) = rule.min_value {
                if number < min {
                    errors.push(format!("field \"{}\" must be at least {min}", rule.field));
                }
            }
            if let Some(max) = rule.max_value {
                if number > max {
                    errors.push(format!("field \"{}\" must be at most {max}", rule.field));
                }
            }
        }
    }
    errors
}


#[cfg(test)]
mod tests {

    /// Build a request carrying the server-verified caller the auth
    /// middleware would attach for this user and role. The old
    /// self-asserted identity headers grant nothing,
    /// so tests attach the verified caller to the request
    /// extensions instead.
    fn verified_request(user_id: &str, role: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid(user_id), Vec::new())
                .with_roles(vec![role.to_string()]),
        );
        req
    }

    /// Build a request carrying a verified caller with no roles
    /// resolved: the caller resolves, but holds the anonymous role.
    /// This is the "no role" case in the new model (roles are
    /// resolved server-side, never claimed in a header).
    fn verified_request_without_roles(user_id: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::auth::Caller::verified(
                test_user_uuid(user_id),
                Vec::new(),
            ));
        req
    }

    /// Deterministic test-user identity: distinct names map to
    /// distinct UUIDs (FNV-1a folded into the low bits), so
    /// ownership tests can still tell two users apart. A value
    /// that already parses as a UUID is used as-is.
    fn test_user_uuid(name: &str) -> uuid::Uuid {
        if let Ok(id) = uuid::Uuid::parse_str(name) {
            return id;
        }
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in name.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        uuid::Uuid::from_u128(u128::from(hash) | 0x0000_0000_0000_4000_8000_0000_0000_0000)
    }

    use super::*;

    fn rule(field: &str, field_type: &str, required: bool) -> EndpointRule {
        EndpointRule {
            field: field.to_string(),
            required,
            field_type: field_type.to_string(),
            min_length: None,
            max_length: None,
            min_value: None,
            max_value: None,
            pattern: None,
        }
    }

    fn seeded_state() -> ApiManagerState {
        let state = ApiManagerState::default();
        state.ensure_seeded_versions();
        state
    }

    #[test]
    fn validate_method_uppercases() {
        assert_eq!(validate_method("get").unwrap(), "GET");
    }

    #[test]
    fn validate_method_rejects_unknown() {
        assert!(validate_method("FETCH").is_err());
    }

    #[test]
    fn validate_path_accepts_rooted() {
        assert_eq!(validate_path("/v1/things").unwrap(), "/v1/things");
    }

    #[test]
    fn validate_path_rejects_relative() {
        assert!(validate_path("v1/things").is_err());
    }

    #[test]
    fn validate_path_rejects_whitespace() {
        assert!(validate_path("/v1/a b").is_err());
    }

    #[test]
    fn validate_version_accepts_v1() {
        assert_eq!(validate_version("v1").unwrap(), "v1");
    }

    #[test]
    fn validate_version_accepts_semver() {
        assert_eq!(validate_version("1.2.3").unwrap(), "1.2.3");
    }

    #[test]
    fn validate_version_rejects_bare_number() {
        assert!(validate_version("1").is_err());
    }

    #[test]
    fn parse_version_number_v10() {
        assert_eq!(parse_version_number("v10"), Some(10));
    }

    #[test]
    fn parse_version_number_rejects_v0() {
        assert_eq!(parse_version_number("v0"), None);
    }

    #[test]
    fn parse_version_number_rejects_dotted() {
        assert_eq!(parse_version_number("v1.2"), None);
    }

    #[test]
    fn parse_version_number_rejects_bare() {
        assert_eq!(parse_version_number("10"), None);
    }

    #[test]
    fn sort_version_ids_orders_numerically() {
        let mut ids = vec!["v10".to_string(), "v1".to_string(), "v2".to_string()];
        sort_version_ids(&mut ids);
        assert_eq!(ids, vec!["v1", "v2", "v10"]);
    }

    #[test]
    fn validate_registry_version_id_accepts_v2() {
        assert_eq!(validate_registry_version_id("v2").unwrap(), "v2");
    }

    #[test]
    fn validate_registry_version_id_rejects_dotted() {
        assert!(validate_registry_version_id("v1.2").is_err());
    }

    #[test]
    fn validate_prefix_accepts_nested() {
        assert_eq!(validate_prefix("api/v2").unwrap(), "api/v2");
    }

    #[test]
    fn validate_prefix_rejects_leading_slash() {
        assert!(validate_prefix("/api").is_err());
    }

    #[test]
    fn validate_prefix_rejects_empty() {
        assert!(validate_prefix("").is_err());
    }

    #[test]
    fn validate_prefix_rejects_bad_segment() {
        assert!(validate_prefix("api/v.2").is_err());
    }

    #[test]
    fn normalize_path_template_replaces_placeholders() {
        assert_eq!(
            normalize_path_template("/vcs/providers/{provider}/"),
            "/vcs/providers/{}"
        );
    }

    #[test]
    fn normalize_path_template_keeps_root_slash() {
        assert_eq!(normalize_path_template("/"), "/");
    }

    #[test]
    fn path_template_matches_placeholder_segment() {
        assert!(path_template_matches("/a/{id}/b", "/a/42/b"));
    }

    #[test]
    fn path_template_matches_rejects_different_length() {
        assert!(!path_template_matches("/a/{id}", "/a/1/2"));
    }

    #[test]
    fn path_template_matches_rejects_literal_mismatch() {
        assert!(!path_template_matches("/a/b", "/a/c"));
    }

    #[test]
    fn lookup_field_walks_dotted_path() {
        let payload = json!({"a": {"b": 42}});
        assert_eq!(lookup_field(&payload, "a.b"), Some(&json!(42)));
        assert_eq!(lookup_field(&payload, "a.missing"), None);
    }

    #[test]
    fn type_matches_string_and_integer() {
        assert!(type_matches(&json!("x"), "string"));
        assert!(!type_matches(&json!(1), "string"));
        assert!(type_matches(&json!(1), "integer"));
        assert!(type_matches(&json!(1.5), "number"));
        assert!(!type_matches(&json!(1.5), "integer"));
        assert!(!type_matches(&json!("x"), "unknown-type"));
    }

    #[test]
    fn validate_rules_accepts_valid_rule() {
        assert!(validate_rules(&[rule("name", "string", true)]).is_ok());
    }

    #[test]
    fn repeated_validation_reuses_compiled_patterns() {
        // The version, segment and field patterns are compiled once
        // and shared: repeated calls must agree exactly with the
        // first call.
        for _ in 0..3 {
            assert_eq!(validate_version("v1").unwrap(), "v1");
            assert_eq!(validate_version("1.2.3").unwrap(), "1.2.3");
            assert!(validate_version("1").is_err());
            assert_eq!(validate_prefix("api/v2").unwrap(), "api/v2");
            assert!(validate_prefix("/api").is_err());
            assert!(validate_rules(&[rule("user.name", "string", true)]).is_ok());
            assert!(validate_rules(&[rule("bad-name!", "string", true)]).is_err());
        }
    }

    #[test]
    fn validate_rules_rejects_unknown_type() {
        assert!(validate_rules(&[rule("name", "date", true)]).is_err());
    }

    #[test]
    fn validate_rules_rejects_inverted_length_bounds() {
        let mut bad = rule("name", "string", false);
        bad.min_length = Some(5);
        bad.max_length = Some(2);
        assert!(validate_rules(&[bad]).is_err());
    }

    #[test]
    fn validate_rules_rejects_invalid_pattern() {
        let mut bad = rule("name", "string", false);
        bad.pattern = Some("([invalid".to_string());
        assert!(validate_rules(&[bad]).is_err());
    }

    #[test]
    fn validate_payload_accepts_valid() {
        let payload = json!({"name": "x", "age": 30});
        let rules = vec![rule("name", "string", true), rule("age", "integer", false)];
        assert!(validate_payload(&payload, &rules).is_empty());
    }

    #[test]
    fn validate_payload_reports_missing_required() {
        let payload = json!({});
        let errors = validate_payload(&payload, &[rule("name", "string", true)]);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("name"));
    }

    #[test]
    fn validate_payload_reports_wrong_type() {
        let payload = json!({"age": "old"});
        let errors = validate_payload(&payload, &[rule("age", "integer", false)]);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("age"));
    }

    #[test]
    fn validate_payload_reports_short_string() {
        let payload = json!({"name": ""});
        let mut short = rule("name", "string", true);
        short.min_length = Some(1);
        let errors = validate_payload(&payload, &[short]);
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn validate_payload_rejects_non_object() {
        let errors = validate_payload(&json!([1, 2]), &[rule("name", "string", true)]);
        assert_eq!(errors, vec!["payload must be a JSON object".to_string()]);
    }

    #[test]
    fn validate_version_support_all_is_ok() {
        let state = seeded_state();
        assert_eq!(
            validate_version_support(&state, VersionSupport::All).unwrap(),
            VersionSupport::All
        );
    }

    #[test]
    fn validate_version_support_rejects_unknown_min() {
        let state = seeded_state();
        let support = VersionSupport::Min {
            min_version: "v99".to_string(),
        };
        assert!(validate_version_support(&state, support).is_err());
    }

    #[test]
    fn validate_version_support_rejects_inverted_range() {
        let state = seeded_state();
        let support = VersionSupport::Range {
            min_version: "v2".to_string(),
            max_version: "v1".to_string(),
        };
        assert!(validate_version_support(&state, support).is_err());
    }

    #[test]
    fn validate_version_support_list_normalizes() {
        let state = seeded_state();
        let support = VersionSupport::List {
            versions: vec!["v2".to_string(), "v1".to_string(), "v2".to_string()],
        };
        assert_eq!(
            validate_version_support(&state, support).unwrap(),
            VersionSupport::List {
                versions: vec!["v1".to_string(), "v2".to_string()],
            }
        );
    }

    #[test]
    fn resolve_input_support_prefers_explicit_support() {
        let state = seeded_state();
        let out = resolve_input_support(&state, &None, &None, &Some(VersionSupport::All)).unwrap();
        assert_eq!(out, VersionSupport::All);
    }

    #[test]
    fn resolve_input_support_requires_something() {
        let state = seeded_state();
        assert!(resolve_input_support(&state, &None, &None, &None).is_err());
    }

    #[test]
    fn resolve_input_support_legacy_version_must_exist_when_strict() {
        let state = seeded_state();
        let out = resolve_input_support(&state, &Some("v1".to_string()), &None, &None).unwrap();
        assert_eq!(
            out,
            VersionSupport::List {
                versions: vec!["v1".to_string()],
            }
        );
        assert!(
            resolve_input_support(&state, &Some("v99".to_string()), &None, &None).is_err()
        );
    }

    #[test]
    fn strip_public_prefix_returns_version_for_public_path() {
        let state = seeded_state();
        assert_eq!(
            strip_public_prefix(&state, "/v/v2/things"),
            (Some("v2".to_string()), "/things".to_string())
        );
    }

    #[test]
    fn strip_public_prefix_returns_v1_for_internal_path() {
        // The `v1` in `/api/v1/...` is an explicit version in the
        // path, not a silent default.
        let state = seeded_state();
        assert_eq!(
            strip_public_prefix(&state, "/api/v1/cards"),
            (Some("v1".to_string()), "/cards".to_string())
        );
    }

    #[test]
    fn strip_public_prefix_returns_none_for_bare_path() {
        let state = seeded_state();
        assert_eq!(
            strip_public_prefix(&state, "/things"),
            (None, "/things".to_string())
        );
    }

    #[test]
    fn strip_public_prefix_returns_none_for_unparseable_public_version() {
        let state = seeded_state();
        assert_eq!(
            strip_public_prefix(&state, "/v/notaversion/things"),
            (None, "/v/notaversion/things".to_string())
        );
    }

    // Caller identity and permission checks (same test shape as the
    // apikeys module: a `role.check` hook that allows exactly the
    // given permissions).
    use unified_core::config::AppConfig;
    use unified_core::hooks::{HookBus, HookContext};
    use unified_core::modules::ModuleRegistry;
    use actix_web::HttpRequest;
    use actix_web::test::TestRequest;
    use std::sync::Arc;

    fn test_ctx(allowed: &[&str]) -> Arc<AppContext> {
        let hooks = Arc::new(HookBus::new());
        let allowed: Vec<String> = allowed
            .iter()
            .map(|permission| permission.to_string())
            .collect();
        hooks.register(
            "role.check",
            "test",
            Arc::new(move |ctx: &mut HookContext| {
                let permission = ctx
                    .get("permission")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                ctx.set(
                    "allowed",
                    serde_json::json!(allowed.iter().any(|held| held == permission)),
                );
            }),
        );
        Arc::new(AppContext::new(
            AppConfig::default(),
            hooks,
            Arc::new(ModuleRegistry::new()),
        ))
    }

    fn authed_request() -> HttpRequest {
        verified_request("tester", "admin")
    }

    fn anonymous_request() -> HttpRequest {
        TestRequest::default().to_http_request()
    }

    #[test]
    fn caller_accepts_a_verified_caller() {
        let found = caller(&authed_request()).unwrap();
        assert_eq!(found.actor, test_user_uuid("tester").to_string());
        assert_eq!(found.role, "admin");
    }

    #[test]
    fn caller_rejects_anonymous_request() {
        let error = caller(&anonymous_request()).unwrap_err();
        assert_eq!(error.code, 401);
    }

    #[test]
    fn caller_rejects_missing_role_header() {
        let no_role = TestRequest::default().to_http_request();
        assert_eq!(caller(&no_role).unwrap_err().code, 401);
    }

    #[test]
    fn require_permission_fails_closed_without_a_role_check_handler() {
        let ctx = test_ctx(&[]);
        assert!(!check_capability(&ctx, "admin", PERM_API_MANAGER_MANAGE));
        assert!(require_permission(&ctx, "admin", PERM_API_MANAGER_MANAGE).is_err());
    }

    #[test]
    fn require_admin_rejects_anonymous_callers() {
        let ctx = test_ctx(&[PERM_API_MANAGER_MANAGE]);
        let error = require_admin(&ctx, &anonymous_request(), PERM_API_MANAGER_MANAGE)
            .unwrap_err();
        assert_eq!(error.code, 401);
    }

    #[test]
    fn require_admin_rejects_callers_without_the_permission() {
        let ctx = test_ctx(&[PERM_API_MANAGER_READ]);
        let error = require_admin(&ctx, &authed_request(), PERM_API_MANAGER_MANAGE)
            .unwrap_err();
        assert_eq!(error.code, 403);
    }

    #[test]
    fn require_admin_accepts_callers_with_the_permission() {
        let ctx = test_ctx(&[PERM_API_MANAGER_MANAGE]);
        let found = require_admin(&ctx, &authed_request(), PERM_API_MANAGER_MANAGE).unwrap();
        assert_eq!(found.actor, test_user_uuid("tester").to_string());
        assert_eq!(found.role, "admin");
    }
}
