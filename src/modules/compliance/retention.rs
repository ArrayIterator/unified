//! Compliance Center module - GDPR / ISO support: data export - Retention-policy handlers.
//!
//! Split from `src/modules/compliance.rs`; behaviour unchanged.

use super::breaches::*;
use super::checklists::*;
use super::consents::*;
use super::constants::*;
use super::deletion_listeners::*;
use super::deletions::*;
use super::exports::*;
use super::helpers::*;
use super::module::*;
use super::records::*;
use super::requests::*;
use super::state::*;
use super::types::*;

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

pub(super) async fn list_policies(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_VIEW)?;
        let params = query.into_inner();
        let category_filter = params.filters.get("data_category").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<RetentionPolicy> = state
            .retention_policies
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|policy| {
                if let Some(category) = &category_filter {
                    if &policy.data_category != category {
                        return false;
                    }
                }
                if !search.is_empty()
                    && !format!(
                        "{} {} {}",
                        policy.data_category,
                        policy.review_action,
                        policy.description.clone().unwrap_or_default()
                    )
                    .to_lowercase()
                    .contains(&search)
                {
                    return false;
                }
                true
            })
            .collect();
        items.sort_by_key(|policy| policy.data_category.clone());
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn create_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    body: web::Json<CreateRetentionPolicyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<RetentionPolicy> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_MANAGE)?;
        let input = body.into_inner();
        let data_category = require_text(&input.data_category, "data_category", MAX_CATEGORY_LEN)?;
        if state
            .retention_policies
            .iter()
            .any(|entry| entry.value().data_category == data_category)
        {
            return Err(UnifiedError::conflict(format!(
                "A retention policy already exists for data category \"{data_category}\""
            )));
        }
        if input.retain_days < 0 || input.retain_days > 365 * 100 {
            return Err(UnifiedError::bad_request(
                "retain_days must be between 0 and 36500",
            ));
        }
        let review_action = require_text(&input.review_action, "review_action", MAX_PURPOSE_LEN)?;
        let description = match &input.description {
            Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
            None => None,
        };
        let now = Utc::now();
        let policy = RetentionPolicy {
            id: Uuid::now_v7(),
            data_category,
            retain_days: input.retain_days,
            review_action,
            description,
            created_at: now,
            updated_at: now,
        };
        state.retention_policies.insert(policy.id, policy.clone());
        state.emit(
            "compliance.retention.policy.created",
            json!({
                "policy_id": policy.id.to_string(),
                "data_category": policy.data_category,
                "retain_days": policy.retain_days,
            }),
        );
        Ok(policy)
    })();
    match result {
        Ok(policy) => created(policy),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn get_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<RetentionPolicy> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_VIEW)?;
        load_policy(&state, &path.into_inner())
    })();
    match result {
        Ok(policy) => ok(policy),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn update_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateRetentionPolicyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<RetentionPolicy> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_MANAGE)?;
        let mut policy = load_policy(&state, &path.into_inner())?;
        let input = body.into_inner();
        if let Some(data_category) = &input.data_category {
            let checked = require_text(data_category, "data_category", MAX_CATEGORY_LEN)?;
            if state.retention_policies.iter().any(|entry| {
                entry.value().data_category == checked && entry.value().id != policy.id
            }) {
                return Err(UnifiedError::conflict(format!(
                    "A retention policy already exists for data category \"{checked}\""
                )));
            }
            policy.data_category = checked;
        }
        if let Some(retain_days) = input.retain_days {
            if retain_days < 0 || retain_days > 365 * 100 {
                return Err(UnifiedError::bad_request(
                    "retain_days must be between 0 and 36500",
                ));
            }
            policy.retain_days = retain_days;
        }
        if let Some(review_action) = &input.review_action {
            policy.review_action = require_text(review_action, "review_action", MAX_PURPOSE_LEN)?;
        }
        if let Some(description) = &input.description {
            policy.description = match description {
                Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        policy.updated_at = Utc::now();
        state.retention_policies.insert(policy.id, policy.clone());
        Ok(policy)
    })();
    match result {
        Ok(policy) => ok(policy),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn delete_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_MANAGE)?;
        let policy = load_policy(&state, &path.into_inner())?;
        let id = policy.id;
        state.retention_policies.remove(&id);
        Ok(json!({ "deleted": true, "policy_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Queue a retention review through `scheduler.enqueue` with kind
/// `compliance.retention.review`. The job only flags items for
/// review - it never deletes anything. See the scheduler note in
/// the module header: the kind is not in `KNOWN_KINDS` yet, so the
/// run records `scheduler_status: "declined"` today.
///
/// The creation is announced through
/// `compliance.retention.review.created` only.
/// `compliance.retention.review_requested` is reserved for the
/// scheduler's dispatch of a queued job: emitting it here would
/// let the request complete its own review (via this module's
/// listener) before the scheduler ever ran it, and the stored row
/// would disagree with the response. The review is created with
/// `reviewed_at: None`; the timestamp is set only when the
/// scheduler dispatch actually runs the review.
pub(super) async fn request_policy_review(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<RetentionReview> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_MANAGE)?;
        let policy = load_policy(&state, &path.into_inner())?;
        let now = Utc::now();
        let mut review = RetentionReview {
            id: Uuid::now_v7(),
            policy_id: policy.id,
            items_flagged: 0,
            notes: None,
            scheduler_status: "declined".to_string(),
            scheduler_job_id: None,
            reviewed_at: None,
            created_at: now,
            updated_at: now,
        };
        if let Some(bus) = state.bus() {
            let answer = bus.emit(
                "scheduler.enqueue",
                json!({
                    "kind": COMPLIANCE_RETENTION_JOB_KIND,
                    "payload": {
                        "policy_id": policy.id.to_string(),
                        "review_id": review.id.to_string(),
                        "data_category": policy.data_category,
                        "retain_days": policy.retain_days,
                    },
                }),
            );
            match answer.results.first() {
                Some(result) if result.get("queued").and_then(Value::as_bool) == Some(true) => {
                    review.scheduler_job_id = result
                        .get("job_id")
                        .and_then(|value| value.as_str())
                        .map(str::to_string);
                    review.scheduler_status = "queued".to_string();
                }
                _ => {
                    review.scheduler_job_id = None;
                    review.scheduler_status = "declined".to_string();
                }
            }
        }
        state.retention_reviews.insert(review.id, review.clone());
        state.emit(
            RETENTION_REVIEW_CREATED,
            json!({
                "review_id": review.id.to_string(),
                "policy_id": policy.id.to_string(),
                "data_category": policy.data_category,
                "scheduler_status": review.scheduler_status,
            }),
        );
        Ok(review)
    })();
    match result {
        Ok(review) => created(review),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn list_retention_reviews(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_VIEW)?;
        let params = query.into_inner();
        let policy_filter = params
            .filters
            .get("policy_id")
            .and_then(|raw| Uuid::parse_str(raw).ok());
        let mut items: Vec<RetentionReview> = state
            .retention_reviews
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|review| {
                if let Some(policy_id) = policy_filter {
                    if review.policy_id != policy_id {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|review| std::cmp::Reverse(review.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
