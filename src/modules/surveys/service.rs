// surveys module — Invitations, follow-ups, scoring.
//
// Split from the former single-file module `src/modules/surveys.rs`.
// The module root (`surveys.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use base64::Engine;
use chrono::{DateTime, Datelike, Duration, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::{CSAT_FOLLOWUP_MAX, CSAT_SATISFIED_MIN, DEFAULT_TOKEN_DAYS, MAX_ID_LEN, NPS_DETRACTOR_MAX, NPS_PROMOTER_MIN, SCHEDULER_KIND_SEND};
use super::types::{Survey, SurveyDistribution, SurveyQuestion, SurveyResponse};
use super::requests::DistributionRecipient;
use super::state::SurveysState;
use super::helpers::{answers_for_response, caller, created, generate_token, valid_email};

// ---------------------------------------------------------------------------
// Invitations, follow-ups, scoring
// ---------------------------------------------------------------------------

/// Issue one invitation (response row) for a recipient on one
/// survey version. Enforces one response per recipient per survey
/// version: returns `None` when the recipient already has a row
/// for this version. The token is unguessable and expires
/// `expires_in_days` after creation. Emits
/// `surveys.response.created` and a `notification.requested`
/// invitation for the recipient.
pub(crate) fn invite_recipient(
    state: &SurveysState,
    survey: &Survey,
    distribution: Option<&SurveyDistribution>,
    recipient_id: &str,
    recipient_email: Option<String>,
    agent_id: Option<String>,
    department_id: Option<String>,
    product_id: Option<String>,
) -> Option<SurveyResponse> {
    // Invitations only go out for published survey versions.
    if survey.status != "published" {
        return None;
    }
    if state
        .responses
        .iter()
        .any(|entry| entry.value().survey_id == survey.id && entry.value().recipient_id == recipient_id)
    {
        return None;
    }
    let expires_in_days = distribution.map(|item| item.expires_in_days).unwrap_or(DEFAULT_TOKEN_DAYS);
    let now = Utc::now();
    let response = SurveyResponse {
        id: Uuid::now_v7(),
        survey_id: survey.id,
        distribution_id: distribution.map(|item| item.id),
        recipient_id: recipient_id.to_string(),
        recipient_email: recipient_email.clone(),
        agent_id,
        department_id,
        product_id,
        token: generate_token(state),
        token_expires_at: now + Duration::days(expires_in_days),
        submitted_at: None,
        follow_up_status: "none".to_string(),
        follow_up_task_id: None,
        created_at: now,
        updated_at: now,
    };
    state.responses.insert(response.id, response.clone());
    state.emit(
        "surveys.response.created",
        json!({
            "survey_id": survey.id.to_string(),
            "response_id": response.id.to_string(),
            "recipient_id": recipient_id,
            "distribution_id": distribution.map(|item| item.id.to_string()),
            "token_expires_at": response.token_expires_at.to_rfc3339(),
        }),
    );
    state.notify(
        "surveys.invitation",
        recipient_id,
        &format!("Please share your feedback: {}", survey.title),
        &format!(
            "You are invited to answer the survey \"{}\". Your response link expires on {}.",
            survey.title,
            response.token_expires_at.format("%Y-%m-%d")
        ),
        Some(format!("/api/v1/surveys/public/{}", response.token)),
    );
    Some(response)
}

/// Send the pending invitations of one distribution: the stored
/// pending recipients (supplied at creation) plus any extra
/// recipients passed by the caller. For trigger distributions
/// (`ticket_closed`, `purchase`) this only sends explicitly passed
/// recipients — their real invitations are created by the trigger
/// listeners. Returns how many invitations were newly issued.
pub(crate) fn send_distribution_invitations(
    state: &SurveysState,
    distribution: &mut SurveyDistribution,
    mut recipients: Vec<DistributionRecipient>,
) -> UnifiedResult<usize> {
    let survey = state
        .surveys
        .get(&distribution.survey_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Survey not found: {}", distribution.survey_id)))?;
    if survey.status != "published" {
        return Err(UnifiedError::conflict(
            "Invitations can only be sent for a published survey version",
        ));
    }
    if let Some((_, stored)) = state.pending_recipients.remove(&distribution.id) {
        recipients.extend(stored);
    }
    let mut issued = 0usize;
    let mut skipped = 0usize;
    for recipient in &recipients {
        let recipient_id = recipient.recipient_id.trim();
        if recipient_id.is_empty() || recipient_id.len() > MAX_ID_LEN {
            skipped += 1;
            continue;
        }
        if let Some(email) = &recipient.recipient_email {
            if !valid_email(email) {
                skipped += 1;
                continue;
            }
        }
        let invited = invite_recipient(
            state,
            &survey,
            Some(distribution),
            recipient_id,
            recipient.recipient_email.clone(),
            recipient
                .agent_id
                .clone()
                .or_else(|| distribution.agent_id.clone()),
            recipient
                .department_id
                .clone()
                .or_else(|| distribution.department_id.clone()),
            recipient
                .product_id
                .clone()
                .or_else(|| distribution.product_id.clone()),
        );
        if invited.is_some() {
            issued += 1;
        } else {
            skipped += 1;
        }
    }
    distribution.sent_count += issued as i64;
    // Only a send that actually issued invitations counts as sent:
    // with nothing issued the distribution stays pending and the
    // skipped recipients are reported in the event below.
    if issued > 0 && (distribution.trigger == "manual" || distribution.trigger == "scheduled") {
        distribution.status = "sent".to_string();
    }
    distribution.updated_at = Utc::now();
    state.distributions.insert(distribution.id, distribution.clone());
    state.emit(
        "surveys.distribution.sent",
        json!({
            "survey_id": survey.id.to_string(),
            "distribution_id": distribution.id.to_string(),
            "issued": issued,
            "skipped": skipped,
            "sent_count": distribution.sent_count,
        }),
    );
    Ok(issued)
}

/// Handle a low score on a submitted response: an NPS detractor
/// (0–6) or a CSAT score of 1–2. Requests a follow-up task from
/// the tasks module via `task.create_requested` (request/response:
/// a handler that answers contributes a `task_id` / `id` result),
/// stores the follow-up link on the response, notifies the
/// responsible user when one is configured, and emits
/// `surveys.followup.created`. The follow-up stays `requested`
/// with no task id when no handler answers (see the HONESTY note).
pub(crate) fn create_followup(
    state: &SurveysState,
    survey: &Survey,
    response: &mut SurveyResponse,
    reason: &str,
    low_score: i32,
) {
    let title = format!(
        "Follow up low survey score ({}: {})",
        survey.title, low_score
    );
    let body = format!(
        "Survey \"{}\" (version {}) received a low score ({}) from recipient \"{}\". Response id: {}. Please contact the customer and record the outcome.",
        survey.title, survey.version, reason, response.recipient_id, response.id
    );
    let responsible: Option<String> = match response.distribution_id {
        Some(distribution_id) => state
            .distributions
            .get(&distribution_id)
            .and_then(|entry| entry.value().responsible_user_id.clone()),
        None => None,
    };
    let answer = state.emit_ctx(
        "task.create_requested",
        json!({
            "source": "surveys",
            "title": title,
            "body": body,
            "assignee_id": responsible,
            "department_id": response.department_id,
            "survey_id": survey.id.to_string(),
            "response_id": response.id.to_string(),
        }),
    );
    let mut task_id: Option<String> = None;
    for result in &answer.results {
        if let Some(id) = result
            .get("task_id")
            .or_else(|| result.get("id"))
            .and_then(|value| value.as_str())
        {
            task_id = Some(id.to_string());
            break;
        }
    }
    response.follow_up_task_id = task_id.clone();
    response.follow_up_status = if task_id.is_some() {
        "created".to_string()
    } else {
        "requested".to_string()
    };
    response.updated_at = Utc::now();
    state.responses.insert(response.id, response.clone());
    state.emit(
        "surveys.followup.created",
        json!({
            "survey_id": survey.id.to_string(),
            "response_id": response.id.to_string(),
            "reason": reason,
            "low_score": low_score,
            "task_id": task_id,
            "status": response.follow_up_status,
        }),
    );
    if let Some(user_id) = responsible {
        state.notify(
            "surveys.followup.created",
            &user_id,
            &format!("Low survey score needs follow-up: {}", survey.title),
            &body,
            Some(format!("/api/v1/surveys/responses/{}", response.id)),
        );
    }
}

/// Inspect a submitted response for low scores and create at most
/// one follow-up per response. Precedence is by type, not raw
/// score: any NPS detractor beats any low CSAT, and among NPS
/// detractors the lowest score wins. Answers are considered in
/// question-position order so the choice is deterministic.
pub(crate) fn maybe_create_followup(state: &SurveysState, survey: &Survey, response: &mut SurveyResponse) {
    if response.follow_up_status != "none" {
        return;
    }
    let answers = answers_for_response(state, response.id);
    let mut scored: Vec<(u32, String, i32)> = Vec::new();
    for answer in &answers {
        let Some(question) = state.questions.get(&answer.question_id) else {
            continue;
        };
        let Some(score) = answer.score else {
            continue;
        };
        scored.push((question.position, question.question_type.clone(), score));
    }
    scored.sort_by_key(|(position, _, _)| *position);
    let mut worst_nps: Option<i32> = None;
    let mut worst_csat: Option<i32> = None;
    for (_, question_type, score) in &scored {
        if question_type == "nps" && *score <= NPS_DETRACTOR_MAX {
            worst_nps = Some(worst_nps.map_or(*score, |worst| worst.min(*score)));
        } else if question_type == "csat" && *score <= CSAT_FOLLOWUP_MAX {
            worst_csat = Some(worst_csat.map_or(*score, |worst| worst.min(*score)));
        }
    }
    if let Some(low_score) = worst_nps {
        create_followup(state, survey, response, "nps_detractor", low_score);
    } else if let Some(low_score) = worst_csat {
        create_followup(state, survey, response, "csat_low", low_score);
    }
}

/// Per-question score summary, computed on read from the submitted
/// responses. `nps` is %promoters minus %detractors; `csat` is
/// % satisfied (score 4–5). Counts, averages, and the score
/// distribution are included for every scored question.
pub(crate) fn question_stats(
    state: &SurveysState,
    question: &SurveyQuestion,
    responses: &[SurveyResponse],
) -> Value {
    let submitted_ids: Vec<Uuid> = responses
        .iter()
        .filter(|response| response.submitted_at.is_some())
        .map(|response| response.id)
        .collect();
    let scores: Vec<i32> = state
        .answers
        .iter()
        .filter(|entry| {
            entry.value().question_id == question.id
                && submitted_ids.contains(&entry.value().response_id)
        })
        .filter_map(|entry| entry.value().score)
        .collect();
    let total = scores.len() as f64;
    let mut distribution: HashMap<String, u64> = HashMap::new();
    for score in &scores {
        *distribution.entry(score.to_string()).or_default() += 1;
    }
    let mut stats = json!({
        "question_id": question.id.to_string(),
        "question_type": question.question_type,
        "text": question.text,
        "responses": scores.len() as u64,
        "distribution": distribution,
    });
    if total > 0.0 {
        let sum: i32 = scores.iter().sum();
        let average = (sum as f64 / total * 100.0).round() / 100.0;
        if let Value::Object(map) = &mut stats {
            map.insert("average".to_string(), json!(average));
        }
        if question.question_type == "nps" {
            let promoters = scores.iter().filter(|score| **score >= NPS_PROMOTER_MIN).count() as f64;
            let detractors = scores.iter().filter(|score| **score <= NPS_DETRACTOR_MAX).count() as f64;
            let nps = ((promoters - detractors) / total * 100.0).round();
            if let Value::Object(map) = &mut stats {
                map.insert("nps".to_string(), json!(nps));
                map.insert("promoters".to_string(), json!(promoters as u64));
                map.insert("detractors".to_string(), json!(detractors as u64));
                map.insert("passives".to_string(), json!((total - promoters - detractors) as u64));
            }
        }
        if question.question_type == "csat" {
            let satisfied = scores.iter().filter(|score| **score >= CSAT_SATISFIED_MIN).count() as f64;
            let dissatisfied = scores.iter().filter(|score| **score <= CSAT_FOLLOWUP_MAX).count() as f64;
            let csat = (satisfied / total * 100.0).round();
            if let Value::Object(map) = &mut stats {
                map.insert("csat".to_string(), json!(csat));
                map.insert("satisfied".to_string(), json!(satisfied as u64));
                map.insert("dissatisfied".to_string(), json!(dissatisfied as u64));
            }
        }
    }
    stats
}

/// Bucket key for a trend bucket: the day (`YYYY-MM-DD`) for
/// `daily`, the Monday of the week (`YYYY-MM-DD`) for `weekly`.
pub(crate) fn bucket_key(date: DateTime<Utc>, bucket: &str) -> String {
    if bucket == "weekly" {
        let weekday = date.weekday().num_days_from_monday() as i64;
        let monday = date.date_naive() - chrono::Duration::days(weekday);
        monday.format("%Y-%m-%d").to_string()
    } else {
        date.format("%Y-%m-%d").to_string()
    }
}

/// Trend buckets for one survey: per bucket, submitted count, NPS
/// (over NPS answers in the bucket), and CSAT (over CSAT answers
/// in the bucket).
pub(crate) fn trend_buckets(
    state: &SurveysState,
    responses: &[SurveyResponse],
    bucket: &str,
) -> Vec<Value> {
    let mut buckets: HashMap<String, (Vec<i32>, Vec<i32>, u64)> = HashMap::new();
    for response in responses {
        let Some(submitted_at) = response.submitted_at else {
            continue;
        };
        let key = bucket_key(submitted_at, bucket);
        let entry = buckets.entry(key).or_default();
        entry.2 += 1;
        for answer in answers_for_response(state, response.id) {
            let Some(question) = state.questions.get(&answer.question_id) else {
                continue;
            };
            let Some(score) = answer.score else {
                continue;
            };
            if question.question_type == "nps" {
                entry.0.push(score);
            } else if question.question_type == "csat" {
                entry.1.push(score);
            }
        }
    }
    let mut keys: Vec<String> = buckets.keys().cloned().collect();
    keys.sort();
    keys.into_iter()
        .map(|key| {
            let (nps_scores, csat_scores, count) = buckets.remove(&key).unwrap_or_default();
            let nps = if nps_scores.is_empty() {
                Value::Null
            } else {
                let total = nps_scores.len() as f64;
                let promoters = nps_scores.iter().filter(|score| **score >= NPS_PROMOTER_MIN).count() as f64;
                let detractors = nps_scores.iter().filter(|score| **score <= NPS_DETRACTOR_MAX).count() as f64;
                json!(((promoters - detractors) / total * 100.0).round())
            };
            let csat = if csat_scores.is_empty() {
                Value::Null
            } else {
                let total = csat_scores.len() as f64;
                let satisfied = csat_scores.iter().filter(|score| **score >= CSAT_SATISFIED_MIN).count() as f64;
                json!((satisfied / total * 100.0).round())
            };
            json!({
                "bucket": key,
                "responses": count,
                "nps": nps,
                "csat": csat,
            })
        })
        .collect()
}

/// Queue the `surveys.send` scheduler job for a scheduled
/// distribution. Mirrors the events module's `schedule_reminder`:
/// the scheduler answers `{"queued": true, "job_id": ...}` when it
/// knows the kind, and `{"queued": false, "error": ...}` today,
/// because `surveys.send` is not in `KNOWN_KINDS` yet. The
/// distribution is stored either way; the outcome is recorded
/// honestly on `scheduler_status` / `scheduler_job_id`.
pub(crate) fn schedule_distribution(state: &SurveysState, distribution: &mut SurveyDistribution) {
    let Some(bus) = state.bus() else {
        distribution.scheduler_status = "declined".to_string();
        return;
    };
    let Some(scheduled_at) = distribution.scheduled_at else {
        distribution.scheduler_status = "not_scheduled".to_string();
        return;
    };
    let answer = bus.emit(
        "scheduler.enqueue",
        json!({
            "kind": SCHEDULER_KIND_SEND,
            "payload": {
                "survey_id": distribution.survey_id.to_string(),
                "distribution_id": distribution.id.to_string(),
                "send_at": scheduled_at.to_rfc3339(),
            },
        }),
    );
    match answer.results.first() {
        Some(result) if result.get("queued").and_then(Value::as_bool) == Some(true) => {
            distribution.scheduler_job_id = result
                .get("job_id")
                .and_then(|value| value.as_str())
                .map(str::to_string);
            distribution.scheduler_status = "queued".to_string();
        }
        _ => {
            distribution.scheduler_job_id = None;
            distribution.scheduler_status = "declined".to_string();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::surveys::SurveyAnswer;
    use chrono::{DateTime, NaiveDate, Utc};

    fn datetime(year: i32, month: u32, day: u32) -> DateTime<Utc> {
        DateTime::from_naive_utc_and_offset(
            NaiveDate::from_ymd_opt(year, month, day)
                .unwrap()
                .and_hms_opt(12, 0, 0)
                .unwrap(),
            Utc,
        )
    }

    #[test]
    fn bucket_key_uses_the_plain_date_for_daily_buckets() {
        assert_eq!(bucket_key(datetime(2026, 11, 18), "daily"), "2026-11-18");
        assert_eq!(bucket_key(datetime(2026, 11, 18), "bogus"), "2026-11-18");
    }

    #[test]
    fn bucket_key_uses_the_monday_of_the_week_for_weekly_buckets() {
        // 2026-11-18 is a Wednesday; its week starts Monday 2026-11-16.
        assert_eq!(bucket_key(datetime(2026, 11, 18), "weekly"), "2026-11-16");
        assert_eq!(bucket_key(datetime(2026, 11, 16), "weekly"), "2026-11-16");
        // 2026-11-22 is a Sunday; it still belongs to the same week.
        assert_eq!(bucket_key(datetime(2026, 11, 22), "weekly"), "2026-11-16");
    }

    fn survey(status: &str) -> Survey {
        let now = Utc::now();
        let id = Uuid::new_v4();
        Survey {
            id,
            family_id: id,
            version: 1,
            supersedes: None,
            title: "Feedback".to_string(),
            description: None,
            status: status.to_string(),
            created_by: "test".to_string(),
            created_at: now,
            updated_at: now,
        }
    }

    fn response(survey_id: Uuid, recipient_id: &str) -> SurveyResponse {
        let now = Utc::now();
        SurveyResponse {
            id: Uuid::new_v4(),
            survey_id,
            distribution_id: None,
            recipient_id: recipient_id.to_string(),
            recipient_email: None,
            agent_id: None,
            department_id: None,
            product_id: None,
            token: "token-1".to_string(),
            token_expires_at: now + Duration::days(30),
            submitted_at: None,
            follow_up_status: "none".to_string(),
            follow_up_task_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn question(survey_id: Uuid, position: u32, question_type: &str) -> SurveyQuestion {
        let now = Utc::now();
        SurveyQuestion {
            id: Uuid::new_v4(),
            survey_id,
            position,
            question_type: question_type.to_string(),
            text: "Question".to_string(),
            required: false,
            options: Vec::new(),
            min_value: None,
            max_value: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn answer(response_id: Uuid, question_id: Uuid, score: i32, created_at: DateTime<Utc>) -> SurveyAnswer {
        SurveyAnswer {
            id: Uuid::new_v4(),
            response_id,
            question_id,
            score: Some(score),
            text: None,
            choice: None,
            created_at,
        }
    }

    fn distribution(survey_id: Uuid, trigger: &str) -> SurveyDistribution {
        let now = Utc::now();
        SurveyDistribution {
            id: Uuid::new_v4(),
            survey_id,
            trigger: trigger.to_string(),
            department_id: None,
            product_id: None,
            agent_id: None,
            responsible_user_id: None,
            scheduled_at: None,
            expires_in_days: 30,
            status: "pending".to_string(),
            scheduler_status: "not_scheduled".to_string(),
            scheduler_job_id: None,
            sent_count: 0,
            created_by: "test".to_string(),
            created_at: now,
            updated_at: now,
        }
    }

    fn followup_events(state: &SurveysState) -> Arc<std::sync::Mutex<Vec<Value>>> {
        let seen: Arc<std::sync::Mutex<Vec<Value>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
        let bus = Arc::new(HookBus::new());
        let seen_clone = Arc::clone(&seen);
        bus.register(
            "surveys.followup.created",
            "test",
            Arc::new(move |ctx: &mut HookContext| {
                seen_clone
                    .lock()
                    .expect("seen lock poisoned")
                    .push(ctx.data.clone());
            }),
        );
        *state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(bus);
        seen
    }

    #[test]
    fn maybe_create_followup_prefers_any_nps_detractor_over_a_lower_csat() {
        let state = SurveysState::default();
        let survey = survey("published");
        state.surveys.insert(survey.id, survey.clone());
        let mut response = response(survey.id, "user-1");
        state.responses.insert(response.id, response.clone());
        // The CSAT answer was created first, so creation order would
        // surface it before the NPS answer; position order must win.
        let nps = question(survey.id, 1, "nps");
        let csat = question(survey.id, 2, "csat");
        state.questions.insert(nps.id, nps.clone());
        state.questions.insert(csat.id, csat.clone());
        let early = datetime(2026, 11, 18);
        let late = datetime(2026, 11, 19);
        state.answers.insert(
            Uuid::new_v4(),
            answer(response.id, csat.id, 1, early),
        );
        state.answers.insert(
            Uuid::new_v4(),
            answer(response.id, nps.id, 5, late),
        );
        let seen = followup_events(&state);
        maybe_create_followup(&state, &survey, &mut response);
        let seen = seen.lock().expect("seen lock poisoned");
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0]["reason"], json!("nps_detractor"));
        assert_eq!(seen[0]["low_score"], json!(5));
    }

    #[test]
    fn maybe_create_followup_takes_the_lowest_score_among_nps_detractors() {
        let state = SurveysState::default();
        let survey = survey("published");
        state.surveys.insert(survey.id, survey.clone());
        let mut response = response(survey.id, "user-1");
        state.responses.insert(response.id, response.clone());
        let first = question(survey.id, 1, "nps");
        let second = question(survey.id, 2, "nps");
        let csat = question(survey.id, 3, "csat");
        state.questions.insert(first.id, first.clone());
        state.questions.insert(second.id, second.clone());
        state.questions.insert(csat.id, csat.clone());
        let at = datetime(2026, 11, 18);
        state.answers.insert(
            Uuid::new_v4(),
            answer(response.id, first.id, 6, at),
        );
        state.answers.insert(
            Uuid::new_v4(),
            answer(response.id, second.id, 2, at),
        );
        state.answers.insert(
            Uuid::new_v4(),
            answer(response.id, csat.id, 1, at),
        );
        let seen = followup_events(&state);
        maybe_create_followup(&state, &survey, &mut response);
        let seen = seen.lock().expect("seen lock poisoned");
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0]["reason"], json!("nps_detractor"));
        assert_eq!(seen[0]["low_score"], json!(2));
    }

    #[test]
    fn send_distribution_invitations_stays_pending_when_nothing_is_issued() {
        let state = SurveysState::default();
        let survey = survey("published");
        state.surveys.insert(survey.id, survey.clone());
        // The recipient already has a response row for this survey
        // version, so the send has nothing new to issue.
        let existing = response(survey.id, "user-1");
        state.responses.insert(existing.id, existing.clone());
        let mut distribution = distribution(survey.id, "manual");
        state.distributions.insert(distribution.id, distribution.clone());
        let issued = send_distribution_invitations(
            &state,
            &mut distribution,
            vec![DistributionRecipient {
                recipient_id: "user-1".to_string(),
                recipient_email: None,
                agent_id: None,
                department_id: None,
                product_id: None,
            }],
        )
        .expect("send succeeds");
        assert_eq!(issued, 0);
        assert_eq!(distribution.status, "pending");
        assert_eq!(distribution.sent_count, 0);
    }

    #[test]
    fn send_distribution_invitations_marks_sent_when_invitations_are_issued() {
        let state = SurveysState::default();
        let survey = survey("published");
        state.surveys.insert(survey.id, survey.clone());
        let mut distribution = distribution(survey.id, "manual");
        state.distributions.insert(distribution.id, distribution.clone());
        let issued = send_distribution_invitations(
            &state,
            &mut distribution,
            vec![DistributionRecipient {
                recipient_id: "user-1".to_string(),
                recipient_email: None,
                agent_id: None,
                department_id: None,
                product_id: None,
            }],
        )
        .expect("send succeeds");
        assert_eq!(issued, 1);
        assert_eq!(distribution.status, "sent");
        assert_eq!(distribution.sent_count, 1);
    }

    #[test]
    fn invitations_refuse_unpublished_surveys() {
        let state = SurveysState::default();
        let survey = survey("draft");
        state.surveys.insert(survey.id, survey.clone());
        assert!(
            invite_recipient(&state, &survey, None, "user-1", None, None, None, None).is_none(),
            "a draft survey must not invite anyone"
        );
        let mut distribution = distribution(survey.id, "manual");
        state.distributions.insert(distribution.id, distribution.clone());
        let error = send_distribution_invitations(&state, &mut distribution, Vec::new())
            .unwrap_err();
        assert!(error.to_string().contains("published"));
    }
}
