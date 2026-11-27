//! Storage - capabilities, target metadata store and usage.
//!
//! Part of the `storage` module split; see `src/modules/storage.rs`.

use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::error::UnifiedResult;
use unified_core::storage::StorageKind;
use actix_web::HttpRequest;
use chrono::DateTime;
use chrono::Utc;
use dashmap::DashMap;
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;
use std::sync::OnceLock;

/// Maximum object upload size through the API (100 MiB). Larger
/// objects must be uploaded through the provider directly or in
/// chunks by the caller.
pub(crate) const MAX_UPLOAD_BYTES: usize = 100 * 1024 * 1024;

/// Default signed-URL lifetime in seconds.
pub(crate) const DEFAULT_SIGNED_URL_SECS: u64 = 3600;

/// Maximum signed-URL lifetime in seconds (7 days, the S3 presign cap).
pub(crate) const MAX_SIGNED_URL_SECS: u64 = 604_800;

// Capabilities (checked through the `role.check` hook; fails closed).

pub(crate) const PERM_READ: &str = "storage.read";

pub(crate) const PERM_WRITE: &str = "storage.write";

pub(crate) const PERM_MANAGE_TARGETS: &str = "storage.manage_targets";

pub(crate) fn authorize(
    ctx: &AppContext,
    req: &HttpRequest,
    permission: &str,
) -> UnifiedResult<()> {
    // The caller must be verified (anonymous callers are refused);
    // the capability check runs on the verified role only.
    let verified = unified_core::caller::require_caller(req)?;
    let role = verified.role();
    let context = ctx.hooks.emit(
        "role.check",
        serde_json::json!({ "role": role, "permission": permission, "allowed": false }),
    );
    let allowed = context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    if allowed {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}
// Tenant edge guard (C3 pilot 4, gate-only).

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller` / `authorize` or any
/// capability check.
///
/// This module's records carry no `company_id` / `tenant_id` scope
/// field, so the guard is a fail-closed gate only: no per-record
/// company check is applied. Real partitioning of these records
/// needs a `tenant_id` on the record (C11 follow-up).
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

/// Company scope of the verified caller: the tenant context's
/// company first, the server-resolved caller company second; a
/// mismatch between the two is a 403. The `X-Company-Id` header is
/// never read. Resolved in every gated handler so a tenant/caller
/// mismatch fails closed even though these records carry no company
/// field to filter on (gate-only; see `require_tenant`).
pub(crate) fn caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<Option<String>> {
    let verified = unified_core::caller::caller_from(req)
        .company_id
        .filter(|value| !value.is_empty());
    match (&tenant.company_id, &verified) {
        (Some(tenant_company), Some(verified_company)) if tenant_company != verified_company => {
            Err(UnifiedError::forbidden(
                "Verified caller company does not match the tenant company scope",
            ))
        }
        (Some(tenant_company), _) => Ok(Some(tenant_company.clone())),
        (None, verified) => Ok(verified.clone()),
    }
}

// Target metadata store (in-memory, like the downloader job store).

#[derive(Debug, Clone, Serialize)]
pub(crate) struct StoredTarget {
    pub(crate) name: String,
    pub(crate) kind: StorageKind,
    /// Target configuration with secret fields masked.
    pub(crate) config: Value,
    /// Optional hard quota for this target, in bytes (`None` means
    /// no target-level limit). Enforced on every upload.
    pub(crate) quota_bytes: Option<i64>,
    /// Optional hard quota for this target, in objects (`None`
    /// means no target-level limit).
    pub(crate) max_objects: Option<i64>,
    pub(crate) created_at: DateTime<Utc>,
}

static TARGETS: OnceLock<DashMap<String, StoredTarget>> = OnceLock::new();

pub(crate) fn targets_store() -> &'static DashMap<String, StoredTarget> {
    TARGETS.get_or_init(DashMap::new)
}

/// Build the stored-target view for a target that exists only in
/// the shared registry (declared in configuration, never added
/// through this module), from the provider kind.
///
/// The registry holds only the provider, not its configuration or
/// secrets, so the view carries just `name` and `kind` in `config`
/// and no quota: config-declared targets carry no target-level
/// quota here. The creation time is unknown for such targets, so
/// it is the epoch rather than an invented timestamp.
pub(crate) fn registry_target_view(name: &str, kind: StorageKind) -> StoredTarget {
    StoredTarget {
        name: name.to_string(),
        kind,
        config: serde_json::json!({ "name": name, "kind": kind.as_str() }),
        quota_bytes: None,
        max_objects: None,
        created_at: DateTime::<Utc>::from_timestamp(0, 0).expect("epoch is a valid timestamp"),
    }
}

/// Recorded usage per target, for the optional target-level quota:
/// total bytes and object count of objects uploaded through this
/// module. Keys are target names.
///
/// Usage only tracks module uploads: objects created outside this
/// module (direct provider use, other modules, or predating
/// tracking) never contribute, and deleting one cannot adjust the
/// byte count either, except on a local target where a recursive
/// listing lets the usage be rebuilt from the surviving tracked
/// size records (see `reconcile_usage_from_listing`). Providers
/// without a recursive listing (S3, R2, Google Drive, SFTP) cannot
/// reconcile, so usage may overstate until the target is deleted,
/// which resets usage for that name.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct TargetUsage {
    pub(crate) bytes: i64,
    pub(crate) objects: i64,
}

static TARGET_USAGE: OnceLock<DashMap<String, TargetUsage>> = OnceLock::new();

pub(crate) fn target_usage_store() -> &'static DashMap<String, TargetUsage> {
    TARGET_USAGE.get_or_init(DashMap::new)
}

/// Check `delta_bytes` / `delta_objects` against the target quotas
/// and, when they fit, add the delta to the recorded usage in the
/// same step, so concurrent uploads cannot both pass the check
/// against the same pre-upload usage and over-commit the quota.
///
/// A negative delta (an overwrite with a smaller object) can only
/// shrink usage and always passes. Usage is clamped at zero: a
/// stale delta larger than the recorded usage must not drive the
/// counters negative.
pub(crate) fn try_reserve_usage(
    name: &str,
    delta_bytes: i64,
    delta_objects: i64,
    quota_bytes: Option<i64>,
    max_objects: Option<i64>,
) -> UnifiedResult<()> {
    use dashmap::Entry;
    match target_usage_store().entry(name.to_string()) {
        Entry::Occupied(mut entry) => {
            let usage = entry.get_mut();
            if let Some(limit) = quota_bytes {
                if usage.bytes + delta_bytes > limit {
                    return Err(quota_exceeded_bytes(name, usage.bytes, delta_bytes, limit));
                }
            }
            if let Some(limit) = max_objects {
                if usage.objects + delta_objects > limit {
                    return Err(quota_exceeded_objects(
                        name,
                        usage.objects,
                        delta_objects,
                        limit,
                    ));
                }
            }
            usage.bytes = (usage.bytes + delta_bytes).max(0);
            usage.objects = (usage.objects + delta_objects).max(0);
            Ok(())
        }
        Entry::Vacant(entry) => {
            if let Some(limit) = quota_bytes {
                if delta_bytes > limit {
                    return Err(quota_exceeded_bytes(name, 0, delta_bytes, limit));
                }
            }
            if let Some(limit) = max_objects {
                if delta_objects > limit {
                    return Err(quota_exceeded_objects(name, 0, delta_objects, limit));
                }
            }
            entry.insert(TargetUsage {
                bytes: delta_bytes.max(0),
                objects: delta_objects.max(0),
            });
            Ok(())
        }
    }
}

fn quota_exceeded_bytes(name: &str, used: i64, delta_bytes: i64, limit: i64) -> UnifiedError {
    UnifiedError::conflict(format!(
        "Storage target \"{name}\" quota exceeded: used {used} + {delta_bytes} exceeds the limit of {limit} bytes"
    ))
    .with_metadata("quota_exceeded", Value::Bool(true))
    .with_metadata("target", Value::String(name.to_string()))
}

fn quota_exceeded_objects(
    name: &str,
    used: i64,
    delta_objects: i64,
    limit: i64,
) -> UnifiedError {
    UnifiedError::conflict(format!(
        "Storage target \"{name}\" object quota exceeded: used {used} + {delta_objects} exceeds the limit of {limit} objects"
    ))
    .with_metadata("quota_exceeded", Value::Bool(true))
    .with_metadata("target", Value::String(name.to_string()))
}

/// Undo a reservation made by `try_reserve_usage` (the provider
/// upload failed, so the reserved delta must not stay counted).
/// Counters are clamped at zero.
pub(crate) fn release_usage(name: &str, delta_bytes: i64, delta_objects: i64) {
    if let Some(mut usage) = target_usage_store().get_mut(name) {
        usage.bytes = (usage.bytes - delta_bytes).max(0);
        usage.objects = (usage.objects - delta_objects).max(0);
    }
}

/// Rebuild a target's usage from the size records of the objects
/// still present in a provider listing, and drop the size records
/// of objects the listing no longer shows.
///
/// Only call this with a listing known to be complete for the
/// target (the local provider walks its directory recursively).
/// A shallow or partial listing would wrongly drop tracked objects
/// that are still stored, so callers must not feed it listings
/// from providers whose `list` is not recursive.
pub(crate) fn reconcile_usage_from_listing(target: &str, listed_keys: &[String]) {
    let prefix = format!("{target}\n");
    let mut bytes: i64 = 0;
    let mut objects: i64 = 0;
    let mut stale: Vec<String> = Vec::new();
    for entry in object_sizes_store().iter() {
        let Some(object_key) = entry.key().strip_prefix(prefix.as_str()) else {
            continue;
        };
        if listed_keys.iter().any(|listed| listed == object_key) {
            bytes += *entry.value();
            objects += 1;
        } else {
            stale.push(entry.key().clone());
        }
    }
    for key in stale {
        object_sizes_store().remove(&key);
    }
    target_usage_store().insert(
        target.to_string(),
        TargetUsage { bytes, objects },
    );
}

/// One async mutex per target, serialising uploads to the same
/// target: the quota check, the usage reservation and the size
/// record then see a stable view of the target, so concurrent
/// uploads of the same key cannot reserve against the same stale
/// previous size. Uploads to different targets run in parallel;
/// the lock is held only for the duration of one upload.
static UPLOAD_LOCKS: OnceLock<DashMap<String, Arc<tokio::sync::Mutex<()>>>> = OnceLock::new();

pub(crate) fn upload_lock_for(target: &str) -> Arc<tokio::sync::Mutex<()>> {
    let locks = UPLOAD_LOCKS.get_or_init(DashMap::new);
    let guard = locks
        .entry(target.to_string())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())));
    (*guard).clone()
}

/// Recorded size per object uploaded through this module, keyed by
/// `{target}\n{key}`. Lets overwrites count only the size delta
/// against the quota instead of double-counting.
static OBJECT_SIZES: OnceLock<DashMap<String, i64>> = OnceLock::new();

pub(crate) fn object_sizes_store() -> &'static DashMap<String, i64> {
    OBJECT_SIZES.get_or_init(DashMap::new)
}

pub(crate) fn object_size_key(target: &str, key: &str) -> String {
    format!("{target}\n{key}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_size_key_joins_with_newline() {
        assert_eq!(object_size_key("photos", "a/b.jpg"), "photos\na/b.jpg");
    }

    #[test]
    fn registry_target_view_carries_provider_kind_and_no_quota() {
        let view = registry_target_view("config-target", StorageKind::S3);
        assert_eq!(view.name, "config-target");
        assert_eq!(view.kind, StorageKind::S3);
        assert_eq!(view.quota_bytes, None);
        assert_eq!(view.max_objects, None);
        // The view carries only name/kind: no config values and no
        // secrets could leak from a provider the registry holds.
        assert_eq!(view.config.get("name").and_then(Value::as_str), Some("config-target"));
        assert_eq!(view.config.get("kind").and_then(Value::as_str), Some("s3"));
        assert!(view.config.get("secret_key").is_none());
        assert!(view.config.get("access_key").is_none());
        // Creation time is unknown, so it is the epoch, not "now".
        assert_eq!(view.created_at.timestamp(), 0);
    }

    #[test]
    fn try_reserve_usage_counts_within_quota() {
        let target = "reserve-within-quota";
        target_usage_store().remove(target);
        try_reserve_usage(target, 400, 1, Some(1000), Some(2)).expect("fits the quota");
        let usage = *target_usage_store().get(target).expect("usage recorded").value();
        assert_eq!(usage.bytes, 400);
        assert_eq!(usage.objects, 1);
        try_reserve_usage(target, 500, 1, Some(1000), Some(2)).expect("still fits");
        let usage = *target_usage_store().get(target).expect("usage recorded").value();
        assert_eq!(usage.bytes, 900);
        assert_eq!(usage.objects, 2);
        target_usage_store().remove(target);
    }

    #[test]
    fn try_reserve_usage_rejects_over_quota_without_counting() {
        let target = "reserve-over-quota";
        target_usage_store().remove(target);
        try_reserve_usage(target, 800, 1, Some(1000), Some(10)).expect("fits the quota");
        let error = try_reserve_usage(target, 300, 1, Some(1000), Some(10))
            .expect_err("800 + 300 exceeds the 1000-byte quota");
        assert_eq!(error.code, 409);
        assert_eq!(
            error.metadata.get("quota_exceeded").and_then(Value::as_bool),
            Some(true)
        );
        // The rejected delta was not counted.
        let usage = *target_usage_store().get(target).expect("usage recorded").value();
        assert_eq!(usage.bytes, 800);
        assert_eq!(usage.objects, 1);
        target_usage_store().remove(target);
    }

    #[test]
    fn try_reserve_usage_rejects_over_object_quota() {
        let target = "reserve-over-objects";
        target_usage_store().remove(target);
        try_reserve_usage(target, 10, 1, None, Some(1)).expect("first object fits");
        assert!(try_reserve_usage(target, 10, 1, None, Some(1)).is_err());
        let usage = *target_usage_store().get(target).expect("usage recorded").value();
        assert_eq!(usage.objects, 1);
        target_usage_store().remove(target);
    }

    #[test]
    fn try_reserve_usage_rejects_first_upload_over_quota() {
        let target = "reserve-first-over-quota";
        target_usage_store().remove(target);
        assert!(try_reserve_usage(target, 2000, 1, Some(1000), None).is_err());
        assert!(target_usage_store().get(target).is_none());
    }

    #[test]
    fn try_reserve_usage_accepts_negative_delta() {
        // An overwrite with a smaller object shrinks usage.
        let target = "reserve-negative-delta";
        target_usage_store().remove(target);
        try_reserve_usage(target, 500, 1, Some(1000), None).expect("fits the quota");
        try_reserve_usage(target, -200, 0, Some(1000), None).expect("shrink always fits");
        let usage = *target_usage_store().get(target).expect("usage recorded").value();
        assert_eq!(usage.bytes, 300);
        assert_eq!(usage.objects, 1);
        target_usage_store().remove(target);
    }

    #[test]
    fn release_usage_rolls_back_a_reservation() {
        let target = "release-rollback";
        target_usage_store().remove(target);
        try_reserve_usage(target, 250, 1, None, None).expect("no quota to exceed");
        release_usage(target, 250, 1);
        let usage = *target_usage_store().get(target).expect("usage recorded").value();
        assert_eq!(usage.bytes, 0);
        assert_eq!(usage.objects, 0);
        // Releasing more than was reserved clamps at zero instead
        // of driving the counters negative.
        release_usage(target, 999, 5);
        let usage = *target_usage_store().get(target).expect("usage recorded").value();
        assert_eq!(usage.bytes, 0);
        assert_eq!(usage.objects, 0);
        target_usage_store().remove(target);
    }

    #[test]
    fn reconcile_usage_from_listing_keeps_survivors_and_drops_stale() {
        let target = "reconcile-listing";
        let kept = object_size_key(target, "kept.txt");
        let gone = object_size_key(target, "gone.txt");
        let other = object_size_key("another-target", "kept.txt");
        object_sizes_store().insert(kept.clone(), 100);
        object_sizes_store().insert(gone.clone(), 50);
        object_sizes_store().insert(other.clone(), 7);
        // Usage is overstated: it still counts the object that no
        // longer exists (the drift a missing size record leaves).
        target_usage_store().insert(
            target.to_string(),
            TargetUsage {
                bytes: 150,
                objects: 2,
            },
        );
        reconcile_usage_from_listing(target, &["kept.txt".to_string()]);
        let usage = *target_usage_store().get(target).expect("usage recorded").value();
        assert_eq!(usage.bytes, 100);
        assert_eq!(usage.objects, 1);
        assert!(object_sizes_store().get(&kept).is_some());
        // The stale size record is pruned with the usage.
        assert!(object_sizes_store().get(&gone).is_none());
        // Another target's records are untouched.
        assert!(object_sizes_store().get(&other).is_some());
        object_sizes_store().remove(&kept);
        object_sizes_store().remove(&other);
        target_usage_store().remove(target);
    }

    #[test]
    fn reconcile_usage_from_listing_empty_listing_zeroes_usage() {
        let target = "reconcile-empty-listing";
        let key = object_size_key(target, "only.txt");
        object_sizes_store().insert(key.clone(), 40);
        target_usage_store().insert(
            target.to_string(),
            TargetUsage {
                bytes: 40,
                objects: 1,
            },
        );
        reconcile_usage_from_listing(target, &[]);
        let usage = *target_usage_store().get(target).expect("usage recorded").value();
        assert_eq!(usage.bytes, 0);
        assert_eq!(usage.objects, 0);
        assert!(object_sizes_store().get(&key).is_none());
        target_usage_store().remove(target);
    }

    #[test]
    fn upload_lock_for_returns_the_same_lock_per_target() {
        let first = upload_lock_for("lock-target-a");
        let second = upload_lock_for("lock-target-a");
        assert!(Arc::ptr_eq(&first, &second));
        let other = upload_lock_for("lock-target-b");
        assert!(!Arc::ptr_eq(&first, &other));
    }
}
