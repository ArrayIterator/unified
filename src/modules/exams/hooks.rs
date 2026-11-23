//! exams - hooks.

use unified_core::hooks::HookContext;
use chrono::Utc;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use super::{constants::*, settings::*, types::*, module::*, helpers::*, inputs::*, handlers_banks::*, handlers_questions::*, handlers_exams::*, handlers_attempts::*, handlers_grading::*, handlers_results::*, handlers_ai::*, handlers_settings::*};



// ---------------------------------------------------------------------------
// Hook handlers
// ---------------------------------------------------------------------------
pub(crate) fn on_ai_draft_grounding(state: ExamsStateRef, ctx: &mut HookContext) {
    let topic = ctx
        .get("topic")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let mut by_category: HashMap<String, u64> = HashMap::new();
    let mut by_difficulty: HashMap<String, u64> = HashMap::new();
    let mut by_type: HashMap<String, u64> = HashMap::new();
    let mut total = 0u64;
    for entry in state.questions.iter() {
        let question = entry.value();
        if question.superseded_by.is_some() {
            continue;
        }
        if !topic.is_empty()
            && !question.text.to_lowercase().contains(&topic.to_lowercase())
            && !question.category.to_lowercase().contains(&topic.to_lowercase())
        {
            continue;
        }
        total += 1;
        *by_category.entry(question.category.clone()).or_insert(0) += 1;
        *by_difficulty
            .entry(question.difficulty.clone())
            .or_insert(0) += 1;
        *by_type
            .entry(question.question_type.clone())
            .or_insert(0) += 1;
    }
    ctx.add_result(json!({
        "exams": "draft_questions_grounding",
        "topic": topic,
        "existing_question_count": total,
        "by_category": by_category,
        "by_difficulty": by_difficulty,
        "by_question_type": by_type,
    }));
}


pub(crate) fn on_ai_analyse_grounding(state: ExamsStateRef, ctx: &mut HookContext) {
    let exam_id = match ctx.get("exam_id").and_then(|v| v.as_str()) {
        Some(id) => id.to_string(),
        None => {
            ctx.add_result(json!({
                "exams": "skipped",
                "reason": "analyse request carries no exam_id",
            }));
            return;
        }
    };
    if !state.exams.contains_key(&exam_id) {
        ctx.add_result(json!({
            "exams": "skipped",
            "reason": "no exam matches this exam_id",
        }));
        return;
    }
    // Anonymised per-question statistics only - never raw student
    // answers or student identities.
    let stats = question_statistics(&state, &exam_id);
    ctx.add_result(json!({
        "exams": "analyse_results_grounding",
        "exam_id": exam_id,
        "statistics": stats,
    }));
}


pub(crate) fn on_course_deleted(state: ExamsStateRef, ctx: &mut HookContext) {
    let course_id = match ctx.get("course_id").and_then(|v| v.as_str()) {
        Some(id) => id.to_string(),
        None => {
            ctx.add_result(json!({
                "exams": "skipped",
                "reason": "course event carries no course_id",
            }));
            return;
        }
    };
    let mut affected = 0usize;
    for entry in state.exams.iter() {
        let exam = entry.value();
        if exam.course_id.as_deref() == Some(course_id.as_str()) {
            affected += 1;
        }
    }
    ctx.add_result(json!({
        "exams": "course_deleted_noted",
        "course_id": course_id,
        "linked_exams": affected,
        "note": "Linked exams keep their link; the course itself is never deleted here",
    }));
}


/// Extract every exams-module exam id from a store order payload:
/// a top-level `exam_id` with `module`/`module_id` = `"exams"`, or
/// `items` entries carrying `exam_id` with the same marker (on the
/// item itself or on the order). Mirrors the e-learning module's
/// `order_course_id`, but collects all exams: one order may buy
/// several certification exams at once. Both spellings of the
/// module marker are accepted: serialized store order items carry
/// `module_id`, never `module`.
pub(crate) fn order_exam_ids(data: &Value) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    let mut push = |raw: Option<String>, ids: &mut Vec<String>| {
        if let Some(id) = raw {
            if !id.trim().is_empty() && !ids.contains(&id) {
                ids.push(id);
            }
        }
    };
    let order_module =
        opt_string(data, "module").or_else(|| opt_string(data, "module_id"));
    let order_is_exams = order_module.as_deref() == Some("exams");
    if order_is_exams {
        push(opt_string(data, "exam_id"), &mut ids);
    }
    if let Some(items) = data.get("items").and_then(|value| value.as_array()) {
        for item in items {
            let item_module =
                opt_string(item, "module").or_else(|| opt_string(item, "module_id"));
            if item_module.as_deref() == Some("exams") || order_is_exams {
                push(opt_string(item, "exam_id"), &mut ids);
            }
        }
    }
    ids
}


/// Answer `order.paid` / `store.order.completed`: a paid order for
/// a paid exam activates that exam for the buyer. The buyer is read
/// from `user_id` (falling back to `customer_id`); a missing buyer
/// or exam id is skipped. Free exams need no activation and are
/// skipped silently. Activation is idempotent: a re-delivered order
/// event (both order events fire for one payment) returns the
/// existing activation instead of creating a duplicate, and the
/// `exams.activation.created` event is emitted only when a new
/// activation was actually created, so downstream consumers that
/// count the event (audit, notifications, billing reconciliation)
/// never double-count a re-delivery.
pub(crate) fn on_order_paid(state: ExamsStateRef, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let exam_ids = order_exam_ids(&data);
    if exam_ids.is_empty() {
        ctx.add_result(json!({ "exams": "skipped", "reason": "order event carries no exams exam_id" }));
        return;
    }
    let Some(user_id) =
        opt_string(&data, "user_id").or_else(|| opt_string(&data, "customer_id"))
    else {
        ctx.add_result(json!({ "exams": "skipped", "reason": "order event carries no user_id or customer_id" }));
        return;
    };
    let order_id = opt_string(&data, "order_id").or_else(|| opt_string(&data, "id"));
    let mut activated: Vec<String> = Vec::new();
    let mut unknown: Vec<String> = Vec::new();
    for exam_id in exam_ids {
        let Some(exam) = state
            .exams
            .get(&exam_id)
            .map(|entry| entry.value().clone())
        else {
            unknown.push(exam_id);
            continue;
        };
        if !exam.is_paid {
            continue;
        }
        let (activation, is_new) = state.activate_exam(&exam.id, &user_id, order_id.clone());
        if is_new {
            state.emit(
                EVT_EXAM_ACTIVATION_CREATED,
                json!({
                    "activation_id": activation.id,
                    "exam_id": exam.id,
                    "user_id": user_id,
                    "order_id": order_id,
                }),
            );
        }
        activated.push(exam.id);
    }
    ctx.add_result(json!({
        "exams": "order_paid",
        "user_id": user_id,
        "order_id": order_id,
        "activated_exam_ids": activated,
        "unknown_exam_ids": unknown,
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_exam(id: &str, price_minor: i64, is_paid: bool) -> Exam {
        Exam {
            id: id.to_string(),
            title: "Certification".to_string(),
            description: String::new(),
            category: "general".to_string(),
            bank_ids: Vec::new(),
            course_id: None,
            academic_enrollment_id: None,
            academic_component_id: None,
            status: "published".to_string(),
            opens_at: None,
            closes_at: None,
            duration_minutes: 60,
            pass_mark: 60.0,
            attempts_allowed: 3,
            price_minor,
            is_paid,
            settings: ExamSettings::default(),
            total_marks: 100.0,
            created_by: "admin".to_string(),
            company_id: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn result_tag(ctx: &HookContext) -> Option<&str> {
        ctx.results
            .first()
            .and_then(|value| value.get("exams"))
            .and_then(|value| value.as_str())
    }

    #[test]
    fn order_exam_ids_reads_top_level_exam() {
        let data = serde_json::json!({
            "module_id": "exams",
            "exam_id": "exam-1",
            "user_id": "user-1"
        });
        assert_eq!(order_exam_ids(&data), vec!["exam-1".to_string()]);
    }

    #[test]
    fn order_exam_ids_reads_items_and_dedupes() {
        let data = serde_json::json!({
            "user_id": "user-1",
            "items": [
                { "exam_id": "exam-1", "module_id": "exams" },
                { "exam_id": "exam-2", "module_id": "exams" },
                { "exam_id": "exam-1", "module_id": "exams" }
            ]
        });
        assert_eq!(
            order_exam_ids(&data),
            vec!["exam-1".to_string(), "exam-2".to_string()]
        );
    }

    #[test]
    fn order_exam_ids_ignores_other_modules_and_unmarked_exams() {
        let data = serde_json::json!({
            "exam_id": "exam-9",
            "items": [
                { "exam_id": "exam-1", "module_id": "elearning" },
                { "course_id": "course-1", "module_id": "elearning" },
                { "exam_id": "exam-2" }
            ]
        });
        assert!(order_exam_ids(&data).is_empty());
    }

    #[test]
    fn on_order_paid_creates_activation_for_paid_exam() {
        let state: ExamsStateRef = Arc::new(ExamsState::default());
        state
            .exams
            .insert("exam-1".to_string(), test_exam("exam-1", 50_000, true));
        let data = serde_json::json!({
            "user_id": "user-1",
            "order_id": "order-1",
            "items": [{ "exam_id": "exam-1", "module_id": "exams" }]
        });
        let mut ctx = HookContext::new("order.paid", data);
        on_order_paid(state.clone(), &mut ctx);
        assert_eq!(result_tag(&ctx), Some("order_paid"));
        assert!(state.has_active_activation("exam-1", "user-1"));
        assert_eq!(state.activations.len(), 1);
    }

    #[test]
    fn on_order_paid_skips_free_exam() {
        let state: ExamsStateRef = Arc::new(ExamsState::default());
        state
            .exams
            .insert("exam-1".to_string(), test_exam("exam-1", 0, false));
        let data = serde_json::json!({
            "user_id": "user-1",
            "order_id": "order-1",
            "items": [{ "exam_id": "exam-1", "module_id": "exams" }]
        });
        let mut ctx = HookContext::new("order.paid", data);
        on_order_paid(state.clone(), &mut ctx);
        assert_eq!(result_tag(&ctx), Some("order_paid"));
        assert!(state.activations.is_empty());
    }

    #[test]
    fn on_order_paid_is_idempotent_on_redelivery() {
        let state: ExamsStateRef = Arc::new(ExamsState::default());
        state
            .exams
            .insert("exam-1".to_string(), test_exam("exam-1", 50_000, true));
        let data = serde_json::json!({
            "user_id": "user-1",
            "order_id": "order-1",
            "items": [{ "exam_id": "exam-1", "module_id": "exams" }]
        });
        let mut first = HookContext::new("order.paid", data.clone());
        on_order_paid(state.clone(), &mut first);
        let mut second = HookContext::new("store.order.completed", data);
        on_order_paid(state.clone(), &mut second);
        assert_eq!(state.activations.len(), 1);
    }

    #[test]
    fn on_order_paid_emits_activation_event_only_once() {
        use unified_core::hooks::HookBus;
        use std::sync::Mutex;
        let state: ExamsStateRef = Arc::new(ExamsState::default());
        state
            .exams
            .insert("exam-1".to_string(), test_exam("exam-1", 50_000, true));
        let bus = Arc::new(HookBus::new());
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_writer = seen.clone();
        bus.register(
            EVT_EXAM_ACTIVATION_CREATED,
            "test",
            Arc::new(move |ctx: &mut HookContext| {
                seen_writer
                    .lock()
                    .expect("seen lock poisoned")
                    .push(ctx.event.clone());
            }),
        );
        *state.hooks.write().expect("hooks lock poisoned") = Some(bus);
        let data = serde_json::json!({
            "user_id": "user-1",
            "order_id": "order-1",
            "items": [{ "exam_id": "exam-1", "module_id": "exams" }]
        });
        let mut first = HookContext::new("order.paid", data.clone());
        on_order_paid(state.clone(), &mut first);
        let mut second = HookContext::new("store.order.completed", data);
        on_order_paid(state.clone(), &mut second);
        assert_eq!(state.activations.len(), 1);
        let seen = seen.lock().expect("seen lock poisoned");
        assert_eq!(
            seen.len(),
            1,
            "a re-delivered order event must not re-emit exams.activation.created"
        );
        assert_eq!(seen[0], EVT_EXAM_ACTIVATION_CREATED);
    }

    #[test]
    fn on_order_paid_skips_unknown_exam_and_missing_user() {
        let state: ExamsStateRef = Arc::new(ExamsState::default());
        let data = serde_json::json!({
            "user_id": "user-1",
            "order_id": "order-1",
            "items": [{ "exam_id": "exam-unknown", "module_id": "exams" }]
        });
        let mut ctx = HookContext::new("order.paid", data);
        on_order_paid(state.clone(), &mut ctx);
        assert_eq!(result_tag(&ctx), Some("order_paid"));
        assert!(state.activations.is_empty());
        assert_eq!(
            ctx.results[0]
                .get("unknown_exam_ids")
                .and_then(|value| value.as_array())
                .map(|items| items.len()),
            Some(1)
        );

        let no_user = serde_json::json!({
            "order_id": "order-1",
            "items": [{ "exam_id": "exam-1", "module_id": "exams" }]
        });
        let mut ctx = HookContext::new("order.paid", no_user);
        on_order_paid(state.clone(), &mut ctx);
        assert_eq!(result_tag(&ctx), Some("skipped"));
    }
}
