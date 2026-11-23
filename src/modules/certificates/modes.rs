//! Certificates - certificate-mode registry and Cloudflare detection types.
//!
//! Part of the `certificates` module split; see `src/modules/certificates.rs`.

use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::error::UnifiedResult;
use unified_core::json;
use actix_web::HttpRequest;
use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use serde_json::json;
use std::collections::HashSet;
use std::net::IpAddr;

use super::cert_events;
use super::types::DEFAULT_CF_RANGES_V4_URL;
use super::types::DEFAULT_CF_RANGES_V6_URL;

// ---------------------------------------------------------------------------
// Certificate modes (data-driven registry) and Cloudflare detection.
// ---------------------------------------------------------------------------

/// One certificate-mode definition in the admin-extensible registry.
/// The built-in modes are seed data only (see
/// `default_cert_mode_definitions`); an admin may add, rename, or
/// disable modes through `PUT /cert-modes/definitions` without a code
/// change. Behaviour is driven by the flags, never by matching the
/// mode id in code: `requires_acme` triggers the existing ACME
/// renewal, `requires_upload` expects a PEM upload, and
/// `requires_cloudflare` marks the mode as a Cloudflare origin mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CertModeDefinition {
    /// Mode id, for example `letsencrypt`, `custom`, `cloudflare`.
    pub id: String,
    pub label: String,
    pub description: String,
    /// Setting this mode expects a certificate/key PEM upload.
    pub requires_upload: bool,
    /// Setting this mode issues/renews through the ACME provider
    /// registry (`AcmeConfig::resolve_provider`).
    pub requires_acme: bool,
    /// This mode is a Cloudflare origin-certificate mode.
    pub requires_cloudflare: bool,
    /// Disabled definitions cannot be newly selected; existing mode
    /// records that reference one keep their stored value.
    pub enabled: bool,
}

/// Seed definitions for the certificate-mode registry. Seed data, not
/// a closed list: the registry is replaced through
/// `PUT /cert-modes/definitions`.
pub fn default_cert_mode_definitions() -> Vec<CertModeDefinition> {
    vec![
        CertModeDefinition {
            id: "letsencrypt".to_string(),
            label: "Let's Encrypt (automatic)".to_string(),
            description: "Automatic issuance and renewal through the configured ACME provider.".to_string(),
            requires_upload: false,
            requires_acme: true,
            requires_cloudflare: false,
            enabled: true,
        },
        CertModeDefinition {
            id: "custom".to_string(),
            label: "Custom certificate upload".to_string(),
            description: "Upload a certificate PEM and private key PEM; validated before install.".to_string(),
            requires_upload: true,
            requires_acme: false,
            requires_cloudflare: false,
            enabled: true,
        },
        CertModeDefinition {
            id: "cloudflare".to_string(),
            label: "Cloudflare origin certificate".to_string(),
            description: "Origin-certificate mode for domains served through Cloudflare; an origin certificate is expected and Full (Strict) mode is required for end-to-end encryption.".to_string(),
            requires_upload: false,
            requires_acme: false,
            requires_cloudflare: true,
            enabled: true,
        },
    ]
}

/// Validate a replacement mode registry: non-empty, unique ids, ids
/// are lowercase slugs, every definition has a label.
pub(crate) fn validate_mode_definitions(definitions: &[CertModeDefinition]) -> Result<(), String> {
    if definitions.is_empty() {
        return Err("at least one certificate mode definition is required".to_string());
    }
    let mut seen: HashSet<String> = HashSet::new();
    for definition in definitions {
        let id = definition.id.trim();
        if id.is_empty()
            || !id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(format!(
                "invalid mode id \"{id}\": use lowercase letters, digits, and hyphens"
            ));
        }
        if !seen.insert(id.to_string()) {
            return Err(format!("duplicate mode id \"{id}\""));
        }
        if definition.label.trim().is_empty() {
            return Err(format!("mode \"{id}\" has an empty label"));
        }
    }
    Ok(())
}

/// One domain's certificate mode: the current row of the
/// `domain_cert_modes` table (mode history is the earlier rows for
/// the same domain). Private keys and PEM material are never stored
/// here: only fingerprints and metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainCertMode {
    pub domain: String,
    /// Mode id from the registry (`letsencrypt`, `custom`,
    /// `cloudflare`, or an admin-defined id).
    pub mode: String,
    /// ACME provider key, `custom`, `cloudflare`, or the mode id.
    pub provider: String,
    /// SHA-256 fingerprint of the installed certificate, if any.
    pub fingerprint_sha256: String,
    pub installed_at: Option<DateTime<Utc>>,
    pub previous_fingerprint: Option<String>,
    /// `active`, `pending`, `awaiting_upload`, or `error`.
    pub status: String,
    pub last_error: Option<String>,
    pub updated_at: DateTime<Utc>,
    pub updated_by: String,
}

/// Stored Cloudflare IP-range snapshot: DATA, not code. The ranges
/// are fetched from the published lists (see the settings URLs),
/// stored with their fetch date, and refreshed through
/// `POST /cloudflare-ranges/refresh` (or automatically on first use).
/// Until the first successful fetch the lists are empty and the DNS
/// detection method honestly reports `ranges_unavailable` instead of
/// pretending a bundled snapshot is current.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CfIpRanges {
    pub v4: Vec<String>,
    pub v6: Vec<String>,
    pub fetched_at: Option<DateTime<Utc>>,
    pub source_v4: String,
    pub source_v6: String,
    pub fetch_error: Option<String>,
}

impl Default for CfIpRanges {
    fn default() -> Self {
        Self {
            v4: Vec::new(),
            v6: Vec::new(),
            fetched_at: None,
            source_v4: DEFAULT_CF_RANGES_V4_URL.to_string(),
            source_v6: DEFAULT_CF_RANGES_V6_URL.to_string(),
            fetch_error: None,
        }
    }
}

/// One stored Cloudflare detection result for a domain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudflareDetection {
    pub domain: String,
    pub detected: bool,
    /// Per-method results (`dns`, `probe`, `api`), each honest about
    /// what it checked, found, or could not check.
    pub methods: Value,
    /// Cloudflare SSL mode reported by the API check, when known:
    /// `flexible`, `full`, or `full_strict`.
    pub ssl_mode: Option<String>,
    /// `false` in Flexible mode (no origin encryption), `true` in
    /// Full / Full (Strict) mode, `None` when unconfirmed.
    pub end_to_end_encrypted: Option<bool>,
    /// Set whenever end-to-end encryption is absent or unconfirmed,
    /// so the system NEVER claims full encryption in Flexible mode.
    pub flexible_warning: Option<String>,
    pub checked_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SetCertModeRequest {
    pub(crate) mode: String,
    #[serde(default)]
    pub(crate) provider: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CustomUploadRequest {
    /// Certificate chain in PEM format.
    pub(crate) cert_pem: String,
    /// Private key in PEM format. Never logged or returned.
    pub(crate) key_pem: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateModeDefinitionsRequest {
    pub(crate) modes: Vec<CertModeDefinition>,
}

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_CERTIFICATES_VIEW: &str = "certificates.view";

pub(crate) const PERM_CERTIFICATES_MANAGE: &str = "certificates.manage";

// NOTE: the `certificates.manage_own` capability still exists in the
// roles registry, but no code in this module references it while the
// tenant branch of `authorize_domain` is removed (see that function):
// it is re-introduced with the branch once a domain-ownership
// provider is registered.

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the properties module.
/// `pub(crate)` (not private): the split put the handlers in sibling
/// modules, and they read `actor` / `role` for authorization.
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

/// Ask the SaaS/domains side (through the `domain.ownership.check`
/// hook) whether `actor` owns the verified `domain`. Fails closed:
/// no answer means not owned.
///
/// NOTE: no module in the repo answers this hook yet (verified
/// 2026-11-22: no listener in the SaaS/domains side), so this always
/// returns `false` today. It is kept for the tenant list filtering
/// in `list_cert_modes` and for the day a listener lands; do not
/// treat a `false` here as proof the caller does not own the domain.
/// The cross-module gap (a listener answering from the verified-alias
/// store) is tracked as follow-up work outside this module.
pub(crate) fn domain_owned(ctx: &AppContext, actor: &str, domain: &str) -> bool {
    let context = ctx.hooks.emit(
        cert_events::DOMAIN_OWNERSHIP_CHECK,
        json!({ "domain": domain, "user_id": actor, "owned": false }),
    );
    context
        .get("owned")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

/// Authorize one domain-scoped call. An admin (role holding
/// `certificates.manage`) may act on any domain.
///
/// Tenant access is currently UNAVAILABLE, not merely denied: the
/// tenant branch (`certificates.manage_own` / `certificates.view`
/// plus a confirming answer from the `domain.ownership.check` hook)
/// is dead code in practice because no module answers that hook, so
/// every non-admin tenant call was denied anyway -- but with a
/// misleading "not owned" error that implied an ownership check had
/// run. The branch is removed here rather than left to pretend: a
/// non-admin caller now gets an explicit error saying tenant-scoped
/// access is unavailable until an ownership provider is registered.
/// Restore the tenant branch (permission check plus
/// `domain_owned`) once such a listener exists.
pub(crate) fn authorize_domain(
    ctx: &AppContext,
    req: &HttpRequest,
    domain: &str,
    write: bool,
) -> UnifiedResult<Caller> {
    let caller = caller(req)?;
    if check_capability(ctx, &caller.role, PERM_CERTIFICATES_MANAGE) {
        return Ok(caller);
    }
    let _ = write;
    Err(UnifiedError::forbidden(format!(
        "Tenant-scoped access to domain \"{domain}\" is unavailable: no domain-ownership provider is registered, so only a role holding \"{PERM_CERTIFICATES_MANAGE}\" may manage certificates"
    )))
}

/// Parse one CIDR notation entry (`192.0.2.0/24`,
/// `2001:db8::/32`) into (network address, prefix length).
pub(crate) fn parse_cidr(cidr: &str) -> Option<(IpAddr, u8)> {
    let (address, prefix) = cidr.trim().split_once('/')?;
    let network: IpAddr = address.trim().parse().ok()?;
    let prefix: u8 = prefix.trim().parse().ok()?;
    match network {
        IpAddr::V4(_) if prefix > 32 => None,
        IpAddr::V6(_) if prefix > 128 => None,
        _ => Some((network, prefix)),
    }
}

/// True when `ip` lies inside the `cidr` range. Address families
/// must match; a malformed range never matches.
pub(crate) fn cidr_contains(cidr: &str, ip: &IpAddr) -> bool {
    let Some((network, prefix)) = parse_cidr(cidr) else {
        return false;
    };
    match (network, ip) {
        (IpAddr::V4(network), IpAddr::V4(address)) => {
            let mask = if prefix == 0 {
                0u32
            } else {
                u32::MAX << (32 - prefix)
            };
            (u32::from(network) & mask) == (u32::from(*address) & mask)
        }
        (IpAddr::V6(network), IpAddr::V6(address)) => {
            let mask = if prefix == 0 {
                0u128
            } else {
                u128::MAX << (128 - prefix)
            };
            (u128::from(network) & mask) == (u128::from(*address) & mask)
        }
        _ => false,
    }
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
    use unified_core::config::AppConfig;
    use unified_core::hooks::HookBus;
    use unified_core::hooks::HookContext;
    use unified_core::modules::ModuleRegistry;
    use actix_web::test::TestRequest;
    use std::sync::Arc;

    /// App context whose `role.check` hook allows exactly the given
    /// permissions (all other permissions are denied).
    fn test_app_context(allowed: &[&str]) -> AppContext {
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
        AppContext::new(
            AppConfig::default(),
            hooks,
            Arc::new(ModuleRegistry::new()),
        )
    }

    fn test_request() -> HttpRequest {
        verified_request("tester", "member")
    }

    #[test]
    fn default_mode_definitions_seed_three_modes() {
        let defs = default_cert_mode_definitions();
        assert_eq!(defs.len(), 3);
        let ids: Vec<&str> = defs.iter().map(|d| d.id.as_str()).collect();
        assert!(ids.contains(&"letsencrypt"));
        assert!(ids.contains(&"custom"));
        assert!(ids.contains(&"cloudflare"));
        let letsencrypt = defs.iter().find(|d| d.id == "letsencrypt").unwrap();
        assert!(letsencrypt.requires_acme);
        assert!(!letsencrypt.requires_upload);
        assert!(!letsencrypt.requires_cloudflare);
        assert!(letsencrypt.enabled);
        let custom = defs.iter().find(|d| d.id == "custom").unwrap();
        assert!(custom.requires_upload);
        assert!(!custom.requires_acme);
        let cloudflare = defs.iter().find(|d| d.id == "cloudflare").unwrap();
        assert!(cloudflare.requires_cloudflare);
        assert!(!cloudflare.requires_acme);
        assert!(!cloudflare.requires_upload);
    }

    #[test]
    fn mode_definition_validation() {
        assert!(validate_mode_definitions(&default_cert_mode_definitions()).is_ok());
        assert!(validate_mode_definitions(&[]).is_err());
        let mut duplicate = default_cert_mode_definitions();
        duplicate.push(duplicate[0].clone());
        assert!(validate_mode_definitions(&duplicate).is_err());
        let mut bad_id = default_cert_mode_definitions();
        bad_id[0].id = "Bad_ID".to_string();
        assert!(validate_mode_definitions(&bad_id).is_err());
        let mut no_label = default_cert_mode_definitions();
        no_label[0].label = "   ".to_string();
        assert!(validate_mode_definitions(&no_label).is_err());
    }

    #[test]
    fn cidr_parsing_and_membership() {
        assert!(parse_cidr("10.0.0.0/8").is_some());
        assert!(parse_cidr("10.0.0.0/33").is_none());
        assert!(parse_cidr("2001:db8::/32").is_some());
        assert!(parse_cidr("not-a-cidr").is_none());
        let inside: IpAddr = "10.1.2.3".parse().unwrap();
        let outside: IpAddr = "11.0.0.1".parse().unwrap();
        assert!(cidr_contains("10.0.0.0/8", &inside));
        assert!(!cidr_contains("10.0.0.0/8", &outside));
        assert!(cidr_contains(
            "192.168.1.0/24",
            &"192.168.1.200".parse().unwrap()
        ));
        assert!(!cidr_contains(
            "192.168.1.0/24",
            &"192.168.2.1".parse().unwrap()
        ));
        assert!(!cidr_contains("garbage", &inside));
        let v6: IpAddr = "2001:db8::1".parse().unwrap();
        assert!(!cidr_contains("10.0.0.0/8", &v6));
    }

    #[test]
    fn domain_owned_is_false_without_an_ownership_listener() {
        // No module answers `domain.ownership.check` yet, so the
        // fail-closed answer is always "not owned".
        let ctx = test_app_context(&[PERM_CERTIFICATES_VIEW]);
        assert!(!domain_owned(&ctx, "tester", "example.com"));
    }

    #[test]
    fn authorize_domain_allows_admin_on_any_domain() {
        let ctx = test_app_context(&[PERM_CERTIFICATES_MANAGE]);
        let req = test_request();
        assert!(authorize_domain(&ctx, &req, "example.com", true).is_ok());
        assert!(authorize_domain(&ctx, &req, "example.com", false).is_ok());
    }

    #[test]
    fn authorize_domain_tenant_branch_is_explicitly_unavailable() {
        // A tenant holding only view / manage_own is denied with an
        // explicit "unavailable" error, not a misleading "not owned"
        // verdict from a check that never ran.
        let ctx = test_app_context(&[PERM_CERTIFICATES_VIEW]);
        let req = test_request();
        let error = authorize_domain(&ctx, &req, "example.com", false).unwrap_err();
        assert_eq!(error.code, 403);
        assert!(error.message.contains("unavailable"));
        assert!(error.message.contains("domain-ownership provider"));
    }

    #[test]
    fn authorize_domain_requires_caller_headers() {
        let ctx = test_app_context(&[PERM_CERTIFICATES_MANAGE]);
        let anonymous = TestRequest::default().to_http_request();
        let error = authorize_domain(&ctx, &anonymous, "example.com", false).unwrap_err();
        assert_eq!(error.code, 401);
    }
}
