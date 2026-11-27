// Hook handlers.

use unified_core::hooks::HookContext;
use unified_core::storage::validate_key;
use serde_json::{json, Value};
use uuid::Uuid;

use super::*;

// Hook handlers.

/// Answer `storage.object.upload_requested`: resolve the caller's
/// module to a bucket (module binding -> company default -> global
/// default), check the quotas that apply, and hand back the resolved
/// binding plus the hub upload endpoint. No bytes move here; the
/// caller uploads through `POST /buckets/{id}/objects?key=`.
/// The payload must never contain secrets; the result never does.
pub(crate) fn on_upload_requested(state: StorageManagerState, ctx: &mut HookContext) {
    let module_raw = ctx
        .data
        .get("module")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let owner = ctx
        .data
        .get("owner")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let company_id = ctx
        .data
        .get("company_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let key_hint = ctx
        .data
        .get("key_hint")
        .and_then(|value| value.as_str())
        .unwrap_or("upload")
        .to_string();
    let bytes = ctx
        .data
        .get("bytes")
        .and_then(|value| value.as_u64())
        .unwrap_or(0) as i64;
    let module_id = match valid_module_id(&module_raw) {
        Ok(id) => id,
        Err(error) => {
            ctx.add_result(json!({ "resolved": false, "error": error.message }));
            return;
        }
    };
    let Some((bucket, prefix, source)) =
        resolve_binding(&state, &module_id, company_id.as_deref())
    else {
        ctx.add_result(json!({
            "resolved": false,
            "module": module_id,
            "error": "No storage binding or default bucket resolves for this module",
        }));
        return;
    };
    let key = if prefix.is_empty() {
        key_hint.clone()
    } else {
        format!("{prefix}/{key_hint}")
    };
    if validate_key(&key).is_err() {
        ctx.add_result(json!({
            "resolved": false,
            "module": module_id,
            "error": "The resolved storage key is invalid (check the binding prefix and key hint)",
        }));
        return;
    }
    let check = check_quotas(
        &state,
        &module_id,
        &owner,
        company_id.as_deref(),
        Some(bucket.provider_kind.as_str()),
        bytes,
        1,
    );
    if check.exceeded {
        state.emit(
            events::QUOTA_EXCEEDED,
            json!({
                "scope": check.scope,
                "scope_id": check.scope_id,
                "module": module_id,
                "user_id": owner,
                "company_id": company_id,
                "bucket_id": bucket.id.to_string(),
                "key": key,
                "bytes": bytes,
                "detail": check.detail,
            }),
        );
        ctx.add_result(json!({
            "resolved": false,
            "module": module_id,
            "bucket_id": bucket.id.to_string(),
            "quota_exceeded": true,
            "error": check.detail,
        }));
        return;
    }
    if check.warning {
        state.emit(
            events::QUOTA_WARNING,
            json!({
                "scope": check.scope,
                "scope_id": check.scope_id,
                "module": module_id,
                "user_id": owner,
                "company_id": company_id,
                "bucket_id": bucket.id.to_string(),
                "key": key,
                "bytes": bytes,
                "detail": check.detail,
            }),
        );
    }
    ctx.add_result(json!({
        "resolved": true,
        "module": module_id,
        "source": source,
        "bucket_id": bucket.id.to_string(),
        "bucket_name": bucket.name,
        "provider_kind": bucket.provider_kind.as_str(),
        "key": key,
        "prefix": prefix,
        "upload_endpoint": format!("/api/v1/storage-manager/buckets/{}/objects?key={}", bucket.id, key),
        "quota_ok": true,
        "quota_warning": check.warning,
        "quota_warning_detail": if check.warning { Some(check.detail) } else { None },
    }));
}

/// Answer `storage.lifecycle.run_requested`: the event the scheduler
/// emits when a `storage.lifecycle.run` job fires. Hook handlers are
/// synchronous, so the run is spawned on the async runtime (the
/// config handle captured at `init` supplies what provider building
/// needs) and the handler answers immediately with
/// `{"accepted": true, "queued": true}`. A stale rule id is
/// answered, not ignored silently. When `init` never ran (no config
/// handle), the handler answers `accepted: false` instead of
/// pretending the run happened.
pub(crate) fn on_lifecycle_run_requested(state: StorageManagerState, ctx: &mut HookContext) {
    let rule_id = ctx
        .data
        .get("rule_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok());
    let bucket_id = ctx
        .data
        .get("bucket_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok());
    let rules: Vec<LifecycleRule> = match (rule_id, bucket_id) {
        (Some(id), _) => match state.lifecycle_rules.get(&id) {
            Some(entry) => vec![entry.value().clone()],
            None => {
                ctx.add_result(json!({
                    "accepted": false,
                    "error": format!("Lifecycle rule not found: {id}"),
                }));
                return;
            }
        },
        (None, Some(bucket)) => state
            .lifecycle_rules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|rule| rule.bucket_id == bucket && rule.enabled)
            .collect(),
        (None, None) => state
            .lifecycle_rules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|rule| rule.enabled)
            .collect(),
    };
    let Some(config) = state
        .app_config
        .read()
        .expect("app config lock poisoned")
        .clone()
    else {
        ctx.add_result(json!({
            "accepted": false,
            "rules_matched": rules.len(),
            "error": "Storage Manager init has not run, so no config handle is available to build providers",
        }));
        return;
    };
    let run_state = state.clone();
    tokio::spawn(async move {
        let mut results: Vec<Value> = Vec::new();
        for rule in &rules {
            results.push(run_one_rule(&run_state, &config, rule).await);
        }
        run_state.emit(
            events::LIFECYCLE_RUN_COMPLETED,
            json!({
                "rules_run": results.len(),
                "trigger": "storage.lifecycle.run_requested",
                "results": results,
            }),
        );
    });
    ctx.add_result(json!({
        "accepted": true,
        "queued": true,
        "rules_matched": rules.len(),
    }));
}
