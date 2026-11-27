// Marketing Automation - Sequence engine: step decisions and enrollment advancement.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json as api_json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::*;
use super::types::*;
use super::store::*;
use super::helpers::*;
use super::validation::*;
use super::service::*;
use super::handlers_campaigns::*;
use super::handlers_segments::*;
use super::handlers_sequences::*;
use super::handlers_scores::*;
use super::handlers_audience::*;
use super::hooks::*;
use super::module::*;

// Sequence step execution.

pub(crate) fn sequence_steps(state: &MarketingState, sequence_id: &str) -> Vec<SequenceStep> {
    let mut items: Vec<SequenceStep> = state
        .steps
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|step| step.sequence_id == sequence_id)
        .collect();
    items.sort_by(|a, b| a.order.cmp(&b.order));
    items
}

/// First step at or after `order` (steps are sorted by order).
/// Step orders may have gaps: a step can be deleted after
/// enrollments already point at it, and callers may choose
/// non-contiguous orders. An exact-order lookup would find
/// nothing at a gap and wrongly complete the enrollment, so
/// every lookup of "the next step" goes through here.
pub(crate) fn next_step_at_or_after<'a>(
    steps: &'a [SequenceStep],
    order: i64,
) -> Option<&'a SequenceStep> {
    steps.iter().find(|step| step.order >= order)
}

/// Evaluate one step condition for an enrollment. Returns
/// `StepDecision::Ready` when the step may run now, `Waiting` when
/// the condition window has not expired yet, and `Skip` when the
/// window expired without the condition ever holding.
pub(crate) enum StepDecision {
    Ready,
    Waiting,
    Skip,
}

pub(crate) fn decide_step(state: &MarketingState, enrollment: &Enrollment, step: &SequenceStep) -> StepDecision {
    let condition = &step.condition;
    let condition_type = condition
        .get("type")
        .and_then(|value| value.as_str())
        .unwrap_or("always");
    if condition_type == "always" {
        return StepDecision::Ready;
    }
    let Some(ref_step_id) = condition
        .get("step_id")
        .and_then(|value| value.as_str())
    else {
        // A branching condition without a referenced step can never
        // be satisfied; treat it as a configuration error and skip
        // the step rather than blocking the enrollment forever.
        return StepDecision::Skip;
    };
    let window_hours = condition
        .get("window_hours")
        .and_then(|value| value.as_u64())
        .unwrap_or(72);
    let ref_send = state
        .sends
        .iter()
        .map(|entry| entry.value().clone())
        .find(|send| {
            send.enrollment_id.as_deref() == Some(enrollment.id.as_str())
                && send.step_id.as_deref() == Some(ref_step_id)
        });
    let Some(ref_send) = ref_send else {
        return StepDecision::Skip;
    };
    let sent_at = DateTime::parse_from_rfc3339(&ref_send.created_at)
        .map(|parsed| parsed.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now());
    let window_end = sent_at + chrono::Duration::hours(window_hours as i64);
    // A click implies an open, but an open does not imply a click:
    // the two conditions must be evaluated separately.
    let opened = ref_send.status == "opened" || ref_send.status == "clicked";
    let clicked = ref_send.status == "clicked";
    let event_happened = match condition_type {
        "opened" => opened,
        "clicked" => clicked,
        "did_not_open" => !opened,
        "did_not_click" => !clicked,
        _ => return StepDecision::Skip,
    };
    match condition_type {
        "opened" | "clicked" => {
            if event_happened {
                StepDecision::Ready
            } else if Utc::now() >= window_end {
                StepDecision::Skip
            } else {
                StepDecision::Waiting
            }
        }
        _ => {
            if Utc::now() >= window_end {
                if event_happened {
                    StepDecision::Ready
                } else {
                    StepDecision::Skip
                }
            } else {
                StepDecision::Waiting
            }
        }
    }
}

pub(crate) fn push_enrollment_history(
    enrollment: &mut Enrollment,
    step: &SequenceStep,
    action: &str,
    detail: &str,
) {
    enrollment.history.push(EnrollmentStepEvent {
        step_id: step.id.clone(),
        step_order: step.order,
        action: action.to_string(),
        detail: detail.to_string(),
        at: now_string(),
    });
}

/// Run the next due step of one enrollment. Returns true when the
/// enrollment advanced (sent or skipped a step).
pub(crate) fn advance_enrollment(state: &Arc<MarketingState>, enrollment_id: &str) -> UnifiedResult<bool> {
    let mut enrollment = state
        .enrollments
        .get(enrollment_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Enrollment not found"))?;
    if enrollment.status != "active" {
        return Ok(false);
    }
    if let Some(next_at) = &enrollment.next_step_at {
        if let Ok(when) = DateTime::parse_from_rfc3339(next_at) {
            if when.with_timezone(&Utc) > Utc::now() {
                return Ok(false);
            }
        }
    }
    let steps = sequence_steps(state, &enrollment.sequence_id);
    let Some(step) = next_step_at_or_after(&steps, enrollment.next_step).cloned() else {
        enrollment.status = "completed".to_string();
        enrollment.next_step = 0;
        enrollment.next_step_at = None;
        enrollment.updated_at = now_string();
        state
            .enrollments
            .insert(enrollment.id.clone(), enrollment.clone());
        state.emit(
            EVENT_MARKETING_SEQUENCE_COMPLETED,
            json!({
                "enrollment_id": enrollment.id,
                "sequence_id": enrollment.sequence_id,
                "contact_id": enrollment.contact_id,
            }),
        );
        apply_score_action(state, &enrollment.contact_id, &enrollment.email, "sequence.completed");
        return Ok(true);
    };
    // Align the pointer with the step actually selected, so a gap
    // below it is never revisited on the next advance.
    enrollment.next_step = step.order;

    match decide_step(state, &enrollment, &step) {
        StepDecision::Waiting => {
            // Recheck in one hour; the condition window may still close.
            enrollment.next_step_at =
                Some((Utc::now() + chrono::Duration::hours(1)).to_rfc3339());
            enrollment.updated_at = now_string();
            state
                .enrollments
                .insert(enrollment.id.clone(), enrollment);
            Ok(false)
        }
        StepDecision::Skip => {
            push_enrollment_history(
                &mut enrollment,
                &step,
                "skipped",
                "Step condition was not met within its window",
            );
            enrollment.next_step = step.order + 1;
            let next = next_step_at_or_after(&steps, enrollment.next_step);
            enrollment.next_step_at = next.map(|next_step| {
                (Utc::now() + chrono::Duration::seconds(next_step.delay_seconds)).to_rfc3339()
            });
            enrollment.updated_at = now_string();
            state
                .enrollments
                .insert(enrollment.id.clone(), enrollment);
            Ok(true)
        }
        StepDecision::Ready => {
            if is_suppressed(state, &enrollment.email, None) {
                record_skipped_send(
                    state,
                    None,
                    Some(&enrollment.sequence_id),
                    Some(&step.id),
                    Some(&enrollment.id),
                    &enrollment.contact_id,
                    &enrollment.email,
                    None,
                    "unsubscribed",
                );
                push_enrollment_history(
                    &mut enrollment,
                    &step,
                    "skipped",
                    "Contact unsubscribed; step skipped and enrollment stopped",
                );
                enrollment.status = "unsubscribed".to_string();
                enrollment.next_step_at = None;
                enrollment.updated_at = now_string();
                state
                    .enrollments
                    .insert(enrollment.id.clone(), enrollment);
                return Ok(true);
            }
            if let Err(reason) = consent_gate(state, &enrollment.contact_id, &enrollment.email) {
                record_skipped_send(
                    state,
                    None,
                    Some(&enrollment.sequence_id),
                    Some(&step.id),
                    Some(&enrollment.id),
                    &enrollment.contact_id,
                    &enrollment.email,
                    None,
                    &reason,
                );
                push_enrollment_history(
                    &mut enrollment,
                    &step,
                    "skipped",
                    &format!("Send skipped: {reason}"),
                );
                enrollment.next_step = step.order + 1;
                let next = next_step_at_or_after(&steps, enrollment.next_step);
                enrollment.next_step_at = next.map(|next_step| {
                    (Utc::now() + chrono::Duration::seconds(next_step.delay_seconds)).to_rfc3339()
                });
                if next.is_none() {
                    enrollment.status = "completed".to_string();
                    enrollment.next_step = 0;
                }
                enrollment.updated_at = now_string();
                state
                    .enrollments
                    .insert(enrollment.id.clone(), enrollment);
                return Ok(true);
            }
            let send_id = new_id();
            let mail_payload = json!({
                "to": enrollment.email,
                "subject": step.subject,
                "body_text": step.body_text,
                "body_html": step.body_html,
                "template_id": step.template_id,
                "variables": {
                    "contact_id": enrollment.contact_id,
                    "sequence_id": enrollment.sequence_id,
                    "step_id": step.id,
                    "send_id": send_id,
                },
            });
            match request_mail_send(state, mail_payload) {
                Ok(job_id) => {
                    state.sends.insert(
                        send_id.clone(),
                        MarketingSend {
                            id: send_id,
                            campaign_id: None,
                            sequence_id: Some(enrollment.sequence_id.clone()),
                            step_id: Some(step.id.clone()),
                            enrollment_id: Some(enrollment.id.clone()),
                            contact_id: enrollment.contact_id.clone(),
                            email: enrollment.email.clone(),
                            variant_id: None,
                            status: "queued".to_string(),
                            mail_job_id: Some(job_id),
                            skip_reason: None,
                            created_at: now_string(),
                            updated_at: now_string(),
                        },
                    );
                    push_enrollment_history(&mut enrollment, &step, "sent", "Step message queued with the mail service");
                    enrollment.next_step = step.order + 1;
                    let next = next_step_at_or_after(&steps, enrollment.next_step);
                    enrollment.next_step_at = next.map(|next_step| {
                        (Utc::now() + chrono::Duration::seconds(next_step.delay_seconds))
                            .to_rfc3339()
                    });
                    if next.is_none() {
                        enrollment.status = "completed".to_string();
                        enrollment.next_step = 0;
                        enrollment.next_step_at = None;
                    }
                    enrollment.updated_at = now_string();
                    state
                        .enrollments
                        .insert(enrollment.id.clone(), enrollment.clone());
                    state.emit(
                        EVENT_MARKETING_SEQUENCE_STEP_SENT,
                        json!({
                            "enrollment_id": enrollment.id,
                            "sequence_id": enrollment.sequence_id,
                            "step_id": step.id,
                            "contact_id": enrollment.contact_id,
                        }),
                    );
                    if enrollment.status == "completed" {
                        state.emit(
                            EVENT_MARKETING_SEQUENCE_COMPLETED,
                            json!({
                                "enrollment_id": enrollment.id,
                                "sequence_id": enrollment.sequence_id,
                                "contact_id": enrollment.contact_id,
                            }),
                        );
                        apply_score_action(
                            state,
                            &enrollment.contact_id,
                            &enrollment.email,
                            "sequence.completed",
                        );
                    }
                    Ok(true)
                }
                Err(error) => Err(UnifiedError::new(
                    format!("Mail service refused the step message: {}", error.message),
                    503,
                )),
            }
        }
    }
}

/// Run every due step of every active enrollment. Returns
/// (advanced, waiting, errors).
pub(crate) fn process_due_enrollments(state: &Arc<MarketingState>) -> (u64, u64, u64) {
    let due_ids: Vec<String> = state
        .enrollments
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|enrollment| {
            enrollment.status == "active"
                && enrollment
                    .next_step_at
                    .as_deref()
                    .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
                    .is_some_and(|when| when.with_timezone(&Utc) <= Utc::now())
        })
        .map(|enrollment| enrollment.id)
        .collect();
    let mut advanced = 0u64;
    let mut waiting = 0u64;
    let mut errors = 0u64;
    for id in due_ids {
        match advance_enrollment(state, &id) {
            Ok(true) => advanced += 1,
            Ok(false) => waiting += 1,
            Err(_) => errors += 1,
        }
    }
    (advanced, waiting, errors)
}

/// Enroll one contact in a sequence. Returns the enrollment, or
/// None when the contact is already enrolled and active.
pub(crate) fn enroll_contact(
    state: &Arc<MarketingState>,
    sequence_id: &str,
    contact_id: &str,
    email: &str,
) -> UnifiedResult<Option<Enrollment>> {
    let sequence = state
        .sequences
        .get(sequence_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Sequence not found"))?;
    if sequence.status != "active" {
        return Err(UnifiedError::conflict(
            "Sequence is not active; only active sequences accept enrollments",
        ));
    }
    if state
        .enrollments
        .iter()
        .any(|entry| {
            entry.value().sequence_id == sequence_id
                && entry.value().contact_id == contact_id
                && entry.value().status == "active"
        })
    {
        return Ok(None);
    }
    if is_suppressed(state, email, None) {
        return Err(UnifiedError::conflict(
            "Contact is unsubscribed and cannot be enrolled",
        ));
    }
    let steps = sequence_steps(state, sequence_id);
    let first_delay = steps.first().map(|step| step.delay_seconds).unwrap_or(0);
    // Start at the first step that actually exists: the lowest
    // order may not be 1 when callers chose non-contiguous orders.
    let first_order = steps.first().map(|step| step.order).unwrap_or(1);
    let now = now_string();
    let enrollment = Enrollment {
        id: new_id(),
        sequence_id: sequence_id.to_string(),
        contact_id: contact_id.to_string(),
        email: email.to_string(),
        next_step: first_order,
        status: "active".to_string(),
        next_step_at: Some(
            (Utc::now() + chrono::Duration::seconds(first_delay)).to_rfc3339(),
        ),
        history: Vec::new(),
        created_at: now.clone(),
        updated_at: now,
    };
    state
        .enrollments
        .insert(enrollment.id.clone(), enrollment.clone());
    state.emit(
        EVENT_MARKETING_SEQUENCE_ENROLLED,
        json!({
            "enrollment_id": enrollment.id,
            "sequence_id": sequence_id,
            "contact_id": contact_id,
            "email": email,
        }),
    );
    Ok(Some(enrollment))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_sequence(id: &str) -> Sequence {
        Sequence {
            id: id.to_string(),
            name: "Test sequence".to_string(),
            description: String::new(),
            trigger_kind: "manual".to_string(),
            trigger_form_id: None,
            trigger_tag: None,
            status: "active".to_string(),
            created_at: now_string(),
            updated_at: now_string(),
        }
    }

    fn test_step(sequence_id: &str, order: i64) -> SequenceStep {
        SequenceStep {
            id: format!("step-{sequence_id}-{order}"),
            sequence_id: sequence_id.to_string(),
            order,
            delay_seconds: 0,
            condition: json!({ "type": "always" }),
            subject: format!("Step {order}"),
            body_text: Some("Hello".to_string()),
            body_html: None,
            template_id: None,
            created_at: now_string(),
            updated_at: now_string(),
        }
    }

    fn grant_consent(state: &MarketingState, email: &str) {
        state.consent_cache.insert(
            email.to_string(),
            ConsentCacheEntry {
                key: email.to_string(),
                contact_id: None,
                granted: true,
                purpose: state.settings_snapshot().consent_purpose,
                source: "test".to_string(),
                updated_at: now_string(),
            },
        );
    }

    #[test]
    fn advance_enrollment_bridges_order_gaps_and_enroll_starts_at_first_step() {
        let state = Arc::new(MarketingState::new());
        let bus = Arc::new(HookBus::new());
        bus.register(
            EVENT_MAIL_SEND_REQUESTED,
            "test",
            Arc::new(|ctx: &mut HookContext| {
                ctx.add_result(json!({ "job_id": "job-1" }));
            }),
        );
        state.set_hooks(bus);
        grant_consent(&state, "ana@example.com");
        // Steps 1 and 3 only: step 2 was deleted, leaving a gap.
        state
            .sequences
            .insert("seq-1".to_string(), test_sequence("seq-1"));
        let step1 = test_step("seq-1", 1);
        let step3 = test_step("seq-1", 3);
        state.steps.insert(step1.id.clone(), step1);
        state.steps.insert(step3.id.clone(), step3.clone());
        let enrollment = Enrollment {
            id: "enr-1".to_string(),
            sequence_id: "seq-1".to_string(),
            contact_id: "c-1".to_string(),
            email: "ana@example.com".to_string(),
            next_step: 2,
            status: "active".to_string(),
            next_step_at: None,
            history: Vec::new(),
            created_at: now_string(),
            updated_at: now_string(),
        };
        state
            .enrollments
            .insert(enrollment.id.clone(), enrollment);
        assert!(advance_enrollment(&state, "enr-1").unwrap());
        let updated = state
            .enrollments
            .get("enr-1")
            .map(|entry| entry.value().clone())
            .unwrap();
        // Before the fix this completed with no send for step 3:
        // the exact-order lookup found nothing at order 2.
        assert_eq!(updated.status, "completed");
        assert!(state.sends.iter().any(|entry| {
            entry.value().step_id.as_deref() == Some(step3.id.as_str())
                && entry.value().status == "queued"
        }));
        assert!(updated
            .history
            .iter()
            .any(|event| event.step_order == 3 && event.action == "sent"));
        // A fresh enrollment starts at the lowest order that
        // exists, not at a hardcoded 1.
        state
            .sequences
            .insert("seq-2".to_string(), test_sequence("seq-2"));
        state.steps.insert(
            "step-seq-2-3".to_string(),
            test_step("seq-2", 3),
        );
        state.steps.insert(
            "step-seq-2-5".to_string(),
            test_step("seq-2", 5),
        );
        let enrolled = enroll_contact(&state, "seq-2", "c-2", "bob@example.com")
            .unwrap()
            .unwrap();
        assert_eq!(enrolled.next_step, 3);
    }
}
