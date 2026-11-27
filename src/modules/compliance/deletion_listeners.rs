//! Compliance Center module - GDPR / ISO support: data export - Deletion listeners.
//!
//! Split from `src/modules/compliance.rs`; behaviour unchanged.

use super::breaches::*;
use super::checklists::*;
use super::consents::*;
use super::constants::*;
use super::deletions::*;
use super::exports::*;
use super::helpers::*;
use super::module::*;
use super::records::*;
use super::requests::*;
use super::retention::*;
use super::state::*;
use super::types::*;

use unified_core::hooks::HookContext;
use unified_core::json;
use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

/// `compliance.deletion.item.executed` carries `item_id` (and
/// optionally `detail`). An unknown item id is ignored so a stale
/// confirmation can never mark the wrong row.
///
/// Note: unlike the manual mark-executed endpoint, this listener
/// deliberately does not require the request to be `executing`.
/// Owning modules may confirm synchronously while
/// `execute_deletion` is still dispatching (the request flips to
/// `executing` only after the dispatch loop), so gating here would
/// reject legitimate confirmations.
pub(super) fn on_deletion_item_executed(state: ComplianceState, ctx: &mut HookContext) {
    let Some(raw_id) = ctx.get("item_id").and_then(|value| value.as_str()) else {
        ctx.add_result(json!({ "compliance": "skipped", "reason": "compliance.deletion.item.executed carries no item_id" }));
        return;
    };
    let Ok(id) = Uuid::parse_str(raw_id) else {
        ctx.add_result(json!({ "compliance": "skipped", "reason": "Invalid item id" }));
        return;
    };
    let Some(item) = state
        .deletion_items
        .get(&id)
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({ "compliance": "skipped", "item_id": raw_id, "reason": "deletion item not found" }));
        return;
    };
    if item.status == "executed" {
        ctx.add_result(json!({ "compliance": "already_executed", "item_id": raw_id }));
        return;
    }
    let detail = ctx
        .get("detail")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let updated = confirm_item_executed(&state, &item, detail);
    ctx.add_result(json!({
        "compliance": "item_executed",
        "item_id": updated.id.to_string(),
        "deletion_id": updated.deletion_request_id.to_string(),
    }));
}

/// `compliance.retention.review_requested` carries `review_id`
/// (and optionally `items_flagged` / `notes`). Records the review
/// run as completed; an unknown review id is ignored.
///
/// This event is reserved for the scheduler's dispatch of a queued
/// job, so the listener only completes a review the scheduler
/// actually queued (`scheduler_status == "queued"`) and that has
/// not been completed already (`reviewed_at` is `None`). A review
/// the scheduler declined - or one that already ran - is skipped:
/// completing it here would record a review that never happened
/// and disagree with the creation response.
pub(super) fn on_retention_review_requested(state: ComplianceState, ctx: &mut HookContext) {
    let Some(raw_id) = ctx.get("review_id").and_then(|value| value.as_str()) else {
        ctx.add_result(json!({ "compliance": "skipped", "reason": "compliance.retention.review_requested carries no review_id" }));
        return;
    };
    let Ok(id) = Uuid::parse_str(raw_id) else {
        ctx.add_result(json!({ "compliance": "skipped", "reason": "Invalid review id" }));
        return;
    };
    let Some(mut review) = state
        .retention_reviews
        .get(&id)
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({ "compliance": "skipped", "review_id": raw_id, "reason": "retention review not found" }));
        return;
    };
    if review.scheduler_status != "queued" {
        ctx.add_result(json!({ "compliance": "skipped", "review_id": raw_id, "reason": "retention review was not queued by the scheduler" }));
        return;
    }
    if review.reviewed_at.is_some() {
        ctx.add_result(json!({ "compliance": "already_completed", "review_id": raw_id }));
        return;
    }
    if let Some(flagged) = ctx.get("items_flagged").and_then(|value| value.as_i64()) {
        if flagged >= 0 {
            review.items_flagged = flagged;
        }
    }
    if let Some(notes) = ctx.get("notes").and_then(|value| value.as_str()) {
        review.notes = Some(notes.to_string());
    }
    review.scheduler_status = "queued".to_string();
    review.reviewed_at = Some(Utc::now());
    review.updated_at = Utc::now();
    state.retention_reviews.insert(review.id, review.clone());
    state.emit(
        "compliance.retention.review.completed",
        json!({
            "review_id": review.id.to_string(),
            "policy_id": review.policy_id.to_string(),
            "items_flagged": review.items_flagged,
        }),
    );
    ctx.add_result(json!({
        "compliance": "review_completed",
        "review_id": review.id.to_string(),
        "items_flagged": review.items_flagged,
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use unified_core::hooks::HookBus;
    use chrono::{DateTime, Duration};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    fn insert_review(
        state: &ComplianceState,
        scheduler_status: &str,
        reviewed_at: Option<DateTime<Utc>>,
    ) -> RetentionReview {
        let now = Utc::now();
        let review = RetentionReview {
            id: Uuid::new_v4(),
            policy_id: Uuid::new_v4(),
            items_flagged: 0,
            notes: None,
            scheduler_status: scheduler_status.to_string(),
            scheduler_job_id: None,
            reviewed_at,
            created_at: now,
            updated_at: now,
        };
        state.retention_reviews.insert(review.id, review.clone());
        review
    }

    fn stored_review(state: &ComplianceState, id: Uuid) -> RetentionReview {
        state
            .retention_reviews
            .get(&id)
            .expect("review row")
            .value()
            .clone()
    }

    #[test]
    fn declined_review_is_not_completed_by_dispatch() {
        // P1: a review the scheduler declined must never be marked
        // complete, even if the dispatch event arrives for it.
        let state = ComplianceState::default();
        let review = insert_review(&state, "declined", None);
        let mut ctx = HookContext::new(
            RETENTION_REVIEW_REQUESTED,
            json!({ "review_id": review.id.to_string(), "items_flagged": 5 }),
        );
        on_retention_review_requested(state.clone(), &mut ctx);
        let stored = stored_review(&state, review.id);
        assert_eq!(stored.scheduler_status, "declined");
        assert_eq!(stored.items_flagged, 0);
        assert_eq!(stored.reviewed_at, None);
        assert_eq!(ctx.results.len(), 1);
        assert_eq!(ctx.results[0]["compliance"].as_str(), Some("skipped"));
    }

    #[test]
    fn queued_review_is_completed_by_dispatch() {
        // P1/P5: a queued review completes on dispatch, and only
        // then gains a reviewed_at timestamp.
        let state = ComplianceState::default();
        let review = insert_review(&state, "queued", None);
        let mut ctx = HookContext::new(
            RETENTION_REVIEW_REQUESTED,
            json!({ "review_id": review.id.to_string(), "items_flagged": 7, "notes": "3 stale" }),
        );
        on_retention_review_requested(state.clone(), &mut ctx);
        let stored = stored_review(&state, review.id);
        assert_eq!(stored.scheduler_status, "queued");
        assert_eq!(stored.items_flagged, 7);
        assert_eq!(stored.notes.as_deref(), Some("3 stale"));
        assert!(stored.reviewed_at.is_some());
        assert_eq!(ctx.results.len(), 1);
        assert_eq!(
            ctx.results[0]["compliance"].as_str(),
            Some("review_completed")
        );
    }

    #[test]
    fn already_completed_review_is_not_completed_twice() {
        let state = ComplianceState::default();
        let completed_at = Utc::now() - Duration::hours(1);
        let review = insert_review(&state, "queued", Some(completed_at));
        let mut ctx = HookContext::new(
            RETENTION_REVIEW_REQUESTED,
            json!({ "review_id": review.id.to_string(), "items_flagged": 99 }),
        );
        on_retention_review_requested(state.clone(), &mut ctx);
        let stored = stored_review(&state, review.id);
        assert_eq!(stored.items_flagged, 0);
        assert_eq!(stored.reviewed_at, Some(completed_at));
        assert_eq!(ctx.results.len(), 1);
        assert_eq!(
            ctx.results[0]["compliance"].as_str(),
            Some("already_completed")
        );
    }

    #[test]
    fn item_confirmation_is_not_reemitted_to_the_bus() {
        // P3: confirming an item must not re-emit
        // `compliance.deletion.item.executed`, or the confirmation
        // would re-enter this module's own listener.
        let bus = Arc::new(HookBus::new());
        let state = ComplianceState::default();
        *state.hooks.write().expect("hooks lock poisoned") = Some(Arc::clone(&bus));

        // The module's own listener, as registered in `register_hooks`.
        let listener_state = state.clone();
        bus.register(
            DELETION_ITEM_EXECUTED,
            "compliance",
            Arc::new(move |ctx: &mut HookContext| {
                on_deletion_item_executed(listener_state.clone(), ctx)
            }),
        );
        // A counter standing in for any other subscriber.
        let deliveries = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&deliveries);
        bus.register(
            DELETION_ITEM_EXECUTED,
            "test-counter",
            Arc::new(move |_ctx: &mut HookContext| {
                counted.fetch_add(1, Ordering::SeqCst);
            }),
        );

        let now = Utc::now();
        let deletion = DeletionRequest {
            id: Uuid::new_v4(),
            requester_id: "user-1".to_string(),
            reason: "leave".to_string(),
            status: DeletionStatus::Executing,
            approved_by: Some("manager-9".to_string()),
            approved_at: Some(now),
            decided_by: Some("manager-9".to_string()),
            decision_reason: Some("approved".to_string()),
            executed_at: None,
            created_at: now,
            updated_at: now,
        };
        state.deletions.insert(deletion.id, deletion.clone());
        let item = DeletionItem {
            id: Uuid::new_v4(),
            deletion_request_id: deletion.id,
            module_id: "community".to_string(),
            subject: "all posts".to_string(),
            status: "dispatched".to_string(),
            detail: None,
            executed_at: None,
            created_at: now,
            updated_at: now,
        };
        state.deletion_items.insert(item.id, item.clone());

        // One confirmation from the owning module.
        bus.emit(
            DELETION_ITEM_EXECUTED,
            json!({ "item_id": item.id.to_string(), "detail": "3 posts" }),
        );

        let stored_item = state
            .deletion_items
            .get(&item.id)
            .expect("item row")
            .value()
            .clone();
        assert_eq!(stored_item.status, "executed");
        assert_eq!(stored_item.detail.as_deref(), Some("3 posts"));
        let stored_request = state
            .deletions
            .get(&deletion.id)
            .expect("request row")
            .value()
            .clone();
        assert_eq!(stored_request.status, DeletionStatus::Executed);
        // Exactly one delivery: the owning module's own emit. A
        // re-emit from confirm would have delivered a second time.
        assert_eq!(deliveries.load(Ordering::SeqCst), 1);
    }
}
