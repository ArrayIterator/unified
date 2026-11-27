// Domain Verification - Hook event names (inter-module contract).

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::network::DnsVerifier;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;
use uuid::Uuid;

use super::*;

// Hook event names (inter-module contract).

/// Emitted when a verification token is issued. Never carries the raw token.
pub const EVENT_TOKEN_ISSUED: &str = "domain.verification.token_issued";
/// Emitted for every single DNS check attempt.
pub const EVENT_ATTEMPTED: &str = "domain.verification.attempted";
/// Emitted when verification succeeds. The SaaS Domains module listens
/// to this event to flip the alias status to verified.
pub const EVENT_COMPLETED: &str = "domain.verification.completed";
/// Emitted when verification attempts are exhausted without success.
pub const EVENT_FAILED: &str = "domain.verification.failed";
/// Emitted when an admin changes the verification settings (audit).
pub const EVENT_SETTINGS_UPDATED: &str = "domain.verification.settings_updated";
/// Answered by this module: payload `{"domain"}`; result
/// `{"domain", "status", "verified_at"}` (no result when unknown).
pub const EVENT_STATUS_REQUESTED: &str = "domain.verification.status_requested";
/// Emitted by the scheduler for kind `domain.verify.retry`: asks
/// this module to re-check every still-pending, unexpired token
/// once. Answered by `on_verify_retry_requested` in `hooks.rs`.
pub const EVENT_VERIFY_RETRY_REQUESTED: &str = "domain.verify.retry_requested";
/// Emitted when a scheduler retry sweep finishes, with the sweep
/// summary (`checked`, `verified`, `failed`, `skipped` counts).
pub const EVENT_VERIFY_RETRY_COMPLETED: &str = "domain.verify.retry_completed";

// Verification statuses.
//
// Contract vocabulary (see the module docs): these four strings are
// fixed by the platform event contract, so resolvers, middleware, and
// the SaaS Domains module can match on them. WHEN a domain moves
// between them is driven entirely by admin settings.

/// DNS check matched the expected record for the current token.
pub const STATUS_VERIFIED: &str = "verified";
/// A current token exists and verification has not finished yet.
pub const STATUS_PENDING: &str = "pending";
/// Attempts were exhausted (or the token expired) without a match.
pub const STATUS_FAILED: &str = "failed";
/// No token is known for the domain at all.
pub const STATUS_UNKNOWN: &str = "unknown";

/// All four contract statuses, in lifecycle order.
pub const ALL_STATUSES: [&str; 4] = [
    STATUS_VERIFIED,
    STATUS_PENDING,
    STATUS_FAILED,
    STATUS_UNKNOWN,
];

// Capability names checked through the `role.check` hook (fail-closed).
pub(crate) const PERMISSION_MANAGE: &str = "domain_verification.manage";
pub(crate) const PERMISSION_ISSUE: &str = "domain_verification.issue";
pub(crate) const PERMISSION_VERIFY: &str = "domain_verification.verify";
pub(crate) const PERMISSION_READ: &str = "domain_verification.read";
