// Elearning — Certificate handlers.

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

// ---------------------------------------------------------------------------
// Certificate handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_certificates(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let params = query.into_inner();
        let is_manager = check_capability(&ctx, &caller.role, PERM_ELEARNING_MANAGE);
        let course_filter = params.filters.get("course_id").cloned();
        let user_filter = params.filters.get("user_id").cloned();
        let status_filter = params.filters.get("status").cloned();
        let anchor_filter = params.filters.get("anchor_status").cloned();
        let mut items: Vec<Certificate> = state
            .certificates
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                if !course_in_scope(&state, row.course_id, &_scope) {
                    return false;
                }
                if !is_manager && row.user_id != caller.actor {
                    return false;
                }
                if let Some(course_id) = &course_filter {
                    if row.course_id.to_string() != *course_id {
                        return false;
                    }
                }
                if let Some(user_id) = &user_filter {
                    if &row.user_id != user_id {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if &row.status != status {
                        return false;
                    }
                }
                if let Some(anchor) = &anchor_filter {
                    if &row.anchor_status != anchor {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|row| row.issued_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_certificate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Certificate> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let row = load_certificate(&state, &path.into_inner())?;
        require_course_by_id_in_scope(&state, row.course_id, &_scope)?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_ELEARNING_MANAGE);
        if !is_manager && row.user_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "Only the learner or a manager may view this certificate",
            ));
        }
        Ok(row)
    })();
    match result {
        Ok(row) => ok(row),
        Err(error) => err(error, debug),
    }
}

/// Public certificate verification: no caller headers and no
/// capability check, by design — anyone holding a code must be
/// able to verify it. The code is compared in constant time
/// against stored codes; an unknown or revoked code answers 404
/// without saying which of the two it was.
pub(crate) async fn verify_certificate(
    state: web::Data<ElearningState>,
    path: web::Path<String>,
) -> HttpResponse {
    let code = path.into_inner();
    let found = state.certificates.iter().find(|entry| {
        let row = entry.value();
        row.status == "valid" && constant_time_eq(&row.code, &code)
    });
    match found {
        Some(entry) => ok(entry.value().clone()),
        None => err(
            UnifiedError::not_found("Certificate not found or revoked"),
            false,
        ),
    }
}

/// Constant-time string comparison so verification timing does
/// not leak how much of a code matched.
pub(crate) fn constant_time_eq(a: &str, b: &str) -> bool {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    if a_bytes.len() != b_bytes.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a_bytes.iter().zip(b_bytes.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

pub(crate) async fn revoke_certificate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Certificate> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_MANAGE)?;
        let mut row = load_certificate(&state, &path.into_inner())?;
        require_course_by_id_in_scope(&state, row.course_id, &_scope)?;
        if row.status == "revoked" {
            return Err(UnifiedError::conflict("Certificate is already revoked"));
        }
        row.status = "revoked".to_string();
        row.revoked_at = Some(Utc::now());
        row.updated_at = Utc::now();
        state.certificates.insert(row.id, row.clone());
        state.emit(
            "elearning.certificate.revoked",
            json!({
                "certificate_id": row.id.to_string(),
                "code": row.code,
                "course_id": row.course_id.to_string(),
                "user_id": row.user_id,
            }),
        );
        Ok(row)
    })();
    match result {
        Ok(row) => ok(row),
        Err(error) => err(error, debug),
    }
}

/// Manual anchoring must respect the anchoring kill-switch,
/// exactly like `issue_certificate` does: when anchoring is
/// disabled in settings, a manual anchor request is a conflict,
/// never a request to the Web3 flow.
pub(crate) fn require_anchoring_enabled(settings: &ElearningSettings) -> UnifiedResult<()> {
    if settings.certificate_anchoring_enabled {
        Ok(())
    } else {
        Err(UnifiedError::conflict(
            "Certificate anchoring is disabled in settings",
        ))
    }
}

pub(crate) async fn anchor_certificate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Certificate> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_MANAGE)?;
        let mut row = load_certificate(&state, &path.into_inner())?;
        require_course_by_id_in_scope(&state, row.course_id, &_scope)?;
        if row.status != "valid" {
            return Err(UnifiedError::conflict(
                "Only a valid certificate can be anchored",
            ));
        }
        if row.anchor_status == "confirmed" {
            return Err(UnifiedError::conflict(
                "Certificate is already anchored and confirmed",
            ));
        }
        let settings = state.settings();
        // Check the kill-switch before mutating anything: a
        // disabled anchor must leave the certificate untouched.
        require_anchoring_enabled(&settings)?;
        row.anchor_status = "requested".to_string();
        row.updated_at = Utc::now();
        state.certificates.insert(row.id, row.clone());
        // Events only: the Web3 module owns the anchor itself.
        state.emit(
            "web3.anchor.requested",
            json!({
                "record_type": "elearning.certificate",
                "record_id": row.id.to_string(),
                "content_sha256": row.content_sha256,
                "network_id": settings.anchoring_network_id,
            }),
        );
        state.emit(
            "elearning.certificate.anchor.requested",
            json!({
                "certificate_id": row.id.to_string(),
                "code": row.code,
                "content_sha256": row.content_sha256,
            }),
        );
        Ok(row)
    })();
    match result {
        Ok(row) => ok(row),
        Err(error) => err(error, debug),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq_accepts_equal_strings() {
        assert!(constant_time_eq("abc123", "abc123"));
    }

    #[test]
    fn constant_time_eq_rejects_different_strings() {
        assert!(!constant_time_eq("abc123", "abc124"));
    }

    #[test]
    fn constant_time_eq_rejects_different_lengths() {
        assert!(!constant_time_eq("abc", "abcd"));
        assert!(!constant_time_eq("", "x"));
    }

    #[test]
    fn require_anchoring_enabled_returns_409_when_disabled() {
        let settings = ElearningSettings::default();
        assert!(!settings.certificate_anchoring_enabled);
        let error = require_anchoring_enabled(&settings).expect_err("anchoring is disabled");
        assert_eq!(error.code, 409);
        let mut enabled = ElearningSettings::default();
        enabled.certificate_anchoring_enabled = true;
        assert!(require_anchoring_enabled(&enabled).is_ok());
    }
}
