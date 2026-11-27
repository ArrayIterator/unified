// Elearning - Hook listeners (store / Web3 events; defensive reads only).

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::ai;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use rand::{distr::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;


// Module notes moved here from the original file header
// (kept verbatim so no documentation is lost):

// HONESTY - READ THIS FIRST. What is real in this module:
// in-memory course / module / lesson / enrollment / progress /
// quiz / attempt / certificate / review / learning-path
// management; free and paid enrollment (paid enrollment never
// calls the store directly - it emits a purchase event and waits
// for `order.paid`); drip content by date and by previous-lesson
// completion; multiple-choice quizzes with a per-quiz pass mark,
// attempt limits, auto-grading, attempt history, and answer
// review only when the quiz allows it; per-lesson completion and
// course progress percentages; course completion when every
// required lesson is done; certificates issued on completion with
// a verifiable code, a public verification endpoint, and a
// SHA-256 content fingerprint; learning paths with ordered
// courses and per-user path progress; instructor ownership
// (instructors manage their own courses). What is NOT built here:
// durable storage (the DashMap stores are authoritative;
// migration `2026-11-22-000076_elearning` is the persistence
// target); payment capture itself (the store/billing modules own
// it - this module only requests a purchase and reacts to the
// paid/refunded events); media storage or playback (lesson video
// and file content are storage references only - the storage
// module owns the bytes); on-chain anchoring itself (the Web3
// module owns it - this module only emits `web3.anchor.requested`
// and listens for the confirmed/failed outcome); and email/push
// delivery (the notifications module owns delivery - this module
// only emits `notification.requested`).
//
// CONFIG-DRIVEN - READ THIS FIRST. Nothing behavioural is
// hard-coded: the allowed course levels, categories, languages,
// and lesson types, the default pass mark, the default attempt
// limit, the default currency, the certificate issuer name, and
// the anchoring switch all live in `ElearningSettings`, editable
// by an administrator through `PUT /settings`. The values in
// `ElearningSettings::default` are CONFIGURABLE DEFAULTS only -
// an admin change applies to every validation and default that
// reads them, and no code path consults a baked-in list instead.
// Lesson types, levels, categories, and languages are validated
// against the live settings, never against a constant list.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the events module. Capability checks use
// the `role.check` hook and fail closed (no answer means not allowed).
// The certificate verification endpoint is the single exception:
// it is intentionally public (no headers, no capability check),
// because anyone holding a certificate code must be able to
// verify it without an account.
//
// Events emitted on the hook bus:
//   - `elearning.course.created`, `elearning.course.updated`,
//     `elearning.course.deleted`, `elearning.course.published`,
//     `elearning.course.archived`
//   - `elearning.module.created`, `elearning.module.updated`,
//     `elearning.module.deleted`
//   - `elearning.lesson.created`, `elearning.lesson.updated`,
//     `elearning.lesson.deleted`, `elearning.lesson.completed`
//   - `elearning.enrollment.created`,
//     `elearning.enrollment.purchase_requested` (paid enrollment:
//     `{"course_id", "user_id", "amount_minor", "currency",
//     "module": "elearning", "item_type": "course_enrollment",
//     "enrollment_id"}` - no store listener creates an order
//     from this request yet: a course is not a store product, so
//     the buyer checks out with the course id on the checkout
//     item and the paid order activates the enrollment),
//     `elearning.enrollment.activated`,
//     `elearning.enrollment.cancelled`,
//     `elearning.enrollment.suspended`,
//     `elearning.enrollment.completed`
//   - `elearning.quiz.created`, `elearning.quiz.updated`,
//     `elearning.quiz.question.created`,
//     `elearning.quiz.question.updated`,
//     `elearning.quiz.question.deleted`,
//     `elearning.quiz.attempt.submitted` (graded result, no
//     correct answers inside), `elearning.quiz.attempt.passed`,
//     `elearning.quiz.attempt.failed`
//   - `elearning.certificate.issued`
//     (`{"certificate_id", "code", "course_id", "user_id",
//     "content_sha256"}`), `elearning.certificate.revoked`,
//     `elearning.certificate.anchor.requested`
//   - `elearning.review.created`, `elearning.review.updated`,
//     `elearning.review.deleted`
//   - `elearning.path.created`, `elearning.path.updated`,
//     `elearning.path.deleted`, `elearning.path.course.added`,
//     `elearning.path.course.removed`,
//     `elearning.path.completed`
//   - `web3.anchor.requested` (certificate anchoring, only when
//     anchoring is enabled in settings:
//     `{"record_type": "elearning.certificate", "record_id",
//     "content_sha256", "network_id"}` - the Web3 module owns the
//     anchor itself)
//   - `notification.requested` (enrollment activated, course
//     completed, certificate issued, in the notifications
//     module's payload shape).
// Events this module listens to (all read defensively - a missing
// field or an unknown course is skipped, never an error):
//   - `elearning.course.requested` - answers `{exists, course_id,
//     title, status}` for the academic module's course links.
//   - `order.paid`, `store.order.completed` - a paid order whose
//     payload names an e-learning course activates the matching
//     pending enrollment (`course_id` plus `module` =
//     `"elearning"`, or an `items` entry carrying `course_id`;
//     serialized store items carry `module_id`, which is accepted
//     too).
//   - `store.order.refunded`, `store.order.cancelled` - suspends
//     the matching active enrollment.
//   - `web3.anchor.confirmed`, `web3.anchor.failed` - updates the
//     anchor status of the named certificate (`record_id`).

// Hook listeners (store / Web3 events; defensive reads only)

/// Extract the e-learning course id from a store order payload:
/// either a top-level `course_id` with `module` = `"elearning"`,
/// or an `items` entry carrying `course_id`. Both spellings of
/// the module marker are accepted: serialized store order items
/// carry `module_id`, never `module`, so matching on `module`
/// alone would never see a real store order.
pub(crate) fn order_course_id(data: &Value) -> Option<(Uuid, Option<String>)> {
    let module = opt_string(data, "module").or_else(|| opt_string(data, "module_id"));
    if module.as_deref() == Some("elearning") {
        if let Some(raw) = opt_string(data, "course_id") {
            if let Ok(id) = Uuid::parse_str(&raw) {
                return Some((id, opt_string(data, "order_id").or_else(|| opt_string(data, "id"))));
            }
        }
    }
    if let Some(items) = data.get("items").and_then(|value| value.as_array()) {
        for item in items {
            let item_module =
                opt_string(item, "module").or_else(|| opt_string(item, "module_id"));
            if item_module.as_deref() == Some("elearning") {
                if let Some(raw) = opt_string(item, "course_id") {
                    if let Ok(id) = Uuid::parse_str(&raw) {
                        return Some((id, opt_string(data, "order_id").or_else(|| opt_string(data, "id"))));
                    }
                }
            }
        }
    }
    None
}

/// Answer `elearning.course.requested` (`{course_id}` - the
/// academic module asks whether a course exists before linking a
/// subject to it): add `{exists, course_id, title, status}` for a
/// known course, or `{exists: false, course_id}` when the id is
/// missing, malformed, or unknown, so the caller can record the
/// link unverified instead of waiting on a silent no-answer.
pub(crate) fn on_course_requested(state: ElearningState, ctx: &mut HookContext) {
    let course_id = ctx
        .get("course_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    match load_course(&state, &course_id) {
        Ok(course) => {
            ctx.add_result(json!({
                "exists": true,
                "course_id": course.id.to_string(),
                "title": course.title,
                "status": course.status,
            }));
        }
        Err(_) => {
            ctx.add_result(json!({
                "exists": false,
                "course_id": course_id,
            }));
        }
    }
}

/// Activate the pending enrollment that matches a paid order.
/// The buyer is read from `user_id` (falling back to
/// `customer_id`); a missing buyer or course id is skipped.
pub(crate) fn on_order_paid(state: ElearningState, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some((course_id, order_id)) = order_course_id(&data) else {
        ctx.add_result(json!({ "elearning": "skipped", "reason": "order event carries no e-learning course_id" }));
        return;
    };
    let Some(user_id) = opt_string(&data, "user_id").or_else(|| opt_string(&data, "customer_id")) else {
        ctx.add_result(json!({ "elearning": "skipped", "reason": "order event carries no user_id or customer_id" }));
        return;
    };
    let Some(mut enrollment) = state
        .enrollments
        .iter_mut()
        .find(|entry| {
            entry.value().course_id == course_id
                && entry.value().user_id == user_id
                && entry.value().status == ENROLL_PENDING_PAYMENT
        })
    else {
        ctx.add_result(json!({ "elearning": "skipped", "reason": "no pending enrollment matches this order" }));
        return;
    };
    enrollment.status = ENROLL_ACTIVE.to_string();
    if let Some(order) = order_id {
        enrollment.order_id = Some(order);
    }
    enrollment.updated_at = Utc::now();
    let updated = enrollment.clone();
    drop(enrollment);
    state.emit(
        "elearning.enrollment.activated",
        json!({
            "enrollment_id": updated.id.to_string(),
            "course_id": updated.course_id.to_string(),
            "user_id": updated.user_id,
        }),
    );
    state.notify(
        "elearning.enrollment.activated",
        &updated.user_id,
        "Enrollment activated",
        "Your paid enrollment is now active.",
        Some(format!("/api/v1/elearning/courses/{}/progress", updated.course_id)),
    );
    ctx.add_result(json!({ "elearning": "activated", "enrollment_id": updated.id.to_string() }));
}

/// Suspend the active enrollment that matches a refunded or
/// cancelled order.
pub(crate) fn on_order_refunded_or_cancelled(state: ElearningState, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some((course_id, _)) = order_course_id(&data) else {
        ctx.add_result(json!({ "elearning": "skipped", "reason": "order event carries no e-learning course_id" }));
        return;
    };
    let Some(user_id) = opt_string(&data, "user_id").or_else(|| opt_string(&data, "customer_id")) else {
        ctx.add_result(json!({ "elearning": "skipped", "reason": "order event carries no user_id or customer_id" }));
        return;
    };
    let Some(mut enrollment) = state.enrollments.iter_mut().find(|entry| {
        entry.value().course_id == course_id
            && entry.value().user_id == user_id
            && entry.value().status == ENROLL_ACTIVE
    }) else {
        ctx.add_result(json!({ "elearning": "skipped", "reason": "no active enrollment matches this order" }));
        return;
    };
    enrollment.status = ENROLL_SUSPENDED.to_string();
    enrollment.updated_at = Utc::now();
    let updated = enrollment.clone();
    drop(enrollment);
    state.emit(
        "elearning.enrollment.suspended",
        json!({
            "enrollment_id": updated.id.to_string(),
            "course_id": updated.course_id.to_string(),
            "user_id": updated.user_id,
        }),
    );
    ctx.add_result(json!({ "elearning": "suspended", "enrollment_id": updated.id.to_string() }));
}

/// Record the anchoring outcome for the named certificate
/// (`record_id` is the certificate id).
pub(crate) fn on_anchor_outcome(state: ElearningState, ctx: &mut HookContext, confirmed: bool) {
    let data = ctx.data.clone();
    let Some(raw) = opt_string(&data, "record_id") else {
        ctx.add_result(json!({ "elearning": "skipped", "reason": "anchor event carries no record_id" }));
        return;
    };
    let Ok(id) = Uuid::parse_str(&raw) else {
        ctx.add_result(json!({ "elearning": "skipped", "reason": "anchor record_id is not a UUID" }));
        return;
    };
    let Some(mut certificate) = state.certificates.get_mut(&id) else {
        ctx.add_result(json!({ "elearning": "skipped", "reason": "no certificate matches this anchor record" }));
        return;
    };
    certificate.anchor_status = if confirmed { "confirmed".to_string() } else { "failed".to_string() };
    if let Some(tx) = opt_string(&data, "tx_hash") {
        certificate.anchor_tx = Some(tx);
    }
    certificate.updated_at = Utc::now();
    ctx.add_result(json!({ "elearning": "anchor_updated", "certificate_id": id.to_string(), "anchor_status": certificate.anchor_status }));
}
