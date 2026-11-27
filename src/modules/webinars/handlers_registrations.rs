// webinars -- Registration handlers.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

use super::helpers::*;
use super::scheduler::*;
use super::state::*;
use super::types::*;

// Registration handlers

pub(crate) async fn register_for_webinar(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<WebinarRegistration> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let webinar = load_webinar(&state, &path.into_inner())?;
        // Hold this webinar's seat lock from here until the
        // registration row is inserted below. Without it, two
        // concurrent registrations could both observe
        // registered_count < capacity and both insert, overfilling
        // the webinar. The guard is released when this closure
        // returns; nothing awaited runs while it is held.
        let seat_lock = state.registration_lock(webinar.id);
        let _seat_guard = seat_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if webinar.status != WebinarStatus::Scheduled && webinar.status != WebinarStatus::Live {
            return Err(UnifiedError::unprocessable(
                "Registration is only open while the webinar is scheduled or live",
            ));
        }
        if is_staff(&webinar, &actor, &roles) {
            return Err(UnifiedError::conflict(
                "Hosts, co-hosts, and panelists do not register as attendees",
            ));
        }
        // An existing active registration is returned as-is, so a
        // double register is idempotent instead of an error.
        if let Some(existing) = find_registration(&state, webinar.id, &actor) {
            if matches!(
                existing.status.as_str(),
                "registered" | "waitlisted" | "pending_approval"
            ) {
                return Ok(existing);
            }
        }
        let now = Utc::now();
        let payment_status = if webinar.access_model == WebinarAccessModel::Free {
            "not_required".to_string()
        } else {
            "pending".to_string()
        };
        let status = if webinar.requires_approval {
            "pending_approval".to_string()
        } else if registered_count(&state, webinar.id) < webinar.capacity {
            "registered".to_string()
        } else if webinar.allow_waitlist {
            "waitlisted".to_string()
        } else {
            return Err(UnifiedError::conflict(
                "This webinar is full and its waitlist is closed",
            ));
        };
        let registration = WebinarRegistration {
            id: Uuid::now_v7(),
            webinar_id: webinar.id,
            user_id: actor.clone(),
            status: status.clone(),
            payment_status,
            registered_at: now,
            approved_at: None,
            cancelled_at: None,
            promoted_at: None,
            created_at: now,
            updated_at: now,
        };
        state
            .registrations
            .insert(registration.id, registration.clone());
        state.emit(
            "webinar.registration.created",
            json!({
                "webinar_id": webinar.id.to_string(),
                "registration_id": registration.id.to_string(),
                "user_id": actor,
                "status": status,
                "access_model": access_name(webinar.access_model),
                "product_id": webinar.product_id,
                "payment_status": registration.payment_status,
                "registered_at": now.to_rfc3339(),
            }),
        );
        if status == "waitlisted" {
            state.emit(
                "webinar.registration.waitlisted",
                json!({
                    "webinar_id": webinar.id.to_string(),
                    "registration_id": registration.id.to_string(),
                    "user_id": registration.user_id,
                }),
            );
        }
        // Paid registration: ask billing / store to collect payment.
        // This module never charges anything itself; the order and
        // the entitlement live with the store module, and the
        // registration flips to `paid` when `order.paid` (or
        // `store.order.completed`) answers for this product.
        if webinar.access_model == WebinarAccessModel::Paid {
            state.emit(
                "webinar.registration.payment_required",
                json!({
                    "webinar_id": webinar.id.to_string(),
                    "registration_id": registration.id.to_string(),
                    "user_id": registration.user_id,
                    "product_id": webinar.product_id,
                }),
            );
        }
        Ok(registration)
    })();
    match result {
        Ok(registration) => created(registration),
        Err(error) => err(error),
    }
}

pub(crate) async fn list_registrations(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<WebinarRegistration>> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let params = query.into_inner();
        let webinar = load_webinar(&state, &path.into_inner())?;
        if !can_manage(&webinar, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host, a co-host, or an admin can list registrations",
            ));
        }
        let status_filter = params.filters.get("status").cloned();
        let user_filter = params.filters.get("user_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<WebinarRegistration> = state
            .registrations
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                if row.webinar_id != webinar.id {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if &row.status != status {
                        return false;
                    }
                }
                if let Some(user_id) = &user_filter {
                    if &row.user_id != user_id {
                        return false;
                    }
                }
                if !search.is_empty() && !row.user_id.to_lowercase().contains(&search) {
                    return false;
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["registered_at", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "registered_at" => a.registered_at.cmp(&b.registered_at),
                _ => a.created_at.cmp(&b.created_at),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}

pub(crate) async fn my_registration(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<WebinarRegistration> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let webinar = load_webinar(&state, &path.into_inner())?;
        find_registration(&state, webinar.id, &actor).ok_or_else(|| {
            UnifiedError::not_found(format!(
                "No registration for user {actor} on webinar {}",
                webinar.id
            ))
        })
    })();
    match result {
        Ok(registration) => ok(registration),
        Err(error) => err(error),
    }
}

pub(crate) async fn approve_registration(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<WebinarRegistration> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let (webinar_id, user_id) = path.into_inner();
        let webinar = load_webinar(&state, &webinar_id)?;
        // Hold this webinar's seat lock from here until the
        // registration row is updated below. Without it, two
        // concurrent approvals could both observe
        // registered_count < capacity and both fill the last seat,
        // overfilling the webinar. The guard is released when this
        // closure returns; nothing awaited runs while it is held.
        let seat_lock = state.registration_lock(webinar.id);
        let _seat_guard = seat_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !can_manage(&webinar, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host, a co-host, or an admin can approve registrations",
            ));
        }
        let mut registration =
            find_registration(&state, webinar.id, &user_id).ok_or_else(|| {
                UnifiedError::not_found(format!("No registration for user {user_id}"))
            })?;
        if registration.status != "pending_approval" {
            return Err(UnifiedError::unprocessable(format!(
                "Only a pending registration can be approved (current status: {})",
                registration.status
            )));
        }
        let now = Utc::now();
        if registered_count(&state, webinar.id) < webinar.capacity {
            registration.status = "registered".to_string();
            registration.approved_at = Some(now);
        } else if webinar.allow_waitlist {
            // No free seat: approval still counts, but the attendee
            // joins the waitlist and is auto-promoted when a seat
            // frees up.
            registration.status = "waitlisted".to_string();
            registration.approved_at = Some(now);
        } else {
            return Err(UnifiedError::conflict(
                "This webinar is full and its waitlist is closed",
            ));
        }
        registration.updated_at = now;
        state
            .registrations
            .insert(registration.id, registration.clone());
        state.emit(
            "webinar.registration.approved",
            json!({
                "webinar_id": webinar.id.to_string(),
                "registration_id": registration.id.to_string(),
                "user_id": registration.user_id,
                "status": registration.status,
                "approved_by": actor,
            }),
        );
        state.notify(
            &registration.user_id,
            &format!("Approved: {}", webinar.title),
            "Your registration has been approved.",
            Some(format!("/api/v1/webinars/{}", webinar.id)),
        );
        Ok(registration)
    })();
    match result {
        Ok(registration) => ok(registration),
        Err(error) => err(error),
    }
}

pub(crate) async fn deny_registration(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<WebinarRegistration> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let (webinar_id, user_id) = path.into_inner();
        let webinar = load_webinar(&state, &webinar_id)?;
        if !can_manage(&webinar, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host, a co-host, or an admin can deny registrations",
            ));
        }
        let mut registration =
            find_registration(&state, webinar.id, &user_id).ok_or_else(|| {
                UnifiedError::not_found(format!("No registration for user {user_id}"))
            })?;
        if registration.status != "pending_approval" {
            return Err(UnifiedError::unprocessable(format!(
                "Only a pending registration can be denied (current status: {})",
                registration.status
            )));
        }
        registration.status = "denied".to_string();
        registration.updated_at = Utc::now();
        state
            .registrations
            .insert(registration.id, registration.clone());
        state.emit(
            "webinar.registration.denied",
            json!({
                "webinar_id": webinar.id.to_string(),
                "registration_id": registration.id.to_string(),
                "user_id": registration.user_id,
                "denied_by": actor,
            }),
        );
        state.notify(
            &registration.user_id,
            &format!("Registration denied: {}", webinar.title),
            "Your registration request was not approved.",
            Some(format!("/api/v1/webinars/{}", webinar.id)),
        );
        Ok(registration)
    })();
    match result {
        Ok(registration) => ok(registration),
        Err(error) => err(error),
    }
}

pub(crate) async fn cancel_registration(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<WebinarRegistration> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let (webinar_id, user_id) = path.into_inner();
        let webinar = load_webinar(&state, &webinar_id)?;
        let is_self = actor == user_id;
        if !is_self && !can_manage(&webinar, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "Only the registrant, the webinar host, a co-host, or an admin can cancel a registration",
            ));
        }
        let mut registration =
            find_registration(&state, webinar.id, &user_id).ok_or_else(|| {
                UnifiedError::not_found(format!("No registration for user {user_id}"))
            })?;
        if !matches!(
            registration.status.as_str(),
            "registered" | "waitlisted" | "pending_approval"
        ) {
            return Err(UnifiedError::unprocessable(format!(
                "A registration with status {} cannot be cancelled",
                registration.status
            )));
        }
        let was_registered = registration.status == "registered";
        registration.status = "cancelled".to_string();
        registration.cancelled_at = Some(Utc::now());
        registration.updated_at = Utc::now();
        state
            .registrations
            .insert(registration.id, registration.clone());
        state.emit(
            "webinar.registration.cancelled",
            json!({
                "webinar_id": webinar.id.to_string(),
                "registration_id": registration.id.to_string(),
                "user_id": registration.user_id,
                "cancelled_by": actor,
            }),
        );
        // A freed seat auto-promotes the earliest waitlisted user.
        if was_registered {
            promote_waitlist(&state, &webinar);
        }
        Ok(registration)
    })();
    match result {
        Ok(registration) => ok(registration),
        Err(error) => err(error),
    }
}

pub(crate) async fn promote_registration(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<WebinarRegistration> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let (webinar_id, user_id) = path.into_inner();
        let webinar = load_webinar(&state, &webinar_id)?;
        // Hold this webinar's seat lock from here until the
        // registration row is updated below. Without it, two
        // concurrent promotions could both observe a free seat and
        // both fill it, overfilling the webinar. The guard is
        // released when this closure returns; nothing awaited runs
        // while it is held.
        let seat_lock = state.registration_lock(webinar.id);
        let _seat_guard = seat_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !can_manage(&webinar, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host, a co-host, or an admin can promote a waitlisted registration",
            ));
        }
        let mut registration =
            find_registration(&state, webinar.id, &user_id).ok_or_else(|| {
                UnifiedError::not_found(format!("No registration for user {user_id}"))
            })?;
        if registration.status != "waitlisted" {
            return Err(UnifiedError::unprocessable(format!(
                "Only a waitlisted registration can be promoted (current status: {})",
                registration.status
            )));
        }
        if registered_count(&state, webinar.id) >= webinar.capacity {
            return Err(UnifiedError::conflict(
                "There is no free seat to promote into",
            ));
        }
        let now = Utc::now();
        registration.status = "registered".to_string();
        registration.promoted_at = Some(now);
        registration.updated_at = now;
        state
            .registrations
            .insert(registration.id, registration.clone());
        state.emit(
            "webinar.registration.promoted",
            json!({
                "webinar_id": webinar.id.to_string(),
                "registration_id": registration.id.to_string(),
                "user_id": registration.user_id,
                "promoted_by": actor,
                "promoted_at": now.to_rfc3339(),
            }),
        );
        state.notify(
            &registration.user_id,
            &format!("A seat opened up: {}", webinar.title),
            "You have been promoted from the waitlist and now hold a seat.",
            Some(format!("/api/v1/webinars/{}", webinar.id)),
        );
        Ok(registration)
    })();
    match result {
        Ok(registration) => ok(registration),
        Err(error) => err(error),
    }
}

// Tests for the per-webinar seat lock: concurrent registrations
// must not overfill a webinar.
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn test_webinar(capacity: usize) -> Webinar {
        let now = Utc::now();
        Webinar {
            id: Uuid::new_v4(),
            title: "Test webinar".to_string(),
            description: None,
            host_id: "host-1".to_string(),
            co_host_ids: Vec::new(),
            panelist_ids: Vec::new(),
            status: WebinarStatus::Scheduled,
            access_model: WebinarAccessModel::Free,
            product_id: None,
            capacity,
            requires_approval: false,
            allow_waitlist: false,
            chat_enabled: true,
            qa_enabled: true,
            polls_enabled: true,
            recording_enabled: true,
            meeting_room_id: None,
            meeting_link_status: "unlinked".to_string(),
            chat_channel_id: None,
            survey_id: None,
            survey_distribution_id: None,
            starts_at: Some(now + chrono::Duration::hours(1)),
            ends_at: None,
            timezone: "UTC".to_string(),
            recurrence_rule: None,
            scheduler_job_id: None,
            scheduler_status: "not_scheduled".to_string(),
            reminder_scheduler_status: "not_scheduled".to_string(),
            started_at: None,
            ended_at: None,
            follow_up_sent_at: None,
            follow_up_count: 0,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn registration_lock_is_stable_per_webinar() {
        let state = WebinarsState::default();
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        assert!(Arc::ptr_eq(
            &state.registration_lock(first),
            &state.registration_lock(first)
        ));
        assert!(!Arc::ptr_eq(
            &state.registration_lock(first),
            &state.registration_lock(second)
        ));
    }

    #[test]
    fn concurrent_seat_claims_cannot_overfill_capacity() {
        // Eight threads race for one seat, each running the same
        // check-and-insert the register handler runs under the
        // per-webinar lock. Without the lock, several threads could
        // observe a free seat at once; with it, exactly one wins.
        let state = WebinarsState::default();
        let webinar = test_webinar(1);
        let webinar_id = webinar.id;
        state.webinars.insert(webinar_id, webinar);
        std::thread::scope(|scope| {
            for n in 0..8 {
                let state = &state;
                scope.spawn(move || {
                    let seat_lock = state.registration_lock(webinar_id);
                    let _seat_guard = seat_lock
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    if registered_count(state, webinar_id) < 1 {
                        let now = Utc::now();
                        state.registrations.insert(
                            Uuid::new_v4(),
                            WebinarRegistration {
                                id: Uuid::new_v4(),
                                webinar_id,
                                user_id: format!("user-{n}"),
                                status: "registered".to_string(),
                                payment_status: "not_required".to_string(),
                                registered_at: now,
                                approved_at: None,
                                cancelled_at: None,
                                promoted_at: None,
                                created_at: now,
                                updated_at: now,
                            },
                        );
                    }
                });
            }
        });
        assert_eq!(registered_count(&state, webinar_id), 1);
    }

    fn test_registration_row(
        webinar_id: Uuid,
        user_id: &str,
        status: &str,
    ) -> WebinarRegistration {
        let now = Utc::now();
        WebinarRegistration {
            id: Uuid::new_v4(),
            webinar_id,
            user_id: user_id.to_string(),
            status: status.to_string(),
            payment_status: "not_required".to_string(),
            registered_at: now,
            approved_at: None,
            cancelled_at: None,
            promoted_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn concurrent_approvals_cannot_overfill_capacity() {
        // Eight threads race to approve a different pending row
        // each, running the same check-and-update the approve
        // handler runs under the per-webinar lock: with one seat,
        // exactly one approval may flip its row to registered.
        let state = WebinarsState::default();
        let webinar = test_webinar(1);
        let webinar_id = webinar.id;
        state.webinars.insert(webinar_id, webinar);
        let pending: Vec<Uuid> = (0..8)
            .map(|n| {
                let row = test_registration_row(webinar_id, &format!("user-{n}"), "pending_approval");
                let id = row.id;
                state.registrations.insert(id, row);
                id
            })
            .collect();
        std::thread::scope(|scope| {
            for id in &pending {
                let state = &state;
                let id = *id;
                scope.spawn(move || {
                    let seat_lock = state.registration_lock(webinar_id);
                    let _seat_guard = seat_lock
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    if registered_count(state, webinar_id) < 1 {
                        if let Some(mut row) = state.registrations.get_mut(&id) {
                            row.status = "registered".to_string();
                            row.approved_at = Some(Utc::now());
                        }
                    }
                });
            }
        });
        assert_eq!(registered_count(&state, webinar_id), 1);
        assert_eq!(
            state
                .registrations
                .iter()
                .filter(|entry| entry.value().status == "registered")
                .count(),
            1
        );
    }

    #[test]
    fn concurrent_promotions_cannot_overfill_capacity() {
        // Eight threads race to promote a different waitlisted row
        // each, running the same check-and-update the promote
        // handler runs under the per-webinar lock: with one seat,
        // exactly one promotion may flip its row to registered.
        let state = WebinarsState::default();
        let webinar = test_webinar(1);
        let webinar_id = webinar.id;
        state.webinars.insert(webinar_id, webinar);
        let waiting: Vec<Uuid> = (0..8)
            .map(|n| {
                let row = test_registration_row(webinar_id, &format!("user-{n}"), "waitlisted");
                let id = row.id;
                state.registrations.insert(id, row);
                id
            })
            .collect();
        std::thread::scope(|scope| {
            for id in &waiting {
                let state = &state;
                let id = *id;
                scope.spawn(move || {
                    let seat_lock = state.registration_lock(webinar_id);
                    let _seat_guard = seat_lock
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    if registered_count(state, webinar_id) < 1 {
                        if let Some(mut row) = state.registrations.get_mut(&id) {
                            row.status = "registered".to_string();
                            row.promoted_at = Some(Utc::now());
                        }
                    }
                });
            }
        });
        assert_eq!(registered_count(&state, webinar_id), 1);
        assert_eq!(
            state
                .registrations
                .iter()
                .filter(|entry| entry.value().status == "registered")
                .count(),
            1
        );
    }
}
