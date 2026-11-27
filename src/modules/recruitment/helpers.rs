//! Recruitment - Shared helpers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::{constants::*, types::*, state::*, module::*, handlers_jobs::*, handlers_applicants::*, handlers_interviews::*, handlers_evaluations::*, handlers_offers::*};

// Helpers

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the ai module.
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
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the ai module's capability check).
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

pub(crate) fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
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

pub(crate) fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

pub(crate) fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.chars().count() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

pub(crate) fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

pub(crate) fn err(error: UnifiedError, debug: bool) -> HttpResponse {
    json::error_response(&error, debug)
}

pub(crate) fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

/// Money: finite, non-negative, rounded to 4 decimal places.
pub(crate) fn valid_price(price: f64) -> UnifiedResult<f64> {
    if !price.is_finite() || price < 0.0 {
        return Err(UnifiedError::bad_request("price must be a finite number >= 0"));
    }
    Ok((price * 10_000.0).round() / 10_000.0)
}

pub(crate) fn round_money(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

/// Currency: 3-letter ISO code, uppercased.
pub(crate) fn valid_currency(currency: &str) -> UnifiedResult<String> {
    let upper = currency.trim().to_uppercase();
    if upper.len() != 3 || !upper.chars().all(|c| c.is_ascii_uppercase()) {
        return Err(UnifiedError::bad_request(
            "currency must be a 3-letter ISO code such as \"USD\"",
        ));
    }
    Ok(upper)
}

pub(crate) fn valid_email(email: &str) -> bool {
    if email.is_empty() || email.len() > MAX_EMAIL_LEN || email.contains(' ') {
        return false;
    }
    let mut parts = email.split('@');
    let (Some(local), Some(domain)) = (parts.next(), parts.next()) else {
        return false;
    };
    if parts.next().is_some() || local.is_empty() || domain.is_empty() {
        return false;
    }
    // No consecutive dots in either part, and no leading/trailing
    // dot in the local part: "a..b@c.com" and "a@b..c" are not
    // deliverable addresses.
    if local.contains("..")
        || domain.contains("..")
        || local.starts_with('.')
        || local.ends_with('.')
    {
        return false;
    }
    // The TLD (after the last dot) must be at least 2 letters:
    // "a@b.c" and "a@b.c0" are not valid addresses.
    let Some(tld) = domain.rsplit('.').next() else {
        return false;
    };
    if tld.len() < 2 || !tld.chars().all(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
}

/// The pipeline state machine. Forward only:
/// `applied` -> `screening` -> `interview` -> `offer` -> `hired`,
/// with `rejected` reachable from any open stage. `hired` and
/// `rejected` are terminal -- a rejected applicant is never moved
/// back, and a hired applicant never re-enters the pipeline.
pub(crate) fn allowed_stage_transition(from: ApplicantStage, to: ApplicantStage) -> bool {
    if from == to {
        return false;
    }
    match from {
        ApplicantStage::Applied => matches!(
            to,
            ApplicantStage::Screening | ApplicantStage::Rejected
        ),
        ApplicantStage::Screening => matches!(
            to,
            ApplicantStage::Interview | ApplicantStage::Rejected
        ),
        ApplicantStage::Interview => {
            matches!(to, ApplicantStage::Offer | ApplicantStage::Rejected)
        }
        ApplicantStage::Offer => {
            matches!(to, ApplicantStage::Hired | ApplicantStage::Rejected)
        }
        ApplicantStage::Hired | ApplicantStage::Rejected => false,
    }
}

pub(crate) fn load_job(state: &RecruitmentState, raw_id: &str) -> UnifiedResult<Job> {
    let id = parse_uuid(raw_id, "job")?;
    state
        .jobs
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Job not found: {id}")))
}

pub(crate) fn load_applicant(state: &RecruitmentState, raw_id: &str) -> UnifiedResult<Applicant> {
    let id = parse_uuid(raw_id, "applicant")?;
    state
        .applicants
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Applicant not found: {id}")))
}

pub(crate) fn load_interview(state: &RecruitmentState, raw_id: &str) -> UnifiedResult<Interview> {
    let id = parse_uuid(raw_id, "interview")?;
    state
        .interviews
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Interview not found: {id}")))
}

pub(crate) fn load_offer(state: &RecruitmentState, raw_id: &str) -> UnifiedResult<Offer> {
    let id = parse_uuid(raw_id, "offer")?;
    state
        .offers
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Offer not found: {id}")))
}

pub(crate) fn validate_salary_range(
    salary_min: Option<f64>,
    salary_max: Option<f64>,
) -> UnifiedResult<(Option<f64>, Option<f64>)> {
    let min = match salary_min {
        Some(value) => Some(valid_price(value)?),
        None => None,
    };
    let max = match salary_max {
        Some(value) => Some(valid_price(value)?),
        None => None,
    };
    if let (Some(min_value), Some(max_value)) = (min, max) {
        if max_value < min_value {
            return Err(UnifiedError::bad_request(
                "salary_max must not be below salary_min",
            ));
        }
    }
    Ok((min, max))
}

pub(crate) fn validate_scores(scores: &[EvaluationScore]) -> UnifiedResult<()> {
    if scores.is_empty() || scores.len() > 20 {
        return Err(UnifiedError::bad_request(
            "An evaluation needs between 1 and 20 criterion scores",
        ));
    }
    let mut seen: Vec<String> = Vec::new();
    for entry in scores {
        let criterion = entry.criterion.trim();
        if criterion.is_empty() || criterion.chars().count() > MAX_CRITERION_LEN {
            return Err(UnifiedError::bad_request(
                "Each score criterion must not be empty and must be at most 120 characters",
            ));
        }
        if !(MIN_SCORE..=MAX_SCORE).contains(&entry.score) {
            return Err(UnifiedError::bad_request(format!(
                "Each score must be between {MIN_SCORE} and {MAX_SCORE}"
            )));
        }
        if entry
            .note
            .as_ref()
            .is_some_and(|note| note.chars().count() > MAX_NOTES_LEN)
        {
            return Err(UnifiedError::bad_request(
                "Each score note must be at most 5000 characters",
            ));
        }
        // Duplicate detection is case-insensitive on the trimmed
        // name: "Skill" and "skill" are the same criterion and must
        // not both be scored in one evaluation (they would otherwise
        // aggregate as separate criteria).
        let normalized = criterion.to_lowercase();
        if seen.iter().any(|item| item == &normalized) {
            return Err(UnifiedError::bad_request(format!(
                "Criterion \"{criterion}\" is scored twice in one evaluation"
            )));
        }
        seen.push(normalized);
    }
    Ok(())
}

/// Aggregate every evaluation of one applicant: the overall
/// average across all scores, the number of scorecards, and the
/// per-criterion averages. Returns `None` for the average when no
/// evaluation exists -- no score is ever invented for an
/// interviewer who did not submit one.
pub(crate) fn aggregate_scores(state: &RecruitmentState, applicant_id: Uuid) -> Value {
    let evaluations: Vec<Evaluation> = state
        .evaluations
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|evaluation| evaluation.applicant_id == applicant_id)
        .collect();
    let mut total: i64 = 0;
    let mut count: i64 = 0;
    let mut by_criterion: Vec<(String, i64, i64)> = Vec::new();
    for evaluation in &evaluations {
        for score in &evaluation.scores {
            total += score.score;
            count += 1;
            match by_criterion
                .iter_mut()
                .find(|(criterion, _, _)| criterion == &score.criterion)
            {
                Some((_, sum, n)) => {
                    *sum += score.score;
                    *n += 1;
                }
                None => by_criterion.push((score.criterion.clone(), score.score, 1)),
            }
        }
    }
    let mut criteria: Vec<Value> = by_criterion
        .iter()
        .map(|(criterion, sum, n)| {
            json!({
                "criterion": criterion,
                "average": (*sum as f64) / (*n as f64),
                "scores": n,
            })
        })
        .collect();
    criteria.sort_by(|a, b| {
        a.get("criterion")
            .and_then(Value::as_str)
            .cmp(&b.get("criterion").and_then(Value::as_str))
    });
    json!({
        "evaluations": evaluations.len(),
        "average_score": if count > 0 { Some((total as f64) / (count as f64)) } else { None },
        "scores_total": count,
        "criteria": criteria,
    })
}

/// Applicant read view: the applicant plus its aggregated
/// evaluation score, so a list never needs a second request to
/// rank candidates.
pub(crate) fn applicant_view(state: &RecruitmentState, applicant: &Applicant) -> Value {
    let mut value = serde_json::to_value(applicant).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        map.insert(
            "evaluation_summary".to_string(),
            aggregate_scores(state, applicant.id),
        );
    }
    value
}

/// Append one stage-history row. The history is append-only; the
/// applicant row itself carries only the live stage.
pub(crate) fn record_stage_move(
    state: &RecruitmentState,
    applicant: &Applicant,
    from_stage: Option<ApplicantStage>,
    to_stage: ApplicantStage,
    moved_by: &str,
    reason: Option<String>,
) {
    let entry = ApplicantStageHistory {
        id: Uuid::now_v7(),
        applicant_id: applicant.id,
        job_id: applicant.job_id,
        from_stage,
        to_stage,
        moved_by: moved_by.to_string(),
        reason,
        created_at: Utc::now(),
    };
    state.stage_history.insert(entry.id, entry);
}

/// Refuse any outbound contact with an applicant who has not
/// consented. Consent is recorded with a timestamp; withdrawing
/// it (setting `consent_contact` to false) blocks interviews and
/// offers again until consent is given anew.
pub(crate) fn require_consent(applicant: &Applicant, action: &str) -> UnifiedResult<()> {
    if applicant.consent_contact {
        Ok(())
    } else {
        Err(UnifiedError::conflict(format!(
            "Applicant has not consented to be contacted, so {action} is refused"
        )))
    }
}

/// Queue one interview reminder through `scheduler.enqueue`.
/// Mirrors the events module's `schedule_reminder`: the scheduler
/// answers `{"queued": true, "job_id": ...}` when it knows the
/// kind, and `{"queued": false, "error": "Unknown job kind: ..."}`
/// today, because `recruitment.interview.reminder` is not in
/// `KNOWN_KINDS` yet. The interview is stored either way; the
/// row records `scheduler_status` honestly (`queued` /
/// `declined`).
pub(crate) fn schedule_interview_reminder(state: &RecruitmentState, interview: &mut Interview) {
    let Some(bus) = state.bus() else {
        interview.scheduler_job_id = None;
        interview.scheduler_status = "declined".to_string();
        return;
    };
    let answer = bus.emit(
        "scheduler.enqueue",
        json!({
            "kind": "recruitment.interview.reminder",
            "payload": {
                "interview_id": interview.id.to_string(),
                "applicant_id": interview.applicant_id.to_string(),
                "job_id": interview.job_id.to_string(),
                "scheduled_at": interview.scheduled_at.to_rfc3339(),
                "interviewers": interview.interviewers,
            },
        }),
    );
    match answer.results.first() {
        Some(result) if result.get("queued").and_then(Value::as_bool) == Some(true) => {
            interview.scheduler_job_id = result
                .get("job_id")
                .and_then(|value| value.as_str())
                .map(str::to_string);
            interview.scheduler_status = "queued".to_string();
        }
        _ => {
            interview.scheduler_job_id = None;
            interview.scheduler_status = "declined".to_string();
        }
    }
}

pub(crate) fn on_interview_reminder_requested(state: RecruitmentState, ctx: &mut HookContext) {
    let interview_id = ctx
        .data
        .get("interview_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok());
    let Some(interview_id) = interview_id else {
        ctx.add_result(json!({ "sent": false, "error": "recruitment.interview.reminder_requested requires 'interview_id'" }));
        return;
    };
    let Some(interview) = state
        .interviews
        .get(&interview_id)
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({ "sent": false, "interview_id": interview_id.to_string(), "error": "Interview not found" }));
        return;
    };
    if interview.status != "scheduled" {
        ctx.add_result(json!({
            "sent": false,
            "interview_id": interview_id.to_string(),
            "reason": format!("Interview is {}", interview.status),
        }));
        return;
    }
    let applicant = state
        .applicants
        .get(&interview.applicant_id)
        .map(|entry| entry.value().clone());
    let applicant_name = applicant
        .as_ref()
        .map(|item| item.name.clone())
        .unwrap_or_default();
    let title = format!("Interview reminder: {applicant_name}");
    let body = format!(
        "Interview with {applicant_name} is scheduled for {} ({}).",
        interview.scheduled_at.to_rfc3339(),
        interview.interview_type.as_str()
    );
    let link = format!(
        "/api/v1/recruitment/applicants/{}/interviews",
        interview.applicant_id
    );
    let mut notified = 0;
    for interviewer in &interview.interviewers {
        state.notify(
            "recruitment.interview.reminder",
            interviewer,
            &title,
            &body,
            Some(link.clone()),
        );
        notified += 1;
    }
    ctx.add_result(json!({
        "sent": true,
        "interview_id": interview_id.to_string(),
        "recipients": notified,
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowed_stage_transition_cases() {
        use ApplicantStage::*;
        assert!(allowed_stage_transition(Applied, Screening));
        assert!(allowed_stage_transition(Applied, Rejected));
        assert!(allowed_stage_transition(Screening, Interview));
        assert!(allowed_stage_transition(Interview, Offer));
        assert!(allowed_stage_transition(Offer, Hired));
        assert!(allowed_stage_transition(Offer, Rejected));
        assert!(!allowed_stage_transition(Applied, Interview));
        assert!(!allowed_stage_transition(Applied, Offer));
        assert!(!allowed_stage_transition(Applied, Hired));
        assert!(!allowed_stage_transition(Applied, Applied));
        assert!(!allowed_stage_transition(Screening, Offer));
        assert!(!allowed_stage_transition(Hired, Rejected));
        assert!(!allowed_stage_transition(Hired, Applied));
        assert!(!allowed_stage_transition(Rejected, Applied));
        assert!(!allowed_stage_transition(Rejected, Rejected));
    }

    #[test]
    fn valid_email_cases() {
        assert!(valid_email("a@b.com"));
        assert!(valid_email("first.last+tag@example.co.id"));
        assert!(!valid_email(""));
        assert!(!valid_email("a@b"));
        assert!(!valid_email("a b@c.com"));
        assert!(!valid_email("@b.com"));
        assert!(!valid_email("a@.com"));
        assert!(!valid_email("a@b."));
        assert!(!valid_email("a@b@c.com"));
        assert!(!valid_email("a@"));
        // Consecutive dots and single-character / non-letter TLDs.
        assert!(!valid_email("a@b..c"));
        assert!(!valid_email("a@b..com"));
        assert!(!valid_email("a..b@c.com"));
        assert!(!valid_email("a@b.c"));
        assert!(!valid_email("a@b.1"));
        assert!(!valid_email("a@b.c0"));
        assert!(!valid_email(".a@b.com"));
        assert!(!valid_email("a.@b.com"));
        assert!(valid_email("a@b.co"));
        assert!(valid_email("a@b.museum"));
    }

    #[test]
    fn require_text_counts_characters_not_bytes() {
        // 5 multibyte chars are 10 bytes: within a 5-character limit.
        assert_eq!(
            require_text("ééééé", "name", 5).expect("5 chars within limit"),
            "ééééé"
        );
        assert!(require_text("éééééé", "name", 5).is_err());
        assert_eq!(
            require_text("  hello  ", "name", 5).expect("trims before counting"),
            "hello"
        );
        assert!(require_text("hello!", "name", 5).is_err());
        assert!(require_text("", "name", 5).is_err());
        assert!(require_text("   ", "name", 5).is_err());
    }

    #[test]
    fn valid_price_rounds_to_4dp() {
        assert_eq!(valid_price(10.12346).expect("rounds"), 10.1235);
        assert_eq!(valid_price(10.12344).expect("rounds"), 10.1234);
        assert_eq!(valid_price(5.0).expect("exact"), 5.0);
        assert!(valid_price(-1.0).is_err());
        assert!(valid_price(f64::NAN).is_err());
        assert!(valid_price(f64::INFINITY).is_err());
    }

    #[test]
    fn round_money_cases() {
        assert_eq!(round_money(2.5), 2.5);
        assert_eq!(round_money(1.23456789), 1.2346);
        assert_eq!(round_money(0.0), 0.0);
    }

    #[test]
    fn valid_currency_cases() {
        assert_eq!(valid_currency("usd").expect("lower"), "USD");
        assert_eq!(valid_currency("  idr ").expect("trimmed"), "IDR");
        assert!(valid_currency("US").is_err());
        assert!(valid_currency("USDD").is_err());
        assert!(valid_currency("u5d").is_err());
        assert!(valid_currency("").is_err());
    }

    #[test]
    fn validate_salary_range_cases() {
        assert_eq!(
            validate_salary_range(Some(50.0), Some(100.0)).expect("ok"),
            (Some(50.0), Some(100.0))
        );
        assert_eq!(
            validate_salary_range(None, None).expect("none"),
            (None, None)
        );
        assert!(validate_salary_range(Some(100.0), Some(50.0)).is_err());
        assert!(validate_salary_range(Some(-5.0), None).is_err());
        assert!(validate_salary_range(Some(50.0), Some(50.0)).is_ok());
    }

    #[test]
    fn validate_scores_cases() {
        let score = |criterion: &str, value: i64| EvaluationScore {
            criterion: criterion.to_string(),
            score: value,
            note: None,
        };
        assert!(validate_scores(&[]).is_err());
        assert!(validate_scores(&[score("skill", 4)]).is_ok());
        assert!(validate_scores(&[score("skill", 1), score("culture", 5)]).is_ok());
        assert!(validate_scores(&[score("skill", 0)]).is_err());
        assert!(validate_scores(&[score("skill", 6)]).is_err());
        assert!(validate_scores(&[score("skill", 4), score("skill", 3)]).is_err());
        // Duplicate detection is case-insensitive on the trimmed name.
        assert!(validate_scores(&[score("Skill", 4), score("skill", 3)]).is_err());
        assert!(validate_scores(&[score(" skill ", 4), score("SKILL", 3)]).is_err());
        assert!(validate_scores(&[score("Skill", 4), score("Culture", 3)]).is_ok());
        // Criterion / note limits count characters, not bytes.
        assert!(validate_scores(&[score(&"é".repeat(120), 4)]).is_ok());
        assert!(validate_scores(&[score(&"é".repeat(121), 4)]).is_err());
        let long_multibyte_note = EvaluationScore {
            criterion: "skill".to_string(),
            score: 3,
            note: Some("é".repeat(5000)),
        };
        assert!(validate_scores(&[long_multibyte_note]).is_ok());
        assert!(validate_scores(&[score("", 4)]).is_err());
        assert!(validate_scores(&[score(&"x".repeat(121), 4)]).is_err());
        let many: Vec<EvaluationScore> =
            (0..21).map(|i| score(&format!("c{i}"), 3)).collect();
        assert!(validate_scores(&many).is_err());
        let long_note = EvaluationScore {
            criterion: "skill".to_string(),
            score: 3,
            note: Some("x".repeat(5001)),
        };
        assert!(validate_scores(&[long_note]).is_err());
    }

    #[test]
    fn aggregate_scores_averages_per_criterion() {
        let state = RecruitmentState::default();
        let applicant = uuid::Uuid::new_v4();
        let job = uuid::Uuid::new_v4();
        let eval = |interviewer: &str, scores: Vec<EvaluationScore>| Evaluation {
            id: uuid::Uuid::new_v4(),
            applicant_id: applicant,
            job_id: job,
            interviewer_id: interviewer.to_string(),
            interview_id: None,
            scores,
            notes: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let score = |criterion: &str, value: i64| EvaluationScore {
            criterion: criterion.to_string(),
            score: value,
            note: None,
        };
        state.evaluations.insert(
            uuid::Uuid::new_v4(),
            eval("alice", vec![score("skill", 4), score("culture", 5)]),
        );
        state
            .evaluations
            .insert(uuid::Uuid::new_v4(), eval("bob", vec![score("skill", 2)]));
        let result = aggregate_scores(&state, applicant);
        assert_eq!(result["evaluations"].as_u64(), Some(2));
        assert_eq!(result["scores_total"].as_u64(), Some(3));
        let average = result["average_score"].as_f64().unwrap();
        assert!((average - 11.0 / 3.0).abs() < 1e-9);
        let criteria = result["criteria"].as_array().unwrap();
        let skill = criteria
            .iter()
            .find(|c| c["criterion"] == "skill")
            .expect("skill criterion");
        assert!((skill["average"].as_f64().unwrap() - 3.0).abs() < 1e-9);
        assert_eq!(skill["scores"].as_u64(), Some(2));
        let culture = criteria
            .iter()
            .find(|c| c["criterion"] == "culture")
            .expect("culture criterion");
        assert!((culture["average"].as_f64().unwrap() - 5.0).abs() < 1e-9);
    }

    #[test]
    fn aggregate_scores_empty_state() {
        let state = RecruitmentState::default();
        let result = aggregate_scores(&state, uuid::Uuid::new_v4());
        assert_eq!(result["evaluations"].as_u64(), Some(0));
        assert_eq!(result["scores_total"].as_u64(), Some(0));
        assert!(result["average_score"].is_null());
    }
}
