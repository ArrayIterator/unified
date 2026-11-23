//! Licensing - Licensing-as-a-service types: products, distributions, releases, event log.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{
    Module, ModuleRegistry, MODULE_ENTITLEMENT_DENIED_EVENT, MODULE_ENTITLEMENT_GRANTED_EVENT,
    RouteInfo,
};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::db::models::{License, LicenseActivation, NewLicense, NewLicenseActivation};
use crate::modules::apikeys::{
    authenticate_key, key_has_scope, note_used, AuthenticatedKey,
};
use actix_multipart::Multipart;
use actix_web::{http::header, web, HttpRequest, HttpResponse};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use diesel::prelude::*;
use futures_util::TryStreamExt;
use hmac::{Hmac, Mac};
use rand::Rng;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use uuid::Uuid;
use super::{keys::*, expiry::*, store::*, hooks::*, types::*, validation::*, handlers::*, helpers::*, product_handlers::*, verify::*, offline::*, distribution::*, releases::*, module::*};

// ---------------------------------------------------------------------------
// Licensing as a service (batch 7).
//
// New tables (migration 2026-11-22-000068_licensing_saas): see the
// migration for the PostgreSQL DDL. In this round the tables below
// live in memory (DashMap), the same pattern as the rest of this
// module; the integrator wires the migration tables into
// crates/unified-core/src/db/schema.rs` / crates/unified-core/src/db/models.rs` and the Diesel writers.
// No handler below touches crates/unified-core/src/db/schema.rs` tables that do not
// exist yet, so nothing here can fail to compile on a missing table.
// ---------------------------------------------------------------------------

/// Emitted when a public verification call fails (invalid key, wrong
/// product, expired, suspended, revoked, or out of seats). Payload:
/// `{"license_key_masked", "product", "reason", "api_key_id"}`. The
/// full key is never emitted here: only the first four characters.
pub const LICENSE_VERIFY_FAILED: &str = "license.verify.failed";

/// Emitted by the expiry scan when a license newly enters a
/// per-product renewal-reminder window.
pub const LICENSE_RENEWAL_REMINDER: &str = "license.renewal.reminder";

/// Emitted once when a license enters its product grace period.
pub const LICENSE_GRACE_STARTED: &str = "license.grace.started";

/// Emitted when an upgrade / plan-change event adjusts a license.
pub const LICENSE_UPGRADED: &str = "license.upgraded";

/// Emitted on every hash-chained event-log append, carrying
/// `{"license_id", "event_id", "hash"}` so an anchoring consumer
/// (for example the Web3 module) can anchor the hash on-chain.
/// Licensing never calls the Web3 module directly.
pub const LICENSE_ANCHORED: &str = "license.anchored";

/// Emitted when a licensed release download link is issued.
pub const LICENSE_DOWNLOAD: &str = "license.download";

/// Emitted when an admin changes a license seat count.
pub const LICENSE_SEATS_CHANGED: &str = "license.seats.changed";

/// Emitted when a git provider webhook reports a release/tag.
pub const LICENSE_DISTRIBUTION_RELEASE: &str = "license.distribution.release";

/// Store/billing upgrade events licensing listens to. The store
/// emits `store.order.upgraded` from a subscription plan upgrade
/// (`change_subscription_plan`) and billing emits
/// `billing.plan.changed` when it invoices the proration for one
/// (`on_subscription_synced`); both carry the order id, and the
/// handler resolves the order's licenses through the store
/// (`store.order.licenses`) when no license ids ride along.
pub const STORE_ORDER_UPGRADED: &str = "store.order.upgraded";

pub const BILLING_PLAN_CHANGED: &str = "billing.plan.changed";

/// Default renewal-reminder windows (days before expiry) when a
/// product does not configure its own.
pub const DEFAULT_RENEWAL_REMINDER_DAYS: &[i64] = &[30, 14, 7, 1];

/// Default offline-token allowance (days) when a product does not
/// configure its own.
pub const DEFAULT_OFFLINE_MAX_DAYS: i64 = 30;

/// Hard cap for any offline token, even when a product allows more.
pub const MAX_OFFLINE_TOKEN_DAYS: i64 = 90;

/// Maximum release upload size (100 MiB, the same cap as the storage
/// module's object upload).
pub const MAX_RELEASE_UPLOAD_BYTES: usize = 100 * 1024 * 1024;

/// Lifetime of a signed download link (1 hour, the same default as
/// the store module's signed download links).
pub const SIGNED_DOWNLOAD_TTL_SECS: i64 = 3600;

/// Default storage target name for releases when no explicit target
/// is given and a provider of that name is registered.
pub const DEFAULT_RELEASE_TARGET: &str = "licensing";

pub const KEY_FORMAT_STANDARD: &str = "standard";

pub const KEY_FORMAT_CUSTOM: &str = "custom";

pub const DISTRIBUTION_CHANNEL_GIT: &str = "git";

pub const DISTRIBUTION_CHANNEL_API: &str = "api";

pub const DISTRIBUTION_CHANNEL_FILE: &str = "file";

/// Honest trust note included in offline-token responses so no
/// consumer can mistake an offline token for a revocable credential.
pub(crate) const OFFLINE_TRUST_NOTE: &str = "Offline token trust model: this token is a bearer credential and can be replayed until expiry; it is valid for exactly one product; revoking the license or the token does not affect verifications that already hold the token; offline verification performs no database lookup.";

// ---------------------------------------------------------------------------
// SaaS records (in-memory in this round; see the section header).
// ---------------------------------------------------------------------------

/// Per-product licensing settings. `product` is the slug, unique.
/// `key_format` is `standard` (prefix + checksum body) or `custom`
/// (the generated key must also match `key_pattern`, a regex).
#[derive(Debug, Clone, Serialize)]
pub struct LicenseProduct {
    pub id: Uuid,
    pub product: String,
    pub display_name: Option<String>,
    /// Product owner (user id). When set, a public verification API
    /// key must belong to this owner; see `authenticate_public_key`.
    pub owner_id: Option<Uuid>,
    pub key_prefix: Option<String>,
    /// Number of 4-character segments in the key body (default 4).
    pub segments: i32,
    pub key_format: String,
    pub key_pattern: Option<String>,
    pub default_max_activations: i32,
    pub default_validity_days: Option<i64>,
    pub grace_period_days: i64,
    pub renewal_reminder_days: Vec<i64>,
    pub offline_max_days: i64,
    pub allowed_distribution_channels: Vec<String>,
    /// Endpoints the verification API may serve for this product
    /// (informational allowlist; empty means the verify endpoint).
    pub api_allowed_endpoints: Vec<String>,
    pub api_rate_limit_per_minute: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One git distribution channel for a product. The raw webhook
/// secret is held in memory only (it is needed to verify the
/// provider HMAC) and is never serialized; only its SHA-256 hash is
/// kept for audit and for the database column.
#[derive(Debug, Clone, Serialize)]
pub struct LicenseDistribution {
    pub id: Uuid,
    pub product: String,
    /// `github`, `gitlab`, `bitbucket`, or `generic`.
    pub provider: String,
    /// Repository as `owner/repo`.
    pub repo: String,
    /// Branch or tag reference (default `main`).
    pub git_ref: String,
    #[serde(skip_serializing)]
    pub webhook_secret: Option<String>,
    pub webhook_secret_hash: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Public view of a distribution: the secret never leaves the server.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct LicenseDistributionView {
    pub(crate) id: Uuid,
    pub(crate) product: String,
    pub(crate) provider: String,
    pub(crate) repo: String,
    pub(crate) git_ref: String,
    pub(crate) webhook_secret_configured: bool,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) updated_at: DateTime<Utc>,
}

impl From<&LicenseDistribution> for LicenseDistributionView {
    fn from(record: &LicenseDistribution) -> Self {
        Self {
            id: record.id,
            product: record.product.clone(),
            provider: record.provider.clone(),
            repo: record.repo.clone(),
            git_ref: record.git_ref.clone(),
            webhook_secret_configured: record.webhook_secret.is_some(),
            created_at: record.created_at,
            updated_at: record.updated_at,
        }
    }
}

/// One uploaded build/package attached to a licensed product.
/// `storage_key` is the object key in the storage target, or the
/// in-memory blob key when no storage target was available.
#[derive(Debug, Clone, Serialize)]
pub struct LicenseRelease {
    pub id: Uuid,
    pub product_id: Uuid,
    pub product: String,
    /// Semantic version (`1.2.3`, optionally with pre-release /
    /// build metadata).
    pub version: String,
    pub file_name: String,
    pub storage_target: Option<String>,
    pub storage_key: String,
    pub sha256_hex: String,
    pub size_bytes: i64,
    pub download_count: i64,
    pub created_at: DateTime<Utc>,
}

/// One hash-chained license event. `hash` is the SHA-256 hex of the
/// canonical payload `prev_hash + "\n" + license_id + "\n" + event +
/// "\n" + actor + "\n" + details_json`; `prev_hash` is the previous
/// entry's hash, or `GENESIS` for the first entry of a license.
#[derive(Debug, Clone, Serialize)]
pub struct LicenseEvent {
    pub id: Uuid,
    pub license_id: Uuid,
    pub event: String,
    pub actor: Option<String>,
    pub details: Value,
    pub prev_hash: String,
    pub hash: String,
    pub created_at: DateTime<Utc>,
}

/// One signed offline token record. The token itself is never stored
/// (only its id and expiry); revocation here only affects online
/// checks, never verifications that already hold the token.
#[derive(Debug, Clone, Serialize)]
pub struct LicenseOfflineToken {
    pub token_id: Uuid,
    pub license_id: Uuid,
    pub product: String,
    pub expires_at: DateTime<Utc>,
    pub issued_at: DateTime<Utc>,
    pub revoked: bool,
    pub created_at: DateTime<Utc>,
}

/// One activation-history row: every activate and deactivate, with
/// actor, instance, hostname, and timestamp.
#[derive(Debug, Clone, Serialize)]
pub struct LicenseActivationHistory {
    pub id: Uuid,
    pub license_id: Uuid,
    pub action: String,
    pub actor: Option<String>,
    pub instance_id: String,
    pub hostname: Option<String>,
    pub created_at: DateTime<Utc>,
}
