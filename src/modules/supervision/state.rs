// supervision module -- In-memory state and small access helpers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse};
use dashmap::DashMap;
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for supervision. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers tolerate a
/// missing bus by skipping event emission. Durable rows for these
/// types are defined by migration
/// `2026-11-22-000062_supervision`; until that migration is
/// executed, supervision data does not survive a restart.
#[derive(Clone)]
pub struct SupervisionState {
    pub assignments: Arc<DashMap<Uuid, SupervisionAssignment>>,
    pub templates: Arc<DashMap<Uuid, InspectionTemplate>>,
    pub inspections: Arc<DashMap<Uuid, Inspection>>,
    pub inspection_items: Arc<DashMap<Uuid, InspectionItemResult>>,
    pub progress_reports: Arc<DashMap<Uuid, ProgressReport>>,
    pub defects: Arc<DashMap<Uuid, Defect>>,
    pub approvals: Arc<DashMap<Uuid, Approval>>,
    pub daily_logs: Arc<DashMap<Uuid, DailyLog>>,
    pub photos: Arc<DashMap<Uuid, Photo>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for SupervisionState {
    fn default() -> Self {
        Self {
            assignments: Arc::new(DashMap::new()),
            templates: Arc::new(DashMap::new()),
            inspections: Arc::new(DashMap::new()),
            inspection_items: Arc::new(DashMap::new()),
            progress_reports: Arc::new(DashMap::new()),
            defects: Arc::new(DashMap::new()),
            approvals: Arc::new(DashMap::new()),
            daily_logs: Arc::new(DashMap::new()),
            photos: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl SupervisionState {
    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    /// Emit a `notification.requested` event in the notifications
    /// module's payload shape: `kind`, `user_id`, `title`, `body`,
    /// `module: "supervision"`, `link`, `priority`, `channels`.
    pub(crate) fn notify(&self, kind: &str, user_id: &str, title: &str, body: &str, link: Option<String>) {
        self.emit(
            "notification.requested",
            json!({
                "kind": kind,
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "supervision",
                "category": "supervision",
                "priority": "normal",
                "link": link,
            }),
        );
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the events module.
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
/// the `role.check` hook. Fails closed: no answer means not allowed.
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

pub(crate) fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

pub(crate) fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn opt_text(value: &Option<String>, field: &str, max: usize) -> UnifiedResult<Option<String>> {
    match value {
        None => Ok(None),
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            if trimmed.len() > max {
                return Err(UnifiedError::bad_request(format!(
                    "{field} must be at most {max} characters"
                )));
            }
            Ok(Some(trimmed.to_string()))
        }
    }
}

pub(crate) fn require_id(value: &str, field: &str) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > 128 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must not be empty and must be at most 128 characters"
        )));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn valid_percent(value: f64, field: &str) -> UnifiedResult<f64> {
    if !value.is_finite() || !(0.0..=100.0).contains(&value) {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a number between 0 and 100"
        )));
    }
    Ok((value * 100.0).round() / 100.0)
}

pub(crate) fn valid_sha256(value: &str) -> UnifiedResult<String> {
    let trimmed = value.trim().to_lowercase();
    if trimmed.len() != MAX_SHA256_LEN
        || !trimmed.chars().all(|c| c.is_ascii_hexdigit())
    {
        return Err(UnifiedError::bad_request(
            "content_sha256 must be a 64-character lowercase hex SHA-256 digest",
        ));
    }
    Ok(trimmed)
}

pub(crate) fn valid_content_type(value: &str) -> UnifiedResult<String> {
    match value.trim() {
        "image/png" | "image/jpeg" | "image/webp" => Ok(value.trim().to_string()),
        _ => Err(UnifiedError::bad_request(
            "content_type must be image/png, image/jpeg, or image/webp",
        )),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn require_text_trims_and_validates() {
        assert_eq!(require_text("  hello ", "title", 10).unwrap(), "hello");
        assert!(require_text("", "title", 10).is_err());
        assert!(require_text("   ", "title", 10).is_err());
        assert!(require_text("toolongvalue", "title", 5).is_err());
    }

    #[test]
    fn opt_text_treats_blank_as_missing() {
        assert_eq!(opt_text(&None, "note", 10).unwrap(), None);
        assert_eq!(
            opt_text(&Some("   ".to_string()), "note", 10).unwrap(),
            None
        );
        assert_eq!(
            opt_text(&Some(" ok ".to_string()), "note", 10).unwrap(),
            Some("ok".to_string())
        );
        assert!(opt_text(&Some("toolongvalue".to_string()), "note", 5).is_err());
    }

    #[test]
    fn require_id_rejects_empty_and_oversize() {
        assert_eq!(require_id("abc", "project_id").unwrap(), "abc");
        assert!(require_id("", "project_id").is_err());
        assert!(require_id(&"x".repeat(129), "project_id").is_err());
    }

    #[test]
    fn valid_percent_accepts_zero_to_hundred() {
        assert_eq!(valid_percent(0.0, "progress").unwrap(), 0.0);
        assert_eq!(valid_percent(100.0, "progress").unwrap(), 100.0);
        assert_eq!(valid_percent(33.333, "progress").unwrap(), 33.33);
    }

    #[test]
    fn valid_percent_rejects_out_of_range_and_nan() {
        assert!(valid_percent(-1.0, "progress").is_err());
        assert!(valid_percent(100.1, "progress").is_err());
        assert!(valid_percent(f64::NAN, "progress").is_err());
        assert!(valid_percent(f64::INFINITY, "progress").is_err());
    }

    #[test]
    fn valid_sha256_normalizes_case() {
        let digest = "A".repeat(64);
        assert_eq!(valid_sha256(&digest).unwrap(), "a".repeat(64));
        assert!(valid_sha256(&"a".repeat(63)).is_err());
        assert!(valid_sha256(&"g".repeat(64)).is_err());
    }

    #[test]
    fn valid_content_type_allows_images_only() {
        assert!(valid_content_type("image/png").is_ok());
        assert!(valid_content_type(" image/jpeg ").is_ok());
        assert!(valid_content_type("image/webp").is_ok());
        assert!(valid_content_type("image/gif").is_err());
    }

    #[test]
    fn parse_uuid_accepts_canonical_form() {
        assert!(parse_uuid("550e8400-e29b-41d4-a716-446655440000", "project").is_ok());
        assert!(parse_uuid("not-a-uuid", "project").is_err());
    }

    #[test]
    fn default_state_starts_empty() {
        assert!(SupervisionState::default().assignments.is_empty());
        assert!(SupervisionState::default().defects.is_empty());
    }
}
