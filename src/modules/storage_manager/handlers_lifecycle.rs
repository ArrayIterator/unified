// Lifecycle handlers.

use unified_core::config::AppConfig;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use unified_core::storage::sha256_hex;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Lifecycle.
// ---------------------------------------------------------------------------

pub(crate) async fn list_lifecycle(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_READ) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let params = params.into_inner();
    let mut items: Vec<LifecycleRule> = state
        .lifecycle_rules
        .iter()
        .map(|entry| entry.value().clone())
        .collect();
    // Tenant scoping: a non-admin lists only rules for buckets
    // they may access; an admin lists all. Rules for deleted
    // buckets are hidden from non-admins.
    let is_admin = is_platform_admin(&ctx, &found.role, &req, PERM_MANAGE_BUCKETS);
    items.retain(|rule| match state.buckets.get(&rule.bucket_id) {
        Some(bucket) => bucket_company_visible(&bucket.value().company_id, &_scope, is_admin),
        None => is_admin,
    });
    if let Some(bucket) = params.filters.get("bucket_id") {
        items.retain(|rule| rule.bucket_id.to_string() == *bucket);
    }
    if let Some(enabled) = params.filters.get("enabled") {
        let want = enabled == "true";
        items.retain(|rule| rule.enabled == want);
    }
    items.sort_by(|a, b| a.bucket_id.cmp(&b.bucket_id).then(a.created_at.cmp(&b.created_at)));
    ok(paginate(items, &params))
}

/// Validate a lifecycle retention: negative values are always
/// rejected; 0 is rejected unless the caller explicitly confirmed
/// it, because a 0-day retention deletes every object older than
/// now — i.e. everything. 1 or more days is always fine.
pub(crate) fn validate_retention_days(
    retention_days: i64,
    confirm_zero_retention: bool,
) -> UnifiedResult<()> {
    if retention_days < 0 {
        return Err(UnifiedError::bad_request(
            "retention_days must not be negative",
        ));
    }
    if retention_days == 0 && !confirm_zero_retention {
        return Err(UnifiedError::bad_request(
            "retention_days of 0 deletes every object older than now; pass confirm_zero_retention: true to confirm this is intended",
        ));
    }
    Ok(())
}

pub(crate) async fn create_lifecycle(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateLifecycleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_MANAGE_LIFECYCLE) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = is_platform_admin(&ctx, &found.role, &req, PERM_MANAGE_BUCKETS);
    let result: UnifiedResult<LifecycleRule> = (|| {
        let input = body.into_inner();
        let bucket = state
            .buckets
            .get(&input.bucket_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!(
                    "Storage bucket not found: {}",
                    input.bucket_id
                ))
            })?;
        check_bucket_access(&bucket, _scope.as_deref(), is_admin)?;
        validate_retention_days(input.retention_days, input.confirm_zero_retention)?;
        let auto_delete = input.auto_delete.unwrap_or(true);
        let archive_transition = input.archive_transition.unwrap_or(false);
        if !auto_delete && !archive_transition {
            return Err(UnifiedError::bad_request(
                "A lifecycle rule must do something: set auto_delete or archive_transition",
            ));
        }
        let archive_prefix = match &input.archive_prefix {
            Some(raw) => valid_prefix(raw)?,
            None => "archive".to_string(),
        };
        let now = Utc::now();
        let rule = LifecycleRule {
            id: Uuid::now_v7(),
            bucket_id: bucket.id,
            retention_days: input.retention_days,
            zero_retention_confirmed: input.retention_days == 0 && input.confirm_zero_retention,
            auto_delete,
            archive_transition,
            archive_prefix,
            enabled: true,
            last_run_at: None,
            last_run_summary: None,
            last_error: None,
            created_at: now,
            updated_at: now,
        };
        state.lifecycle_rules.insert(rule.id, rule.clone());
        Ok(rule)
    })();
    match result {
        Ok(view) => created(view),
        Err(error) => err_response(error, debug),
    }
}

pub(crate) async fn get_lifecycle(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_READ) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = is_platform_admin(&ctx, &found.role, &req, PERM_MANAGE_BUCKETS);
    let result: UnifiedResult<LifecycleRule> = (|| {
        let rule = load_rule(&state, &id)?;
        let bucket = load_bucket(&state, &rule.bucket_id.to_string())?;
        check_bucket_access(&bucket, _scope.as_deref(), is_admin)?;
        Ok(rule)
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_lifecycle(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    body: web::Json<UpdateLifecycleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_MANAGE_LIFECYCLE) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = is_platform_admin(&ctx, &found.role, &req, PERM_MANAGE_BUCKETS);
    let result: UnifiedResult<LifecycleRule> = (|| {
        let input = body.into_inner();
        let mut rule = load_rule(&state, &id)?;
        let bucket = load_bucket(&state, &rule.bucket_id.to_string())?;
        check_bucket_access(&bucket, _scope.as_deref(), is_admin)?;
        if let Some(days) = input.retention_days {
            validate_retention_days(days, input.confirm_zero_retention)?;
            rule.retention_days = days;
            // The confirmation only covers a 0-day retention; it
            // lapses as soon as the retention becomes >= 1 day.
            rule.zero_retention_confirmed = days == 0 && input.confirm_zero_retention;
        }
        if let Some(flag) = input.auto_delete {
            rule.auto_delete = flag;
        }
        if let Some(flag) = input.archive_transition {
            rule.archive_transition = flag;
        }
        if let Some(raw) = &input.archive_prefix {
            rule.archive_prefix = valid_prefix(raw)?;
        }
        if let Some(enabled) = input.enabled {
            rule.enabled = enabled;
        }
        if !rule.auto_delete && !rule.archive_transition {
            return Err(UnifiedError::bad_request(
                "A lifecycle rule must do something: set auto_delete or archive_transition",
            ));
        }
        rule.updated_at = Utc::now();
        state.lifecycle_rules.insert(rule.id, rule.clone());
        Ok(rule)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_lifecycle(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_MANAGE_LIFECYCLE) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = is_platform_admin(&ctx, &found.role, &req, PERM_MANAGE_BUCKETS);
    let result: UnifiedResult<Value> = (|| {
        let rule = load_rule(&state, &id)?;
        let bucket = load_bucket(&state, &rule.bucket_id.to_string())?;
        check_bucket_access(&bucket, _scope.as_deref(), is_admin)?;
        state.lifecycle_rules.remove(&rule.id);
        Ok(json!({ "lifecycle_rule_id": rule.id, "deleted": true }))
    })();
    json::respond(result, debug)
}

/// Run one lifecycle rule: candidates are recorded objects older
/// than `retention_days`. Auto-delete removes them (provider delete
/// + metadata + quota decrement); archive transition moves them to
/// the archive prefix (download + upload + delete through the
/// provider contract) and marks the record archived. Objects the
/// provider no longer has are cleaned from the metadata instead of
/// failing the whole run. The run never deletes unrecorded objects:
/// the hub only manages what it recorded.
pub(crate) async fn run_one_rule(
    state: &StorageManagerState,
    config: &Arc<RwLock<AppConfig>>,
    rule: &LifecycleRule,
) -> Value {
    let Some(bucket) = state.buckets.get(&rule.bucket_id).map(|entry| entry.value().clone()) else {
        return json!({
            "rule_id": rule.id.to_string(),
            "bucket_id": rule.bucket_id.to_string(),
            "ran": false,
            "error": "Bucket no longer exists",
        });
    };
    // Defense in depth for rules created before the 0-day
    // confirmation existed: an unconfirmed 0-day rule never runs
    // (it would delete everything); the operator must recreate or
    // update it with confirm_zero_retention: true.
    if rule.retention_days < 1 && !rule.zero_retention_confirmed {
        return json!({
            "rule_id": rule.id.to_string(),
            "bucket_id": rule.bucket_id.to_string(),
            "ran": false,
            "error": "Lifecycle rule has an unconfirmed retention_days of 0; set retention_days >= 1 or confirm the 0-day retention",
        });
    }
    let cutoff = Utc::now() - chrono::Duration::days(rule.retention_days);
    let candidates: Vec<StoredObject> = state
        .objects
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|object| object.bucket_id == bucket.id && object.created_at <= cutoff)
        .collect();
    let provider = match provider_for_bucket_with(config, &bucket) {
        Ok(provider) => provider,
        Err(error) => {
            return json!({
                "rule_id": rule.id.to_string(),
                "bucket_id": bucket.id.to_string(),
                "ran": false,
                "error": error.message,
            });
        }
    };
    let mut deleted = 0usize;
    let mut archived = 0usize;
    let mut cleaned = 0usize;
    let mut errors: Vec<String> = Vec::new();
    for object in candidates {
        if rule.archive_transition {
            let archive_key = if rule.archive_prefix.is_empty() {
                object.key.clone()
            } else {
                format!("{}/{}", rule.archive_prefix, object.key)
            };
            match provider.download(&object.key).await {
                Ok(bytes) => {
                    if bytes.len() > MAX_UPLOAD_BYTES {
                        errors.push(format!(
                            "Object \"{}\" exceeds the {} byte move cap; left in place",
                            object.key, MAX_UPLOAD_BYTES
                        ));
                        continue;
                    }
                    match provider.upload(&archive_key, bytes.clone()).await {
                        Ok(_) => match provider.delete(&object.key).await {
                            Ok(()) => {
                                record_deleted(state, bucket.id, &object.key);
                                record_stored(
                                    state,
                                    bucket.id,
                                    &archive_key,
                                    bytes.len() as i64,
                                    Some(sha256_hex(&bytes)),
                                    &object.mime,
                                    &object.owner_module,
                                    &object.owner_user,
                                    object.company_id.as_deref(),
                                    true,
                                );
                                if let Some(mut moved) = state
                                    .objects
                                    .get_mut(&object_key(bucket.id, &archive_key))
                                {
                                    moved.archived = true;
                                    moved.created_at = object.created_at;
                                    moved.updated_at = Utc::now();
                                }
                                state.emit(
                                    events::OBJECT_ARCHIVED,
                                    json!({
                                        "bucket_id": bucket.id.to_string(),
                                        "key": object.key,
                                        "archive_key": archive_key,
                                        "rule_id": rule.id.to_string(),
                                    }),
                                );
                                archived += 1;
                            }
                            Err(error) => errors.push(format!(
                                "Object \"{}\": archive upload succeeded but source delete failed: {}",
                                object.key, error.message
                            )),
                        },
                        Err(error) => errors.push(format!(
                            "Object \"{}\": archive upload failed: {}",
                            object.key, error.message
                        )),
                    }
                }
                Err(error) if error.code == 404 => {
                    // The provider no longer has it: drop the stale
                    // metadata row instead of failing the run.
                    record_deleted(state, bucket.id, &object.key);
                    cleaned += 1;
                }
                Err(error) => errors.push(format!(
                    "Object \"{}\": download failed: {}",
                    object.key, error.message
                )),
            }
        } else if rule.auto_delete {
            match provider.delete(&object.key).await {
                Ok(()) => {
                    record_deleted(state, bucket.id, &object.key);
                    state.emit(
                        events::OBJECT_DELETED,
                        json!({
                            "bucket_id": bucket.id.to_string(),
                            "key": object.key,
                            "rule_id": rule.id.to_string(),
                            "reason": "lifecycle retention expired",
                        }),
                    );
                    deleted += 1;
                }
                Err(error) => errors.push(format!(
                    "Object \"{}\": delete failed: {}",
                    object.key, error.message
                )),
            }
        }
    }
    let summary = format!(
        "Lifecycle rule {} on bucket \"{}\": {} deleted, {} archived, {} stale rows cleaned, {} errors",
        rule.id,
        bucket.name,
        deleted,
        archived,
        cleaned,
        errors.len()
    );
    let now = Utc::now();
    if let Some(mut stored) = state.lifecycle_rules.get_mut(&rule.id) {
        stored.last_run_at = Some(now);
        stored.last_run_summary = Some(summary.clone());
        stored.last_error = if errors.is_empty() {
            None
        } else {
            Some(errors.join("; "))
        };
        stored.updated_at = now;
    }
    json!({
        "rule_id": rule.id.to_string(),
        "bucket_id": bucket.id.to_string(),
        "ran": true,
        "deleted": deleted,
        "archived": archived,
        "cleaned": cleaned,
        "errors": errors,
        "summary": summary,
    })
}

pub(crate) async fn run_lifecycle(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<RunLifecycleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_MANAGE_LIFECYCLE) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let input = body.into_inner();
    // Tenant scoping for the enqueue: the scheduler payload carries
    // only the requested rule / bucket ids, so the selection is
    // checked BEFORE the scheduler is asked — a non-admin can never
    // enqueue a run for a rule on a bucket they may not access, and
    // an unscoped all-rules run is refused for non-admins (the
    // scheduled run could not be scoped later).
    let is_admin = is_platform_admin(&ctx, &found.role, &req, PERM_MANAGE_BUCKETS);
    if input.enqueue.unwrap_or(false) {
        match (input.rule_id, input.bucket_id) {
            (Some(rule_id), _) => {
                let rule = match state.lifecycle_rules.get(&rule_id) {
                    Some(entry) => entry.value().clone(),
                    None => {
                        return err_response(
                            UnifiedError::not_found(format!("Lifecycle rule not found: {rule_id}")),
                            debug,
                        );
                    }
                };
                if let Err(error) = (|| {
                    let bucket = load_bucket(&state, &rule.bucket_id.to_string())?;
                    check_bucket_access(&bucket, _scope.as_deref(), is_admin)
                })() {
                    return err_response(error, debug);
                }
            }
            (None, Some(bucket_id)) => {
                if let Err(error) = (|| {
                    let bucket = load_bucket(&state, &bucket_id.to_string())?;
                    check_bucket_access(&bucket, _scope.as_deref(), is_admin)
                })() {
                    return err_response(error, debug);
                }
            }
            (None, None) if !is_admin => {
                return err_response(
                    UnifiedError::forbidden(
                        "Only a storage admin may enqueue a lifecycle run for all rules",
                    ),
                    debug,
                );
            }
            (None, None) => {}
        }
    }
    // Ask the scheduler first when requested. Until the
    // `storage.lifecycle.run` kind is registered, the scheduler
    // declines and the run executes in-process below — the decline
    // is reported, never hidden.
    let mut scheduler_answer: Value = Value::Null;
    if input.enqueue.unwrap_or(false) {
        let answer = ctx.hooks.emit(
            events::SCHEDULER_ENQUEUE,
            json!({
                "kind": "storage.lifecycle.run",
                "payload": {
                    "rule_id": input.rule_id.map(|id| id.to_string()),
                    "bucket_id": input.bucket_id.map(|id| id.to_string()),
                    "requested_by": found.actor,
                },
            }),
        );
        scheduler_answer = answer
            .results
            .first()
            .cloned()
            .unwrap_or_else(|| json!({ "queued": false, "error": "No scheduler handler answered scheduler.enqueue" }));
    }
    let rules: Vec<LifecycleRule> = match (input.rule_id, input.bucket_id) {
        (Some(rule_id), _) => match state.lifecycle_rules.get(&rule_id) {
            Some(entry) => vec![entry.value().clone()],
            None => {
                return err_response(
                    UnifiedError::not_found(format!("Lifecycle rule not found: {rule_id}")),
                    debug,
                );
            }
        },
        (None, Some(bucket_id)) => state
            .lifecycle_rules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|rule| rule.bucket_id == bucket_id && rule.enabled)
            .collect(),
        (None, None) => state
            .lifecycle_rules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|rule| rule.enabled)
            .collect(),
    };
    // Tenant scoping: a non-admin runs only rules for buckets they
    // may access; an admin runs all selected rules. A single-rule
    // selection that filters to nothing answers not-found, so the
    // rule's existence never leaks across tenants.
    let rules: Vec<LifecycleRule> = rules
        .into_iter()
        .filter(|rule| match state.buckets.get(&rule.bucket_id) {
            Some(bucket) => bucket_company_visible(&bucket.value().company_id, &_scope, is_admin),
            None => is_admin,
        })
        .collect();
    if let Some(rule_id) = input.rule_id {
        if rules.is_empty() {
            return err_response(
                UnifiedError::not_found(format!("Lifecycle rule not found: {rule_id}")),
                debug,
            );
        }
    }
    let mut results: Vec<Value> = Vec::new();
    for rule in &rules {
        results.push(run_one_rule(&state, &ctx.config, rule).await);
    }
    state.emit(
        events::LIFECYCLE_RUN_COMPLETED,
        json!({
            "rules_run": results.len(),
            "requested_by": found.actor,
            "scheduler_answer": scheduler_answer,
        }),
    );
    ok(json!({
        "rules_run": results.len(),
        "results": results,
        "scheduler_answer": scheduler_answer,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retention_zero_needs_explicit_confirmation() {
        assert!(validate_retention_days(1, false).is_ok());
        assert!(validate_retention_days(30, false).is_ok());
        assert!(validate_retention_days(-1, true).is_err());
        // 0 without confirmation is refused: it would delete everything.
        assert!(validate_retention_days(0, false).is_err());
        // 0 with an explicit confirmation is allowed.
        assert!(validate_retention_days(0, true).is_ok());
    }
}
