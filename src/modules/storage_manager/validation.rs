// Validation helpers and quota logic.

use unified_core::config::AppConfig;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::storage::{
    provider_from_config,
    sha256_hex,
    validate_key,
    StorageProvider,
    StorageTargetConfig,
};
use chrono::Utc;
use serde_json::Value;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Validation helpers.
// ---------------------------------------------------------------------------

pub(crate) fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
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

pub(crate) fn optional_text(value: &Option<String>, field: &str, max: usize) -> UnifiedResult<Option<String>> {
    match value {
        None => Ok(None),
        Some(text) => Ok(Some(require_text(text, field, max)?)),
    }
}

pub(crate) fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw)
        .map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

/// A scope id (company, user, or module id as supplied by callers):
/// non-empty, at most 128 characters, no control characters.
pub(crate) fn valid_scope_id(value: &str) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > 128 {
        return Err(UnifiedError::bad_request(
            "Scope id must be 1..=128 characters",
        ));
    }
    if trimmed.chars().any(|c| c.is_control()) {
        return Err(UnifiedError::bad_request(
            "Scope id must not contain control characters",
        ));
    }
    Ok(trimmed.to_string())
}

/// A module id: lowercase letters, digits, `_`, `-`, 1..=128 chars.
pub(crate) fn valid_module_id(value: &str) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > 128 {
        return Err(UnifiedError::bad_request(
            "Module id must be 1..=128 characters",
        ));
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        return Err(UnifiedError::bad_request(
            "Module id must contain only lowercase letters, digits, '_' or '-'",
        ));
    }
    Ok(trimmed.to_string())
}

/// A key prefix inside a bucket: empty (bucket root) or a key-like
/// path without a leading or trailing slash.
pub(crate) fn valid_prefix(value: &str) -> UnifiedResult<String> {
    let trimmed = value.trim().trim_matches('/');
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    validate_key(trimmed)?;
    Ok(trimmed.to_string())
}

pub(crate) fn valid_quota_limit(value: Option<i64>, field: &str) -> UnifiedResult<Option<i64>> {
    match value {
        None => Ok(None),
        Some(limit) if limit < 0 => Err(UnifiedError::bad_request(format!(
            "{field} must not be negative (use null for unlimited)"
        ))),
        Some(limit) => Ok(Some(limit)),
    }
}

pub(crate) fn object_key(bucket_id: Uuid, key: &str) -> String {
    format!("{bucket_id}:{key}")
}

pub(crate) fn quota_key(scope: &str, scope_id: &str) -> String {
    format!("{scope}:{scope_id}")
}

pub(crate) fn load_bucket(state: &StorageManagerState, raw_id: &str) -> UnifiedResult<StorageBucket> {
    let id = parse_uuid(raw_id, "bucket")?;
    state
        .buckets
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Storage bucket not found: {id}")))
}

pub(crate) fn load_binding(state: &StorageManagerState, raw_id: &str) -> UnifiedResult<StorageBinding> {
    let id = parse_uuid(raw_id, "binding")?;
    state
        .bindings
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Storage binding not found: {id}")))
}

pub(crate) fn load_quota(state: &StorageManagerState, raw_id: &str) -> UnifiedResult<StorageQuota> {
    let id = parse_uuid(raw_id, "quota")?;
    state
        .quotas
        .iter()
        .find(|entry| entry.value().id == id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Storage quota not found: {id}")))
}

pub(crate) fn load_rule(state: &StorageManagerState, raw_id: &str) -> UnifiedResult<LifecycleRule> {
    let id = parse_uuid(raw_id, "lifecycle rule")?;
    state
        .lifecycle_rules
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Lifecycle rule not found: {id}")))
}

/// Build the `StorageTargetConfig` for a bucket, decrypting its
/// stored credentials. Only the non-secret fields live on the bucket;
/// the secret fields come from the encrypted credential JSON. The
/// returned config is used in memory only and never serialized.
pub(crate) fn target_config_for_bucket(
    ctx: &AppContext,
    bucket: &StorageBucket,
) -> UnifiedResult<StorageTargetConfig> {
    target_config_for_bucket_with(&ctx.config, bucket)
}

/// Same as `target_config_for_bucket`, but from a bare config handle.
pub(crate) fn target_config_for_bucket_with(
    config: &RwLock<AppConfig>,
    bucket: &StorageBucket,
) -> UnifiedResult<StorageTargetConfig> {
    let mut target = StorageTargetConfig {
        name: format!("storage-manager:{}", bucket.id),
        kind: bucket.provider_kind,
        endpoint: bucket.endpoint.clone(),
        region: bucket.region.clone(),
        bucket: bucket.container.clone(),
        base_dir: bucket.path.clone(),
        sftp_host: bucket.sftp_host.clone(),
        sftp_port: bucket.sftp_port,
        sftp_username: bucket.sftp_username.clone(),
        sftp_auth_kind: bucket.sftp_auth_kind.clone(),
        sftp_keyfile: bucket.sftp_keyfile.clone(),
        sftp_base_path: bucket.path.clone(),
        ..StorageTargetConfig::default()
    };
    if let Some(encrypted) = &bucket.credentials_encrypted {
        let secret = encryption_secret_from(config);
        let plaintext = decrypt_secret_value(&secret, encrypted)?;
        let value: Value = serde_json::from_str(&plaintext).map_err(|_| {
            UnifiedError::internal("Stored bucket credentials cannot be decoded")
        })?;
        let get = |field: &str| {
            value
                .get(field)
                .and_then(|item| item.as_str())
                .unwrap_or("")
                .to_string()
        };
        target.access_key = get("access_key");
        target.secret_key = get("secret_key");
        target.client_id = get("client_id");
        target.client_secret = get("client_secret");
        target.refresh_token = get("refresh_token");
        target.access_token = get("access_token");
        target.token_url = get("token_url");
        target.sftp_password = get("sftp_password");
    }
    Ok(target)
}

/// Build a live provider for a bucket from its stored configuration.
/// The provider is built per call (providers are cheap to construct;
/// no provider instance is cached here, so a bucket edit takes
/// effect on the next call). The bucket's operator-configured
/// `allowed_hosts` are passed as the extra-host allowlist; hosts are
/// never taken from a request.
pub(crate) fn provider_for_bucket(
    ctx: &AppContext,
    bucket: &StorageBucket,
) -> UnifiedResult<Arc<dyn StorageProvider>> {
    provider_for_bucket_with(&ctx.config, bucket)
}

/// Same as `provider_for_bucket`, but from a bare config handle.
pub(crate) fn provider_for_bucket_with(
    config: &RwLock<AppConfig>,
    bucket: &StorageBucket,
) -> UnifiedResult<Arc<dyn StorageProvider>> {
    if !bucket.enabled {
        return Err(UnifiedError::conflict(format!(
            "Storage bucket \"{}\" is disabled",
            bucket.name
        )));
    }
    let target = target_config_for_bucket_with(config, bucket)?;
    // Re-validate the endpoint hostname against the network at
    // request time (DNS rebinding / private-IP check); a hostname
    // that was safe when the bucket was created may resolve
    // differently now.
    if !target.endpoint.trim().is_empty() {
        validate_endpoint_dns(&target.endpoint)?;
    }
    // Only the OPERATOR-configured allowlist (`storage.allowed_hosts`
    // in the application config) extends the endpoint allowlist.
    // The bucket's own `allowed_hosts` are never passed here: they
    // are caller-supplied at bucket creation and must already be a
    // subset of the operator list (see
    // `validate_requested_allowed_hosts`), so a bucket holder cannot
    // allowlist an arbitrary hostname.
    provider_from_config(&target, &operator_allowed_hosts(config))
}

/// Operator-configured endpoint hosts (`storage.allowed_hosts` in
/// the application config). This is the only list that may extend
/// the endpoint allowlist; bucket-level lists never do.
pub(crate) fn operator_allowed_hosts(config: &RwLock<AppConfig>) -> Vec<String> {
    config
        .read()
        .expect("config lock poisoned")
        .storage
        .allowed_hosts
        .iter()
        .map(|host| host.to_ascii_lowercase())
        .collect()
}

/// Reject any bucket-supplied `allowed_hosts` entry that is not on
/// the operator-configured allowlist. Bucket holders may only
/// narrow the operator list, never extend it.
pub(crate) fn validate_requested_allowed_hosts(
    operator_hosts: &[String],
    requested: &[String],
) -> UnifiedResult<()> {
    for host in requested {
        let host = host.to_ascii_lowercase();
        if !operator_hosts
            .iter()
            .any(|allowed| allowed.to_ascii_lowercase() == host)
        {
            return Err(UnifiedError::forbidden(format!(
                "Endpoint host \"{host}\" is not on the operator-configured allowlist (storage.allowed_hosts); a bucket cannot allowlist its own hosts"
            )));
        }
    }
    Ok(())
}

/// Re-validate an endpoint URL's host against the network at
/// request time. IP-literal hosts are rejected unless loopback
/// (same rule as `validate_endpoint_url`); a hostname that resolves
/// to a private, link-local, unspecified or multicast address is
/// rejected, so DNS rebinding cannot smuggle a provider request
/// onto the internal network. Loopback resolution is allowed only
/// for `localhost` (local development endpoints). A host that does
/// not resolve at all fails closed.
pub(crate) fn validate_endpoint_dns(raw: &str) -> UnifiedResult<()> {
    use std::net::{IpAddr, ToSocketAddrs};
    let url = url::Url::parse(raw.trim())
        .map_err(|_| UnifiedError::bad_request("Storage endpoint is not a valid URL"))?;
    let host = url
        .host_str()
        .ok_or_else(|| UnifiedError::bad_request("Storage endpoint has no host"))?
        .to_ascii_lowercase();
    if let Ok(addr) = host.parse::<IpAddr>() {
        if addr.is_loopback() {
            return Ok(());
        }
        return Err(UnifiedError::forbidden(
            "Storage endpoints on IP-literal hosts are not allowed",
        ));
    }
    let addrs: Vec<IpAddr> = (host.as_str(), 443)
        .to_socket_addrs()
        .map_err(|_| {
            UnifiedError::new(
                format!("Storage endpoint host \"{host}\" does not resolve"),
                502,
            )
        })?
        .map(|socket| socket.ip())
        .collect();
    if addrs.is_empty() {
        return Err(UnifiedError::new(
            format!("Storage endpoint host \"{host}\" does not resolve"),
            502,
        ));
    }
    let is_localhost = host == "localhost";
    for addr in &addrs {
        if addr.is_loopback() {
            if !is_localhost {
                return Err(UnifiedError::forbidden(format!(
                    "Storage endpoint host \"{host}\" resolves to a loopback address"
                )));
            }
        } else if addr.is_unspecified()
            || addr.is_multicast()
            || is_private_or_link_local(addr)
        {
            return Err(UnifiedError::forbidden(format!(
                "Storage endpoint host \"{host}\" resolves to a non-public address ({addr})"
            )));
        }
    }
    Ok(())
}

fn is_private_or_link_local(addr: &std::net::IpAddr) -> bool {
    match addr {
        std::net::IpAddr::V4(v4) => {
            v4.is_private() || v4.is_link_local() || v4.is_broadcast() || v4.is_documentation()
        }
        std::net::IpAddr::V6(v6) => {
            // Unique-local (fc00::/7), link-local (fe80::/10) and
            // documentation ranges are not public endpoints.
            (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                || (v6.segments()[0] & 0xffff) == 0x2001 && v6.segments()[1] == 0xdb8
        }
    }
}

/// Net change one upload makes over a previous object of the same
/// key: an overwrite counts only the size delta and zero new files.
/// Pure helper so the delta is computed the same way everywhere
/// (and inside the quota guard — see `upload_object`).
pub(crate) fn upload_delta(previous_size: Option<i64>, size_bytes: i64) -> (i64, i64) {
    match previous_size {
        Some(old) => (size_bytes - old, 0),
        None => (size_bytes, 1),
    }
}

/// Resolve the binding for a module, following the fallback chain:
/// module binding (company-scoped first, then global) -> company
/// default bucket -> global default bucket. Returns the bucket and
/// the key prefix to store under, plus which step resolved it.
pub(crate) fn resolve_binding(
    state: &StorageManagerState,
    module_id: &str,
    company_id: Option<&str>,
) -> Option<(StorageBucket, String, &'static str)> {
    // 1. Module binding, company-scoped first.
    if let Some(company) = company_id {
        if let Some(found) = state.bindings.iter().find(|entry| {
            let binding = entry.value();
            binding.enabled
                && binding.module_id == module_id
                && binding.company_id.as_deref() == Some(company)
        }) {
            let binding = found.value().clone();
            if let Some(bucket) = state.buckets.get(&binding.bucket_id) {
                if bucket.enabled {
                    return Some((bucket.value().clone(), binding.key_prefix, "module"));
                }
            }
        }
    }
    // 2. Module binding, global.
    if let Some(found) = state.bindings.iter().find(|entry| {
        let binding = entry.value();
        binding.enabled && binding.module_id == module_id && binding.company_id.is_none()
    }) {
        let binding = found.value().clone();
        if let Some(bucket) = state.buckets.get(&binding.bucket_id) {
            if bucket.enabled {
                return Some((bucket.value().clone(), binding.key_prefix, "module"));
            }
        }
    }
    // 3. Company default bucket.
    if let Some(company) = company_id {
        if let Some(bucket_id) = state.company_defaults.get(company) {
            let bucket_id = *bucket_id.value();
            if let Some(bucket) = state.buckets.get(&bucket_id) {
                if bucket.enabled {
                    return Some((bucket.value().clone(), String::new(), "company_default"));
                }
            }
        }
    }
    // 4. Global default bucket.
    let global = state
        .global_default
        .read()
        .expect("global default lock poisoned");
    if let Some(bucket_id) = *global {
        if let Some(bucket) = state.buckets.get(&bucket_id) {
            if bucket.enabled {
                return Some((bucket.value().clone(), String::new(), "global_default"));
            }
        }
    }
    None
}

/// Outcome of a quota check for one upload.
pub(crate) struct QuotaCheck {
    /// Hard limit crossed: the upload must be rejected with 409.
    pub exceeded: bool,
    /// Soft limit crossed: warn, but allow the upload.
    pub warning: bool,
    pub scope: String,
    pub scope_id: String,
    pub detail: String,
}

/// The buyer's active storage plan, if any: a company-scoped
/// assignment wins over a personal one. Expired or non-active
/// assignments never apply.
pub(crate) fn active_plan_for_buyer(
    state: &StorageManagerState,
    company_id: Option<&str>,
    user_id: &str,
) -> Option<(StoragePlan, PlanAssignment)> {
    let now = Utc::now();
    let mut personal: Option<PlanAssignment> = None;
    for entry in state.plan_assignments.iter() {
        let assignment = entry.value();
        if assignment.status != "active" {
            continue;
        }
        if assignment
            .expires_at
            .is_some_and(|expires| expires <= now)
        {
            continue;
        }
        if let Some(company) = company_id {
            if assignment.company_id.as_deref() == Some(company) {
                let plan = state.plans.get(&assignment.plan_id)?;
                return Some((plan.value().clone(), assignment.clone()));
            }
        } else if assignment.company_id.is_none() && assignment.user_id == user_id {
            personal = Some(assignment.clone());
        }
    }
    let assignment = personal?;
    let plan = state.plans.get(&assignment.plan_id)?;
    Some((plan.value().clone(), assignment))
}

/// Enforce a buyer's active plan quota on one upload. Returns a
/// hard `QuotaCheck` when the plan would be exceeded, or when the
/// plan does not include the bucket's provider kind; `None` when
/// there is no active plan or the upload fits. Usage is the recorded
/// usage (`buyer_usage`) plus any in-flight reservations for the
/// same buyer, so concurrent uploads cannot over-admit past the plan.
pub(crate) fn check_plan_limits(
    state: &StorageManagerState,
    company_id: Option<&str>,
    user_id: &str,
    provider_kind: Option<&str>,
    delta_bytes: i64,
    delta_files: i64,
) -> Option<QuotaCheck> {
    let (plan, _) = active_plan_for_buyer(state, company_id, user_id)?;
    if let Some(kind) = provider_kind {
        if !plan.included_provider_kinds.is_empty()
            && !plan
                .included_provider_kinds
                .iter()
                .any(|included| included == kind)
        {
            return Some(QuotaCheck {
                exceeded: true,
                warning: false,
                scope: "plan".to_string(),
                scope_id: plan.id.to_string(),
                detail: format!(
                    "Storage plan \"{}\" does not include provider kind \"{kind}\"",
                    plan.name
                ),
            });
        }
    }
    let (used_bytes, used_files) = buyer_usage(state, company_id, user_id);
    let reserved = state
        .plan_reserved
        .get(&assignment_key(company_id, user_id))
        .map(|entry| *entry.value())
        .unwrap_or((0, 0));
    let bytes = used_bytes + reserved.0 + delta_bytes;
    let files = used_files + reserved.1 + delta_files;
    if bytes > plan.quota_bytes {
        return Some(QuotaCheck {
            exceeded: true,
            warning: false,
            scope: "plan".to_string(),
            scope_id: plan.id.to_string(),
            detail: format!(
                "Storage plan \"{}\" quota exceeded: {} bytes stored + {delta_bytes} exceeds the plan quota of {} bytes",
                plan.name, used_bytes, plan.quota_bytes
            ),
        });
    }
    if plan.max_files > 0 && files > plan.max_files {
        return Some(QuotaCheck {
            exceeded: true,
            warning: false,
            scope: "plan".to_string(),
            scope_id: plan.id.to_string(),
            detail: format!(
                "Storage plan \"{}\" file quota exceeded: {} files stored + {delta_files} exceeds the plan limit of {} files",
                plan.name, used_files, plan.max_files
            ),
        });
    }
    None
}

/// Check the quotas that apply to one upload (module, then user,
/// then company, then the buyer's active plan). `delta_bytes` /
/// `delta_files` are the net change the upload makes — an overwrite
/// counts only the size delta and zero new files, so re-uploading
/// the same key cannot double-count. Hard failures win over soft
/// warnings: a soft crossing in an early scope never hides a hard
/// limit in a later scope (the first soft crossing is kept as the
/// warning). The plan check runs last and its hard failure wins
/// over any soft warning.
pub(crate) fn check_quotas(
    state: &StorageManagerState,
    module_id: &str,
    user_id: &str,
    company_id: Option<&str>,
    provider_kind: Option<&str>,
    delta_bytes: i64,
    delta_files: i64,
) -> QuotaCheck {
    let mut scopes: Vec<(&str, String)> = vec![
        ("module", module_id.to_string()),
        ("user", user_id.to_string()),
    ];
    if let Some(company) = company_id {
        scopes.push(("company", company.to_string()));
    }
    let mut warning: Option<QuotaCheck> = None;
    for (scope, scope_id) in scopes {
        let Some(quota) = state.quotas.get(&quota_key(scope, &scope_id)) else {
            continue;
        };
        let quota = quota.value();
        if let Some(limit) = quota.bytes_limit {
            if quota.used_bytes + delta_bytes > limit {
                return QuotaCheck {
                    exceeded: true,
                    warning: false,
                    scope: scope.to_string(),
                    scope_id: scope_id.clone(),
                    detail: format!(
                        "Hard byte quota exceeded for {scope} \"{scope_id}\": used {} + {delta_bytes} exceeds the limit of {limit} bytes",
                        quota.used_bytes
                    ),
                };
            }
        }
        if let Some(limit) = quota.objects_limit {
            if quota.used_objects + delta_files > limit {
                return QuotaCheck {
                    exceeded: true,
                    warning: false,
                    scope: scope.to_string(),
                    scope_id: scope_id.clone(),
                    detail: format!(
                        "Hard object quota exceeded for {scope} \"{scope_id}\": used {} + {delta_files} exceeds the limit of {limit} objects",
                        quota.used_objects
                    ),
                };
            }
        }
        let soft_bytes = quota
            .soft_bytes_limit
            .is_some_and(|limit| quota.used_bytes + delta_bytes > limit);
        let soft_objects = quota
            .soft_objects_limit
            .is_some_and(|limit| quota.used_objects + delta_files > limit);
        if (soft_bytes || soft_objects) && warning.is_none() {
            warning = Some(QuotaCheck {
                exceeded: false,
                warning: true,
                scope: scope.to_string(),
                scope_id: scope_id.clone(),
                detail: format!(
                    "Soft quota crossed for {scope} \"{scope_id}\" (used {} bytes, {} objects)",
                    quota.used_bytes, quota.used_objects
                ),
            });
        }
    }
    if let Some(plan_check) = check_plan_limits(
        state,
        company_id,
        user_id,
        provider_kind,
        delta_bytes,
        delta_files,
    ) {
        // A soft-quota warning computed earlier must not be
        // discarded by the plan result: merge it in so the caller
        // still learns about the soft crossing even when the plan
        // check is the hard failure.
        if let Some(warning) = warning {
            return QuotaCheck {
                exceeded: plan_check.exceeded,
                warning: true,
                scope: plan_check.scope,
                scope_id: plan_check.scope_id,
                detail: format!("{} (also: {})", plan_check.detail, warning.detail),
            };
        }
        return plan_check;
    }
    warning.unwrap_or(QuotaCheck {
        exceeded: false,
        warning: false,
        scope: String::new(),
        scope_id: String::new(),
        detail: String::new(),
    })
}

/// Apply a quota delta to the explicit quota rows that exist for
/// the upload's scopes (module, user, company). Counters are
/// clamped at zero so a release can never drive them negative.
/// Rows that do not exist are skipped: no row means no limit.
pub(crate) fn apply_quota_delta(
    state: &StorageManagerState,
    module_id: &str,
    user_id: &str,
    company_id: Option<&str>,
    delta_bytes: i64,
    delta_files: i64,
) {
    let now = Utc::now();
    for (scope, scope_id) in [
        ("module", module_id.to_string()),
        ("user", user_id.to_string()),
    ] {
        if let Some(mut quota) = state.quotas.get_mut(&quota_key(scope, &scope_id)) {
            quota.used_bytes = (quota.used_bytes + delta_bytes).max(0);
            quota.used_objects = (quota.used_objects + delta_files).max(0);
            quota.updated_at = now;
        }
    }
    if let Some(company) = company_id {
        if let Some(mut quota) = state.quotas.get_mut(&quota_key("company", company)) {
            quota.used_bytes = (quota.used_bytes + delta_bytes).max(0);
            quota.used_objects = (quota.used_objects + delta_files).max(0);
            quota.updated_at = now;
        }
    }
}

/// Reserve quota for one upload: the explicit quota counters and
/// the buyer's plan reservation are both increased by the upload's
/// net delta. Must be called while holding `state.quota_guard`,
/// immediately after a passing `check_quotas`, so the check and the
/// reservation are atomic. `release_quota` undoes it on failure;
/// a successful upload keeps the reservation until the object row
/// is recorded, then releases the plan part (the recorded object
/// counts towards the plan usage from then on).
pub(crate) fn reserve_quota(
    state: &StorageManagerState,
    module_id: &str,
    user_id: &str,
    company_id: Option<&str>,
    delta_bytes: i64,
    delta_files: i64,
) {
    apply_quota_delta(state, module_id, user_id, company_id, delta_bytes, delta_files);
    let buyer = assignment_key(company_id, user_id);
    state
        .plan_reserved
        .entry(buyer)
        .and_modify(|reserved| {
            reserved.0 += delta_bytes;
            reserved.1 += delta_files;
        })
        .or_insert((delta_bytes, delta_files));
}

/// Undo a reservation made by `reserve_quota`. Call it when the
/// provider upload fails. `release_plan_reservation` releases just
/// the plan part (used after a successful `record_stored` with
/// `apply_quota` false: the explicit counters stay reserved because
/// the object row now accounts for them, and the plan usage is
/// counted from the recorded object instead).
pub(crate) fn release_quota(
    state: &StorageManagerState,
    module_id: &str,
    user_id: &str,
    company_id: Option<&str>,
    delta_bytes: i64,
    delta_files: i64,
) {
    apply_quota_delta(
        state,
        module_id,
        user_id,
        company_id,
        -delta_bytes,
        -delta_files,
    );
    release_plan_reservation(state, company_id, user_id, delta_bytes, delta_files);
}

pub(crate) fn release_plan_reservation(
    state: &StorageManagerState,
    company_id: Option<&str>,
    user_id: &str,
    delta_bytes: i64,
    delta_files: i64,
) {
    let buyer = assignment_key(company_id, user_id);
    if let Some(mut reserved) = state.plan_reserved.get_mut(&buyer) {
        reserved.0 = (reserved.0 - delta_bytes).max(0);
        reserved.1 = (reserved.1 - delta_files).max(0);
    }
}

/// Record one stored object: the object row, the quota counters, and
/// the daily rollup. An overwrite of an existing key counts only the
/// size delta (and zero new files) everywhere, and keeps the
/// original `created_at`. When `apply_quota` is false the explicit
/// quota counters are left untouched — the caller reserved them
/// already (see `reserve_quota`) — but the object row and the daily
/// rollup are still recorded.
pub(crate) fn record_stored(
    state: &StorageManagerState,
    bucket_id: Uuid,
    key: &str,
    size_bytes: i64,
    sha256_hex: Option<String>,
    mime: &str,
    owner_module: &str,
    owner_user: &str,
    company_id: Option<&str>,
    apply_quota: bool,
) {
    let now = Utc::now();
    let map_key = object_key(bucket_id, key);
    let previous = state
        .objects
        .get(&map_key)
        .map(|entry| (entry.value().size_bytes, entry.value().created_at));
    let (delta_bytes, delta_files) = match previous {
        Some((old_size, _)) => (size_bytes - old_size, 0),
        None => (size_bytes, 1),
    };
    let created_at = previous.map(|(_, created)| created).unwrap_or(now);
    state.objects.insert(
        map_key,
        StoredObject {
            bucket_id,
            key: key.to_string(),
            size_bytes,
            sha256_hex,
            mime: mime.to_string(),
            owner_module: owner_module.to_string(),
            owner_user: owner_user.to_string(),
            company_id: company_id.map(str::to_string),
            archived: false,
            created_at,
            updated_at: now,
        },
    );
    if apply_quota {
        apply_quota_delta(
            state,
            owner_module,
            owner_user,
            company_id,
            delta_bytes,
            delta_files,
        );
    }
    let date = now.format("%Y-%m-%d").to_string();
    let daily_key = format!(
        "{date}|{bucket_id}|{}|{owner_user}|{owner_module}",
        company_id.unwrap_or("")
    );
    match state.usage_daily.get_mut(&daily_key) {
        Some(mut row) => {
            row.bytes_total = (row.bytes_total + delta_bytes).max(0);
            row.objects_total = (row.objects_total + delta_files).max(0);
            row.bytes_delta += delta_bytes;
            row.objects_delta += delta_files;
            row.updated_at = now;
        }
        None => {
            state.usage_daily.insert(
                daily_key,
                UsageDaily {
                    date,
                    bucket_id,
                    company_id: company_id.map(str::to_string),
                    user_id: owner_user.to_string(),
                    module_id: owner_module.to_string(),
                    // First record of the day for this scope: totals
                    // are seeded from the NET delta of this record,
                    // not the absolute size — an overwrite (delta
                    // files 0, possibly negative delta bytes) must
                    // not inflate the day's totals with the full
                    // object size.
                    bytes_total: delta_bytes.max(0),
                    objects_total: delta_files.max(0),
                    bytes_delta: delta_bytes,
                    objects_delta: delta_files,
                    created_at: now,
                    updated_at: now,
                },
            );
        }
    }
}

/// Remove one recorded object and decrement the quota counters and
/// the daily rollup. Called after a provider delete succeeds.
pub(crate) fn record_deleted(state: &StorageManagerState, bucket_id: Uuid, key: &str) {
    let map_key = object_key(bucket_id, key);
    let Some((_, object)) = state.objects.remove(&map_key) else {
        return;
    };
    let now = Utc::now();
    for (scope, scope_id) in [
        ("module", object.owner_module.clone()),
        ("user", object.owner_user.clone()),
    ] {
        if let Some(mut quota) = state.quotas.get_mut(&quota_key(scope, &scope_id)) {
            quota.used_bytes = (quota.used_bytes - object.size_bytes).max(0);
            quota.used_objects = (quota.used_objects - 1).max(0);
            quota.updated_at = now;
        }
    }
    if let Some(company) = &object.company_id {
        if let Some(mut quota) = state.quotas.get_mut(&quota_key("company", company)) {
            quota.used_bytes = (quota.used_bytes - object.size_bytes).max(0);
            quota.used_objects = (quota.used_objects - 1).max(0);
            quota.updated_at = now;
        }
    }
    let date = now.format("%Y-%m-%d").to_string();
    let daily_key = format!(
        "{date}|{bucket_id}|{}|{}|{}",
        object.company_id.clone().unwrap_or_default(),
        object.owner_user,
        object.owner_module
    );
    if let Some(mut row) = state.usage_daily.get_mut(&daily_key) {
        row.bytes_total = (row.bytes_total - object.size_bytes).max(0);
        row.objects_total = (row.objects_total - 1).max(0);
        row.updated_at = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_scope_id_trims_and_enforces_bounds() {
        assert_eq!(valid_scope_id("  acme  ").unwrap(), "acme");
        assert!(valid_scope_id("").is_err());
        assert!(valid_scope_id(&"s".repeat(129)).is_err());
        assert_eq!(valid_scope_id(&"s".repeat(128)).unwrap(), "s".repeat(128));
        assert!(valid_scope_id("has\tcontrol").is_err());
    }

    #[test]
    fn valid_module_id_accepts_lowercase_digits_dash_underscore() {
        assert_eq!(valid_module_id("billing_2-x").unwrap(), "billing_2-x");
        assert!(valid_module_id("").is_err());
        assert!(valid_module_id("Billing").is_err());
        assert!(valid_module_id("has space").is_err());
        assert!(valid_module_id("has.dot").is_err());
        assert!(valid_module_id(&"m".repeat(129)).is_err());
    }

    #[test]
    fn valid_prefix_normalizes_root_and_rejects_bad_segments() {
        assert_eq!(valid_prefix("").unwrap(), "");
        assert_eq!(valid_prefix("/").unwrap(), "");
        assert_eq!(valid_prefix("photos/2024").unwrap(), "photos/2024");
        assert_eq!(valid_prefix("/photos/").unwrap(), "photos");
        assert!(valid_prefix("..").is_err());
        assert!(valid_prefix("a//b").is_err());
        assert!(valid_prefix("a/../b").is_err());
        assert!(valid_prefix("a/./b").is_err());
    }

    #[test]
    fn valid_quota_limit_allows_none_and_non_negative() {
        assert_eq!(valid_quota_limit(None, "max_bytes").unwrap(), None);
        assert_eq!(valid_quota_limit(Some(0), "max_bytes").unwrap(), Some(0));
        assert_eq!(valid_quota_limit(Some(1024), "max_bytes").unwrap(), Some(1024));
        assert!(valid_quota_limit(Some(-1), "max_bytes").is_err());
    }

    #[test]
    fn object_key_and_quota_key_join_with_colon() {
        let id = uuid::Uuid::parse_str("123e4567-e89b-12d3-a456-426614174000").unwrap();
        assert_eq!(
            object_key(id, "photos/a.jpg"),
            "123e4567-e89b-12d3-a456-426614174000:photos/a.jpg"
        );
        assert_eq!(quota_key("company", "acme"), "company:acme");
        assert_eq!(quota_key("user", "u-1"), "user:u-1");
    }

    #[test]
    fn upload_delta_counts_only_the_net_change() {
        // A fresh key reserves the full size and one file.
        assert_eq!(upload_delta(None, 1000), (1000, 1));
        // An overwrite of the same key reserves only the delta.
        assert_eq!(upload_delta(Some(400), 1000), (600, 0));
        assert_eq!(upload_delta(Some(1000), 1000), (0, 0));
        // Shrinking frees bytes but never changes the file count.
        assert_eq!(upload_delta(Some(1000), 400), (-600, 0));
    }

    #[test]
    fn requested_allowed_hosts_must_be_operator_approved() {
        let operator = vec!["minio.example.com".to_string()];
        assert!(validate_requested_allowed_hosts(&operator, &["minio.example.com".to_string()]).is_ok());
        // Case-insensitive match against the operator list is fine.
        assert!(validate_requested_allowed_hosts(&operator, &["MINIO.EXAMPLE.COM".to_string()]).is_ok());
        // An arbitrary host a bucket holder invents is rejected.
        assert!(validate_requested_allowed_hosts(&operator, &["evil.example.com".to_string()]).is_err());
        assert!(validate_requested_allowed_hosts(&operator, &["minio.example.com".to_string(), "evil.example.com".to_string()]).is_err());
        // The operator list itself is read from the app config.
        let mut config = AppConfig::default();
        config.storage.allowed_hosts = vec!["minio.example.com".to_string()];
        let config = RwLock::new(config);
        assert_eq!(operator_allowed_hosts(&config), operator);
    }

    #[test]
    fn endpoint_dns_rejects_private_ip_literals() {
        // Loopback literals are allowed (local development endpoints).
        assert!(validate_endpoint_dns("https://127.0.0.1:9000/x").is_ok());
        assert!(validate_endpoint_dns("https://localhost:9000/x").is_ok());
        // Private / link-local IP literals are rejected outright.
        assert!(validate_endpoint_dns("https://10.1.2.3/x").is_err());
        assert!(validate_endpoint_dns("https://192.168.1.1/x").is_err());
        assert!(validate_endpoint_dns("https://169.254.1.1/x").is_err());
        // A host that does not resolve fails closed.
        assert!(validate_endpoint_dns("https://nonexistent.invalid/x").is_err());
    }

    #[test]
    fn record_stored_seeds_daily_totals_from_the_delta_on_overwrite() {
        let state = StorageManagerState::default();
        let bucket_id = Uuid::now_v7();
        let now = Utc::now();
        // A pre-existing 1000-byte object, recorded on an earlier
        // day (so no daily row exists for today yet).
        state.objects.insert(
            object_key(bucket_id, "a.txt"),
            StoredObject {
                bucket_id,
                key: "a.txt".to_string(),
                size_bytes: 1000,
                sha256_hex: None,
                mime: "text/plain".to_string(),
                owner_module: "files".to_string(),
                owner_user: "u-1".to_string(),
                company_id: None,
                archived: false,
                created_at: now,
                updated_at: now,
            },
        );
        // Overwrite it with 1200 bytes as the first record of the day.
        record_stored(
            &state,
            bucket_id,
            "a.txt",
            1200,
            None,
            "text/plain",
            "files",
            "u-1",
            None,
            true,
        );
        let date = now.format("%Y-%m-%d").to_string();
        let row = state
            .usage_daily
            .get(&format!("{date}|{bucket_id}||u-1|files"))
            .expect("daily row must exist");
        // Totals are seeded from the net delta (200 bytes, 0 new
        // files) — not the absolute 1200 bytes / 1 file.
        assert_eq!(row.bytes_total, 200);
        assert_eq!(row.objects_total, 0);
        assert_eq!(row.bytes_delta, 200);
        assert_eq!(row.objects_delta, 0);
    }

    #[test]
    fn check_quotas_merges_soft_warning_into_plan_failure() {
        let state = StorageManagerState::default();
        let now = Utc::now();
        // Soft user quota already crossed (used 100 > soft 50).
        state.quotas.insert(
            quota_key("user", "u-1"),
            StorageQuota {
                id: Uuid::now_v7(),
                scope: "user".to_string(),
                scope_id: "u-1".to_string(),
                bytes_limit: None,
                objects_limit: None,
                soft_bytes_limit: Some(50),
                soft_objects_limit: None,
                used_bytes: 100,
                used_objects: 1,
                created_at: now,
                updated_at: now,
            },
        );
        // A plan whose byte quota the buyer's live usage exceeds.
        let plan = StoragePlan {
            id: Uuid::now_v7(),
            name: "tiny".to_string(),
            description: String::new(),
            quota_bytes: 10,
            max_files: 0,
            included_provider_kinds: Vec::new(),
            product_id: None,
            billing_cycle: None,
            is_public: false,
            sort_order: 0,
            enabled: true,
            created_at: now,
            updated_at: now,
        };
        state.plans.insert(plan.id, plan.clone());
        let assignment_id = Uuid::now_v7();
        state.plan_assignments.insert(
            assignment_id,
            PlanAssignment {
                id: assignment_id,
                plan_id: plan.id,
                company_id: None,
                user_id: "u-1".to_string(),
                status: "active".to_string(),
                assigned_by: "admin".to_string(),
                assigned_at: now,
                expires_at: None,
                cancelled_at: None,
                cancel_reason: None,
                created_at: now,
                updated_at: now,
            },
        );
        state.objects.insert(
            object_key(Uuid::now_v7(), "a.txt"),
            StoredObject {
                bucket_id: Uuid::now_v7(),
                key: "a.txt".to_string(),
                size_bytes: 100,
                sha256_hex: None,
                mime: "text/plain".to_string(),
                owner_module: "files".to_string(),
                owner_user: "u-1".to_string(),
                company_id: None,
                archived: false,
                created_at: now,
                updated_at: now,
            },
        );
        let check = check_quotas(&state, "files", "u-1", None, None, 0, 0);
        assert!(check.exceeded);
        // The soft warning survives the plan failure instead of
        // being discarded.
        assert!(check.warning);
        assert!(check.detail.contains("Soft quota crossed"));
        assert!(check.detail.contains("tiny"));
    }
}
