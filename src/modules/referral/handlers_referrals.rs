//! HTTP handlers for referrals: who referred whom for
//! which job, the audited pipeline moves, and the history.

use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::helpers::{
    caller, caller_company, created, eligibility_for_job, err, evaluate_anti_gaming, load_referral,
    load_stage, load_stage_by_slug, ok, on_referral_hired, paginate, parse_uuid, record_audit,
    record_stage_move, require_manage, require_tenant, require_text, require_view, resolve_tenant,
    validate_candidate_email, validate_phone, verify_referrer, void_open_rewards,
};
use super::state::ReferralState;
use super::types::{
    CreateReferralRequest, EVENT_SUBMITTED, MAX_NAME_LEN, MAX_NOTE_LEN, MAX_PHONE_LEN,
    MAX_REASON_LEN, Referral, ReferralStageHistory, TransitionReferralRequest,
    UpdateReferralRequest,
};

/// Render a referral with its stage name resolved, so
/// clients never have to join stage ids themselves.
fn referral_view(state: &ReferralState, referral: &Referral) -> Value {
    let mut view = serde_json::to_value(referral).unwrap_or(Value::Null);
    if let Some(stage) = state.stages.get(&referral.stage_id) {
        view["stage_name"] = Value::String(stage.name.clone());
    }
    view
}

/// List referrals (paginated, filter by job, referrer,
/// stage slug or id, source; search by candidate name or
/// email).
pub(crate) async fn list_referrals(
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
        let referrer_filter = params.filters.get("referrer_user_id").cloned();
        let stage_filter = params.filters.get("stage").cloned();
        let source_filter = params.filters.get("source").cloned();
        let search = params.search.clone().map(|needle| needle.to_lowercase());
        let mut items = state.referrals_of(&tenant_id);
        items.retain(|referral| {
            if let Some(job_id) = &job_filter {
                if referral.job_id.to_string() != *job_id {
                    return false;
                }
            }
            if let Some(referrer) = &referrer_filter {
                if referral.referrer_user_id != *referrer {
                    return false;
                }
            }
            if let Some(stage) = &stage_filter {
                if referral.stage_id.to_string() != *stage && referral.stage_slug != *stage {
                    return false;
                }
            }
            if let Some(source) = &source_filter {
                if referral.source.as_deref().unwrap_or("") != source {
                    return false;
                }
            }
            if let Some(needle) = &search {
                if !referral.candidate_name.to_lowercase().contains(needle)
                    && !referral.candidate_email.to_lowercase().contains(needle)
                {
                    return false;
                }
            }
            true
        });
        let views: Vec<Value> = items
            .iter()
            .map(|item| referral_view(&state, item))
            .collect();
        Ok(paginate(views, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Submit a referral: the caller is the referrer. The job
/// must be referral-eligible (a job with no eligibility row
/// is eligible by default; an explicit `eligible: false`
/// row blocks the submission), the candidate must not
/// already be referred for the job, the tenant must have
/// defined at least one pipeline stage, and the enabled
/// anti-gaming rules must not block the submission.
/// Anti-gaming checks that cannot be evaluated from the
/// data this module has are recorded as `unverified` on the
/// row instead of blocking it.
pub(crate) async fn create_referral(
    state: web::Data<ReferralState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateReferralRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let input = body.into_inner();
        let job_id = parse_uuid(&input.job_id, "job")?;
        if let Some(row) = eligibility_for_job(&state, &tenant_id, job_id) {
            if !row.eligible {
                return Err(UnifiedError::conflict(format!(
                    "Job {job_id} is not referral-eligible"
                )));
            }
        }
        let candidate_name = require_text(&input.candidate_name, "candidate_name", MAX_NAME_LEN)?;
        let candidate_email = validate_candidate_email(&input.candidate_email)?;
        let candidate_phone = validate_phone(&input.candidate_phone)?;
        let notes = match &input.notes {
            Some(text) => Some(require_text(text, "notes", MAX_NOTE_LEN)?),
            None => None,
        };
        // Resolve the link when one is named: by id (the
        // referrer's own link) or by raw token (a candidate
        // arriving through a shared link). A link for another
        // job, a revoked link, or someone else's link is
        // rejected; the referral itself is still recorded
        // with the link attached only when it checks out.
        let mut link_id: Option<Uuid> = None;
        let mut source = "manual".to_string();
        if let Some(raw_link_id) = &input.link_id {
            let link = super::helpers::load_link(&state, &tenant_id, raw_link_id)?;
            if link.job_id != job_id {
                return Err(UnifiedError::bad_request(
                    "Referral link is for a different job".to_string(),
                ));
            }
            if !link.active {
                return Err(UnifiedError::bad_request(
                    "Referral link is revoked".to_string(),
                ));
            }
            if link.referrer_user_id != caller.actor {
                return Err(UnifiedError::forbidden(
                    "Referral link belongs to another referrer".to_string(),
                ));
            }
            link_id = Some(link.id);
            source = "link".to_string();
        } else if let Some(raw_token) = &input.link_token {
            let token_hash = super::types::hash_value(raw_token.trim());
            if let Some(link) = state.link_for_token_hash(&token_hash) {
                if link.tenant_id != tenant_id || link.job_id != job_id || !link.active {
                    return Err(UnifiedError::bad_request(
                        "Referral link is not usable for this job".to_string(),
                    ));
                }
                // The link owner is the referrer, not the
                // caller: a candidate (or a manager) may
                // submit through a shared link, and the
                // referrer is taken from the link.
                link_id = Some(link.id);
                source = "link".to_string();
                let referrer_user_id = link.referrer_user_id.clone();
                return create_referral_for(
                    &state,
                    &tenant_id,
                    job_id,
                    &referrer_user_id,
                    &caller.actor,
                    &candidate_name,
                    &candidate_email,
                    candidate_phone,
                    link_id,
                    &source,
                    notes,
                );
            }
            return Err(UnifiedError::bad_request(
                "Referral link token is unknown".to_string(),
            ));
        }
        create_referral_for(
            &state,
            &tenant_id,
            job_id,
            &caller.actor,
            &caller.actor,
            &candidate_name,
            &candidate_email,
            candidate_phone,
            link_id,
            &source,
            notes,
        )
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

/// Shared referral creation after the referrer is known
/// (the caller, or the owner of a link token). Kept separate
/// so the link-token path can attribute the referral to the
/// link owner while the caller stays the actor.
#[allow(clippy::too_many_arguments)]
fn create_referral_for(
    state: &ReferralState,
    tenant_id: &str,
    job_id: Uuid,
    referrer_user_id: &str,
    actor: &str,
    candidate_name: &str,
    candidate_email: &str,
    candidate_phone: Option<String>,
    link_id: Option<Uuid>,
    source: &str,
    notes: Option<String>,
) -> UnifiedResult<Value> {
    if state
        .referral_for_email_job(tenant_id, candidate_email, job_id)
        .is_some()
    {
        return Err(UnifiedError::conflict(format!(
            "Candidate {candidate_email} was already referred for job {job_id}"
        )));
    }
    let stages = state.stages_of(tenant_id);
    let Some(initial) = super::types::initial_stage(&stages) else {
        return Err(UnifiedError::conflict(
            "No pipeline stage is defined for this tenant; an administrator must define the referral pipeline first"
                .to_string(),
        ));
    };
    let initial = initial.clone();
    let now = Utc::now();
    let outcome = evaluate_anti_gaming(state, tenant_id, referrer_user_id, candidate_email, now);
    if outcome.blocked {
        return Err(UnifiedError::conflict(format!(
            "Referral blocked by anti-gaming rules: {}",
            outcome.notes.join("; ")
        )));
    }
    let (verified, verification_note) = verify_referrer(state, referrer_user_id);
    let mut all_notes = outcome.notes;
    if !verified {
        all_notes.push(verification_note.clone());
    }
    let referral = Referral {
        id: Uuid::new_v4(),
        tenant_id: tenant_id.to_string(),
        job_id,
        referrer_user_id: referrer_user_id.to_string(),
        referrer_verified: verified,
        verification_note: all_notes.join("; "),
        candidate_name: candidate_name.to_string(),
        candidate_email: candidate_email.to_string(),
        candidate_phone,
        link_id,
        stage_id: initial.id,
        stage_slug: initial.slug.clone(),
        source: Some(source.to_string()),
        notes,
        created_by: actor.to_string(),
        created_at: now,
        updated_at: now,
    };
    state.referrals.insert(referral.id, referral.clone());
    let history = ReferralStageHistory {
        id: Uuid::new_v4(),
        tenant_id: tenant_id.to_string(),
        referral_id: referral.id,
        job_id,
        from_stage_id: None,
        from_stage_slug: None,
        to_stage_id: initial.id,
        to_stage_slug: initial.slug.clone(),
        moved_by: actor.to_string(),
        reason: Some("submitted".to_string()),
        created_at: now,
    };
    state.stage_history.insert(history.id, history);
    record_audit(
        state,
        tenant_id,
        "referral",
        &referral.id.to_string(),
        "created",
        actor,
        None,
        serde_json::to_value(&referral).ok(),
    );
    state.emit(
        EVENT_SUBMITTED,
        json!({
            "tenant_id": tenant_id,
            "referral_id": referral.id.to_string(),
            "job_id": job_id.to_string(),
            "referrer_user_id": referrer_user_id,
            "candidate_email": candidate_email,
            "stage": initial.slug,
            "link_id": link_id.map(|id| id.to_string()),
            "actor": actor,
        }),
    );
    Ok(serde_json::to_value(&referral).unwrap_or(Value::Null))
}

/// Get one referral with its stage name.
pub(crate) async fn get_referral(
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
        let referral = load_referral(&state, &tenant_id, &path.into_inner())?;
        Ok(referral_view(&state, &referral))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Update a referral (manager): candidate name, phone, and
/// notes only. The candidate email, job, referrer, and
/// stage never change here - the stage moves through the
/// transition endpoint so every move is audited.
pub(crate) async fn update_referral(
    state: web::Data<ReferralState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    body: web::Json<UpdateReferralRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let input = body.into_inner();
        let mut referral = load_referral(&state, &tenant_id, &path.into_inner())?;
        let before = serde_json::to_value(&referral).ok();
        if let Some(name) = &input.candidate_name {
            referral.candidate_name = require_text(name, "candidate_name", MAX_NAME_LEN)?;
        }
        if let Some(phone) = &input.candidate_phone {
            referral.candidate_phone = match phone {
                Some(text) => Some(require_text(text, "candidate_phone", MAX_PHONE_LEN)?),
                None => None,
            };
        }
        if let Some(notes) = &input.notes {
            referral.notes = match notes {
                Some(text) => Some(require_text(text, "notes", MAX_NOTE_LEN)?),
                None => None,
            };
        }
        referral.updated_at = Utc::now();
        state.referrals.insert(referral.id, referral.clone());
        record_audit(
            &state,
            &tenant_id,
            "referral",
            &referral.id.to_string(),
            "updated",
            &caller.actor,
            before,
            serde_json::to_value(&referral).ok(),
        );
        Ok(referral_view(&state, &referral))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Delete a referral (manager). A referral with a paid
/// reward cannot be deleted - the payout record must stay.
/// Stage history and audit rows are kept.
pub(crate) async fn delete_referral(
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
        require_manage(&ctx, &caller.role)?;
        let referral = load_referral(&state, &tenant_id, &path.into_inner())?;
        let paid = state.rewards_of(&tenant_id).into_iter().any(|reward| {
            reward.referral_id == referral.id && reward.status == super::types::RewardStatus::Paid
        });
        if paid {
            return Err(UnifiedError::conflict(
                "Referral has a paid reward and cannot be deleted".to_string(),
            ));
        }
        let before = serde_json::to_value(&referral).ok();
        state.referrals.remove(&referral.id);
        record_audit(
            &state,
            &tenant_id,
            "referral",
            &referral.id.to_string(),
            "deleted",
            &caller.actor,
            before,
            None,
        );
        Ok(json!({ "deleted": true, "referral_id": referral.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Move a referral to another pipeline stage (manager):
/// the transition must be allowed by the current stage's
/// data, and the move is written to the stage history and
/// the audit trail. Entering a hired stage creates the
/// reward (when a rule applies) and emits `referral.hired`;
/// entering a rejected stage voids any open rewards.
pub(crate) async fn transition_referral(
    state: web::Data<ReferralState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    body: web::Json<TransitionReferralRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let input = body.into_inner();
        let referral = load_referral(&state, &tenant_id, &path.into_inner())?;
        let to_stage = match (&input.to_stage_id, &input.to_stage_slug) {
            (Some(raw), _) => load_stage(&state, &tenant_id, raw)?,
            (None, Some(slug)) => load_stage_by_slug(&state, &tenant_id, slug)?,
            (None, None) => {
                return Err(UnifiedError::bad_request(
                    "to_stage_id or to_stage_slug is required".to_string(),
                ));
            }
        };
        let reason = match &input.reason {
            Some(text) => Some(require_text(text, "reason", MAX_REASON_LEN)?),
            None => None,
        };
        let updated = record_stage_move(
            &state,
            &tenant_id,
            &referral,
            &to_stage,
            &caller.actor,
            &reason,
        )?;
        let now = Utc::now();
        if to_stage.is_hired_stage {
            on_referral_hired(&state, &tenant_id, &updated, now, &caller.actor);
        }
        if to_stage.is_rejected_stage {
            void_open_rewards(
                &state,
                &tenant_id,
                &updated,
                "referral rejected",
                &caller.actor,
            );
        }
        Ok(referral_view(&state, &updated))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// List the stage history of one referral, oldest first
/// (paginated).
pub(crate) async fn list_referral_history(
    state: web::Data<ReferralState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let referral = load_referral(&state, &tenant_id, &path.into_inner())?;
        let params = query.into_inner();
        let items = state.history_of(referral.id);
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
