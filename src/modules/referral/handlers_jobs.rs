//! HTTP handlers for job referral eligibility: which
//! recruitment jobs accept referrals, and which reward rule
//! (if any) overrides the tenant default for one job.

use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::helpers::{
    caller, caller_company, created, eligibility_for_job, err, load_rule, ok, paginate, parse_uuid,
    record_audit, require_manage, require_tenant, require_view, resolve_tenant,
};
use super::state::ReferralState;
use super::types::{EVENT_ELIGIBILITY_SET, JobEligibility, SetEligibilityRequest};

/// List job eligibility rows (paginated, filter by job,
/// eligible; search is not applicable - rows are keyed by
/// job id). Jobs with no row are eligible by default and do
/// not appear here until an administrator sets them.
pub(crate) async fn list_eligibility(
    state: web::Data<ReferralState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let params = query.into_inner();
        let job_filter = params.filters.get("job_id").cloned();
        let eligible_filter = params
            .filters
            .get("eligible")
            .and_then(|raw| match raw.as_str() {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            });
        let mut items: Vec<JobEligibility> = state
            .eligibility
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| row.tenant_id == tenant_id)
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        items.retain(|row| {
            if let Some(job_id) = &job_filter {
                if row.job_id.to_string() != *job_id {
                    return false;
                }
            }
            if let Some(eligible) = eligible_filter {
                if row.eligible != eligible {
                    return false;
                }
            }
            true
        });
        let views: Vec<Value> = items
            .iter()
            .map(|item| serde_json::to_value(item).unwrap_or(Value::Null))
            .collect();
        Ok(paginate(views, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Get the eligibility row of one job. A job with no row is
/// eligible by default - this endpoint answers 404 for it,
/// like every other get-one handler in this tree.
pub(crate) async fn get_eligibility(
    state: web::Data<ReferralState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let job_id = parse_uuid(&path.into_inner(), "job")?;
        let row = eligibility_for_job(&state, &tenant_id, job_id).ok_or_else(|| {
            UnifiedError::not_found(format!("No eligibility row for job: {job_id}"))
        })?;
        Ok(serde_json::to_value(&row).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Set the eligibility of one job (manager): mark a job
/// referral-eligible or not, and optionally point it at a
/// per-job reward rule that overrides the tenant default.
/// Creates the row on first set, updates it afterwards.
pub(crate) async fn set_eligibility(
    state: web::Data<ReferralState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    body: web::Json<SetEligibilityRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<(Value, bool)> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let job_id = parse_uuid(&path.into_inner(), "job")?;
        let input = body.into_inner();
        let reward_rule_id = match &input.reward_rule_id {
            Some(raw) => Some(load_rule(&state, &tenant_id, raw)?.id),
            None => None,
        };
        let now = Utc::now();
        let (row, is_new) = match eligibility_for_job(&state, &tenant_id, job_id) {
            Some(mut existing) => {
                existing.eligible = input.eligible;
                existing.reward_rule_id = reward_rule_id;
                existing.updated_at = now;
                (existing, false)
            }
            None => (
                JobEligibility {
                    id: Uuid::new_v4(),
                    tenant_id: tenant_id.clone(),
                    job_id,
                    eligible: input.eligible,
                    reward_rule_id,
                    created_by: caller.actor.clone(),
                    created_at: now,
                    updated_at: now,
                },
                true,
            ),
        };
        let before = if is_new {
            None
        } else {
            eligibility_for_job(&state, &tenant_id, job_id)
                .and_then(|row| serde_json::to_value(row).ok())
        };
        state.eligibility.insert(row.id, row.clone());
        record_audit(
            &state,
            &tenant_id,
            "job_eligibility",
            &row.id.to_string(),
            if is_new { "created" } else { "updated" },
            &caller.actor,
            before,
            serde_json::to_value(&row).ok(),
        );
        state.emit(
            EVENT_ELIGIBILITY_SET,
            json!({
                "tenant_id": tenant_id,
                "job_id": job_id.to_string(),
                "eligible": row.eligible,
                "reward_rule_id": row.reward_rule_id.map(|id| id.to_string()),
                "actor": caller.actor,
            }),
        );
        if is_new {
            Ok((serde_json::to_value(&row).unwrap_or(Value::Null), true))
        } else {
            Ok((serde_json::to_value(&row).unwrap_or(Value::Null), false))
        }
    })();
    match result {
        Ok((data, is_new)) => {
            if is_new {
                created(data)
            } else {
                ok(data)
            }
        }
        Err(error) => err(error, debug),
    }
}
