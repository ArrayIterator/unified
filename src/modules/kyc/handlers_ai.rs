//! HTTP handlers for the AI verification pipeline: run a
//! case through the AI module's gated runner under the
//! tenant's active default verification context merged with
//! its active extra instructions, store the verdict, and
//! return the case to the human review queue.
//!
//! The AI never approves a case on its own. The verdict feeds
//! the review queue; the only approval without a human is the
//! admin-enabled auto-approve policy (see `KycSettings`),
//! which applies only to a `verified` verdict whose resolved
//! risk level is the configured low-risk level, and which
//! defaults to off.

use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::UnifiedError;

use super::handlers_cases::load_case_for_manager;
use super::helpers::{append_audit, caller, case_event, case_view, err, ok, resolve_tenant};
use super::state::KycState;
use super::types::{
    can_transition, compute_risk_score, parse_ai_verdict, resolve_risk_level,
    resolve_verification_context, KycAiVerdict, KycCaseStatus, KycVerdictOutcome,
    EVENT_CASE_APPROVED, EVENT_CASE_VERIFIED,
};
use crate::modules::ai;

/// Path carrying a case id.
#[derive(Debug, Deserialize)]
struct CasePath {
    case_id: String,
}

/// Run AI verification for one case (manager).
///
/// 1. The case must be `submitted` or `more_info_requested`.
/// 2. The tenant must have an active default verification
///    context — without one the run is refused (400), because
///    there is no declared behaviour for the AI to follow.
/// 3. The context is resolved with its active extra
///    instructions, and the case's document metadata is sent
///    to the AI module's gated runner
///    (`run_ai_capability`, capability
///    `verify_identity_documents`). The runner emits the
///    grounding hook, answered in `hooks.rs`, so the resolved
///    context and instructions reach the model as grounding.
/// 4. The returned output is parsed into a verdict; an
///    unparseable output becomes an `uncertain` verdict with a
///    parse-failure reason, never a silent success.
/// 5. The risk score is computed from the verdict and
///    resolved against the tenant's risk levels; the case
///    returns to `submitted` for human review, unless the
///    admin-enabled auto-approve policy applies.
///
/// A failed run returns the case to `submitted` so it can be
/// retried; the failure is audited, not hidden.
pub(crate) async fn verify_case(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<CasePath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let tenant_id = match resolve_tenant(&req) {
        Ok(tenant_id) => tenant_id,
        Err(error) => return err(error, debug),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    let mut case = match load_case_for_manager(&state, &ctx, &tenant_id, &caller, &path.case_id) {
        Ok(case) => case,
        Err(error) => return err(error, debug),
    };
    let settings = state.current_settings(&tenant_id);
    if !settings.require_ai_verification {
        return err(
            UnifiedError::bad_request("AI verification is disabled for this tenant"),
            debug,
        );
    }
    if !matches!(
        case.status,
        KycCaseStatus::Submitted | KycCaseStatus::MoreInfoRequested
    ) {
        return err(
            UnifiedError::bad_request(format!(
                "Only a submitted case can be verified, not a {} case",
                case.status.as_str()
            )),
            debug,
        );
    }
    let context = match state.active_context(&tenant_id) {
        Some(context) => context,
        None => {
            return err(
                UnifiedError::bad_request(
                    "No active verification context exists for this tenant: an admin must create and activate one first",
                ),
                debug,
            );
        }
    };
    let instructions = state.applicable_instructions(&tenant_id, context.id);
    let resolved = resolve_verification_context(&context, &instructions);
    let documents: Vec<Value> = state
        .case_documents(case.id)
        .iter()
        .map(|document| {
            json!({
                "document_type_code": document.document_type_code,
                "document_id": document.document_id,
                "content_hash": document.content_hash,
            })
        })
        .collect();
    if documents.is_empty() {
        return err(
            UnifiedError::bad_request("Case has no linked documents to verify"),
            debug,
        );
    }
    // Move to verifying before the provider call so a
    // concurrent second run sees the case is busy.
    if !can_transition(case.status, KycCaseStatus::Verifying) {
        return err(
            UnifiedError::bad_request(format!(
                "Case cannot move from {} to verifying",
                case.status.as_str()
            )),
            debug,
        );
    }
    case.status = KycCaseStatus::Verifying;
    case.updated_at = Utc::now();
    state.cases.insert(case.id, case.clone());

    let fail = |state: &KycState, case: &mut super::types::KycCase, error: UnifiedError| {
        if case.status == KycCaseStatus::Verifying {
            case.status = KycCaseStatus::Submitted;
            case.updated_at = Utc::now();
            state.cases.insert(case.id, case.clone());
        }
        append_audit(
            state,
            &case.tenant_id,
            case.id,
            "verification_failed",
            &caller.actor,
            Some(error.to_string()),
        );
        err(error, debug)
    };
    let ai_caller = match ai::caller(&req) {
        Ok(caller) => caller,
        Err(error) => return fail(&state, &mut case, error),
    };
    if let Err(error) = ai::require_permission(&ctx, &ai_caller.role, ai::PERM_AI_USE) {
        return fail(&state, &mut case, error);
    }
    let company_id = ai::resolve_capability_company(&ctx, &req, &ai_caller);
    let input = json!({
        "case_id": case.id.to_string(),
        "subject_type": case.subject_type,
        "documents": documents,
        "checks": resolved.checks,
        "instructions": resolved.instruction_texts,
    });
    let output = match ai::run_ai_capability(
        &ctx,
        &ai_caller,
        company_id,
        "kyc",
        "verify_identity_documents",
        input,
    )
    .await
    {
        Ok(output) => output,
        Err(error) => return fail(&state, &mut case, error),
    };
    let output_text = output
        .get("output")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let model = output
        .get("model")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let mut verdict = parse_ai_verdict(&output_text).unwrap_or_else(|| KycAiVerdict {
        outcome: KycVerdictOutcome::Uncertain,
        confidence: 0,
        reasons: vec!["AI verification output could not be parsed as a verdict".to_string()],
        failed_checks: Vec::new(),
        model: model.clone(),
        run_at: Utc::now(),
        context_version: resolved.context_version,
        instruction_versions: resolved.instruction_versions.clone(),
    });
    verdict.model = model;
    verdict.context_version = resolved.context_version;
    verdict.instruction_versions = resolved.instruction_versions.clone();
    let risk_score = compute_risk_score(
        verdict.outcome,
        verdict.confidence,
        verdict.failed_checks.len() as i64,
    );
    // `active_risk_levels` builds a fresh list each call, so bind
    // it first: passing the temporary straight into
    // `resolve_risk_level` would drop it while still borrowed.
    let risk_levels = state.active_risk_levels(&tenant_id);
    let level = resolve_risk_level(&risk_levels, risk_score);
    case.ai_verdict = Some(verdict.clone());
    case.risk_score = Some(risk_score);
    case.risk_level_id = level.map(|row| row.id);
    case.risk_level_code = level.map(|row| row.code.clone());
    case.status = KycCaseStatus::Submitted;
    case.updated_at = Utc::now();
    state.cases.insert(case.id, case.clone());
    case_event(
        &state,
        EVENT_CASE_VERIFIED,
        &case,
        &caller.actor,
        "verified",
        Some(format!(
            "AI verdict {} (confidence {}, risk score {}, level {})",
            verdict.outcome.as_str(),
            verdict.confidence,
            risk_score,
            case.risk_level_code.as_deref().unwrap_or("unresolved")
        )),
    );
    // Auto-approve is the only path by which an AI verdict
    // approves a case without a human, and only when an admin
    // explicitly enabled it for the resolved low-risk level.
    // It defaults to off, and a rejected or uncertain verdict
    // never auto-approves.
    if settings.auto_approve_low_risk
        && verdict.outcome == KycVerdictOutcome::Verified
        && case.risk_level_code.as_deref() == Some(settings.auto_approve_risk_level_code.as_str())
    {
        case.status = KycCaseStatus::Approved;
        case.reviewed_by = Some("system".to_string());
        case.decision_reason = Some(
            "Auto-approved by the admin-enabled low-risk policy after a verified AI verdict"
                .to_string(),
        );
        case.updated_at = Utc::now();
        state.cases.insert(case.id, case.clone());
        case_event(
            &state,
            EVENT_CASE_APPROVED,
            &case,
            "system",
            "auto_approved",
            Some(format!(
                "AI verdict verified, risk level {}",
                settings.auto_approve_risk_level_code
            )),
        );
        state.notify(
            "kyc.case.approved",
            &case.subject_id,
            "Identity verification approved",
            "Your identity verification was approved.",
            Some(format!("/kyc/cases/{}", case.id)),
        );
    }
    ok(case_view(&state, &case))
}

/// Run one registered KYC AI capability generically:
/// `POST /api/v1/kyc/ai/{capability}` with body
/// `{"input": {...}}`. Requires `kyc.view` plus the AI
/// module's `ai:use`; delegates to the AI module's gated
/// runner, exactly like the e-learning AI endpoint.
pub(crate) async fn ai_capability(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ai::CapabilityCallInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let capability = path.into_inner();
    let input = body.into_inner().input;
    let _ = state;
    let _tenant_id = match resolve_tenant(&req) {
        Ok(tenant_id) => tenant_id,
        Err(error) => return err(error, debug),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) =
        super::helpers::require_permission(&ctx, &caller.role, super::types::PERM_KYC_VIEW)
    {
        return err(error, debug);
    }
    let ai_caller = match ai::caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = ai::require_permission(&ctx, &ai_caller.role, ai::PERM_AI_USE) {
        return err(error, debug);
    }
    let company_id = ai::resolve_capability_company(&ctx, &req, &ai_caller);
    match ai::run_ai_capability(&ctx, &ai_caller, company_id, "kyc", &capability, input).await {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
