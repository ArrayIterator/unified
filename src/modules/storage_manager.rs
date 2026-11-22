// Storage Manager module — the storage hub every other module uses.
//
// This module is the MANAGEMENT layer on top of the existing storage
// abstraction (`crate::core::storage`): it does not re-implement any
// provider. Buckets/containers, per-module bindings, quotas, the file
// browser, usage stats, lifecycle rules, signed-URL revocation, and
// health checks live here; the actual bytes move through the
// `StorageProvider` trait (local, S3, R2, Google Drive, and the SFTP
// provider added in `src/core/storage.rs`).
//
// Routes live under `/api/v1/storage-manager` (NOT `/api/v1/storage`,
// which belongs to the existing storage module):
//
//   Status / defaults:
//     GET    /                              status (counts, providers, defaults)
//     GET    /defaults                      global + per-company default buckets
//     POST   /defaults                      set a global or per-company default
//   Buckets:
//     GET    /buckets                       list buckets (paginated, filtered)
//     POST   /buckets                       create a bucket (credentials write-only)
//     GET    /buckets/{id}                  one bucket (secrets never returned)
//     PATCH  /buckets/{id}                  update a bucket
//     DELETE /buckets/{id}                  delete a bucket
//     POST   /buckets/{id}/test             test the bucket connection
//     POST   /buckets/{id}/health           run a health check (probe + cleanup)
//     GET    /buckets/{id}/health           recent health checks of a bucket
//     GET    /health                        recent health checks (all buckets)
//   Objects (file browser):
//     GET    /buckets/{id}/objects          list recorded objects (paginated)
//     POST   /buckets/{id}/objects?key=     upload object bytes (raw body)
//     GET    /buckets/{id}/objects/download?key=
//                                          download object bytes (raw body, SHA-256 verified when recorded)
//     DELETE /buckets/{id}/objects?key=     delete an object
//     POST   /buckets/{id}/objects/move     move / rename within a bucket
//     POST   /buckets/{id}/objects/verify?key=
//                                          verify one object against its recorded SHA-256
//     POST   /buckets/{id}/objects/orphans/cleanup
//                                          delete provider objects with no recorded row (dry-run by default)
//   Signed URLs (revocable hub tokens):
//     POST   /buckets/{id}/signed-url       issue a hub download token (+ provider URL when supported)
//     GET    /signed-urls                   list issued tokens (paginated)
//     POST   /signed-urls/{id}/revoke       revoke a token
//     GET    /download/{token}              redeem a hub token (expiry + revocation enforced)
//   Bindings:
//     GET    /bindings                      list bindings (paginated, filtered)
//     POST   /bindings                      create a binding (module -> bucket + prefix)
//     GET    /bindings/{id}                 one binding
//     PATCH  /bindings/{id}                 update a binding (emits storage.binding.changed)
//     DELETE /bindings/{id}                 delete a binding
//     GET    /bindings/resolve?module=&company_id=
//                                          resolve the fallback chain for a module
//   Quotas:
//     GET    /quotas                        list quotas (paginated, filtered)
//     POST   /quotas                        create a quota (company / user / module)
//     GET    /quotas/{id}                   one quota
//     PATCH  /quotas/{id}                   update quota limits
//     DELETE /quotas/{id}                   delete a quota
//   Stats:
//     GET    /stats                         totals + per-bucket / company / user / module breakdowns
//     GET    /stats/daily                   daily usage rollups (from, to, filters)
//     GET    /stats/top                     top consumers by bytes
//   Lifecycle:
//     GET    /lifecycle                     list lifecycle rules
//     POST   /lifecycle                     create a lifecycle rule
//     GET    /lifecycle/{id}                one rule
//     PATCH  /lifecycle/{id}                update a rule
//     DELETE /lifecycle/{id}                delete a rule
//     POST   /lifecycle/run                 run lifecycle rules now (sync; also enqueues via scheduler)
//
// Events emitted on the hook bus:
//   - `storage.bucket.created`, `storage.bucket.updated`,
//     `storage.bucket.deleted`
//   - `storage.binding.changed` (binding created / updated / deleted,
//     and when a default bucket changes)
//   - `storage.object.stored`, `storage.object.deleted`,
//     `storage.object.archived`
//   - `storage.quota.exceeded` (hard limit, upload rejected with 409),
//     `storage.quota.warning` (soft limit crossed, upload allowed)
//   - `storage.health.failed`, `storage.health.ok`
//   - `storage.signed_url.issued`, `storage.signed_url.revoked`
//   - `storage.lifecycle.run.completed`
//   - `storage.plan.created`, `storage.plan.updated`,
//     `storage.plan.deleted`, `storage.plan.assigned`,
//     `storage.plan.changed`, `storage.plan.cancelled`,
//     `storage.plan.quota_refused` (plan change refused because
//     current usage exceeds the new plan quota)
//   - `storage.metering.reported` (buyer + plan + bytes + files +
//     period, for billing metered plans; also emitted for BYO usage)
//   - `storage.byo.created`, `storage.byo.updated`,
//     `storage.byo.validated`, `storage.byo.validation_failed`,
//     `storage.byo.activated`, `storage.byo.suspended`,
//     `storage.byo.revoked`, `storage.byo.terms_accepted`,
//     `storage.byo.terms_changed`, `storage.byo.health_ok`,
//     `storage.byo.health_failed`, `storage.byo.quota_exceeded`
// Events this module listens to:
//   - `storage.object.upload_requested` — answered with the resolved
//     binding + upload endpoint (see `on_upload_requested`). Payload:
//     `{"module", "owner", "company_id"?, "key_hint", "bytes", "mime"?}`.
//     Result: `{"resolved", "bucket_id", "bucket_name",
//     "provider_kind", "key", "prefix", "upload_endpoint",
//     "quota_ok", "quota_warning"}` or `{"resolved": false, "error", ...}`.
//   - `storage.lifecycle.run_requested` — the event the scheduler
//     emits when a `storage.lifecycle.run` job fires (see the
//     scheduler note below). The handler spawns the run in the
//     background and answers `{"accepted": true, "queued": true}`.
//   - `storage.byo.health_check_requested` — the event the
//     scheduler emits when a `storage.byo.health_check` job fires
//     (see the scheduler note below). The handler spawns health
//     checks for every active BYO connection in the background and
//     answers `{"accepted": true, "queued": true}`.
//   - This module EMITS `scheduler.enqueue` with kind
//     `storage.byo.health_check` when `POST /byo/health/run` is
//     called with `enqueue: true`.
//
// Scheduler note (integrator): the scheduler's `KNOWN_KINDS` does
// not contain `storage.byo.health_check` yet, so today the enqueue
// answers `{"queued": false, "error": "Unknown job kind: ..."}` and
// the run endpoint falls back to running synchronously in-process.
// The follow-up is to add `"storage.byo.health_check"` to
// `KNOWN_KINDS` in `src/modules/scheduler.rs` and arm it, e.g.:
//   `"storage.byo.health_check" => {
//       ctx.hooks.emit("storage.byo.health_check_requested", payload);
//   }`
// Until then BYO health runs only through `POST /byo/health/run`
// or the in-process fallback — never pretend a job was scheduled
// when the scheduler declined it. The same note applies to
// `storage.lifecycle.run` above.
//
// Storage plans (storage as a product): plans are products sold
// through Product Management / billing — this module stores only
// the quota, the included provider kinds, and a `product_id`
// billing reference plus a billing-cycle hint; it never invents a
// money column. A buyer holds at most one active plan assignment
// (company and/or user scoped). Changing a plan re-checks current
// usage against the new quota and refuses with 409 when usage
// exceeds it. Usage metering (bytes + files per buyer, from the
// recorded hub objects plus BYO objects) is reported with
// `storage.metering.reported` so billing can charge metered plans.
// Plan-gated use checks the module entitlement in
// `crate::modules::licensing` for module id `storage_manager`.
//
// Bring-your-own storage (BYO): a user or company connects their
// own external storage as a backend. Only provider kinds on the
// admin-managed allowlist may be connected; every connection is
// validated (connect, list a test prefix, write + read + delete a
// probe object, then remove it) before it can activate; the owner
// must accept the current terms version first; and every object
// operation through the connection is scoped to the connection's
// path prefix, checked against its allowed operations, file-type
// allowlists, and quotas, with ownership checked on every access.
// A cross-owner access is answered 404 (never 403), so existence
// does not leak. BYO credentials follow the bucket pattern exactly:
// write-only, encrypted with the same versioned `v1.` construction,
// never returned, logged, or placed in a hook payload.
//
// Provider-kind names used by plans and the BYO allowlist are the
// canonical strings `local`, `s3`, `r2`, `drive`, `sftp`, and
// `s3-compatible`. `drive` is the Google Drive kind and
// `s3-compatible` is the S3 kind with a custom endpoint (an endpoint
// is required for it); there is no separate `StorageKind` for
// either alias.
//
// Scheduler note (integrator): the scheduler's `KNOWN_KINDS` does
// not contain `storage.lifecycle.run` yet, so today the enqueue
// answers `{"queued": false, "error": "Unknown job kind: ..."}` and
// the run endpoint falls back to running synchronously in-process.
// The follow-up is to add `"storage.lifecycle.run"` to `KNOWN_KINDS`
// in `src/modules/scheduler.rs` and arm it, e.g.:
//   `"storage.lifecycle.run" => {
//       ctx.hooks.emit("storage.lifecycle.run_requested", payload);
//   }`
// Until then lifecycle runs only through `POST /lifecycle/run` or
// the in-process fallback — never pretend a job was scheduled when
// the scheduler declined it.
//
// Encryption at rest — honest matrix (the flag is stored per bucket;
// enforcement is provider-side where supported):
//   - S3 / R2: the existing S3 provider does NOT send a
//     server-side-encryption header today, so the flag is recorded
//     but NOT enforced by this codebase. The bucket must be created
//     with default encryption at the provider console for at-rest
//     encryption to hold. Do not read the flag as a guarantee.
//   - Local: OS / filesystem-level only (disk encryption, file
//     permissions). No application-level object encryption.
//   - Google Drive: transport encryption only (TLS + Google's
//     platform encryption); no customer-managed key support here.
//   - SFTP: transport encryption only (SSH); at-rest encryption
//     depends on the remote server's filesystem.
// The flag therefore means "the operator requires at-rest
// encryption for this bucket" and is surfaced in the bucket view so
// operators can audit it; it does not change provider behaviour.
//
// Credentials are write-only: bucket credentials are accepted once,
// encrypted with the same versioned `v1.` HMAC-keystream construction
// the AI module uses (`encrypt_secret_value` / `decrypt_secret_value`
// below), and never returned, logged, or placed in a hook payload.
// Views expose only `credentials_configured` and a short fingerprint.
// A shared core crypto helper would be better than a second copy;
// that deduplication is integrator work, not done here.
//
// File browser honesty: the browser lists objects RECORDED through
// this hub (uploads via `POST /buckets/{id}/objects` and the
// `upload_requested` flow). Providers that implement `list()`
// (local, SFTP) are also enumerated live and unrecorded keys are
// shown with `recorded: false`; providers without `list()` (S3, R2,
// Drive) cannot enumerate, so objects uploaded outside the hub are
// invisible to the browser until they are recorded. The browser
// never claims a bucket is empty when listing is unsupported.
//
// Actor identity and capability checks follow the AI module: the
// caller is identified by the `X-User-Id` header and the role by the
// `X-Role` header, and every route asks the roles module through the
// `role.check` hook. The check fails closed: no answer means not
// allowed.

use crate::core::config::AppConfig;
use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::core::storage::{
    provider_from_config, sha256_hex, validate_key, StorageKind, StorageProvider,
    StorageTargetConfig,
};
use crate::modules::licensing;
use actix_web::{http::header, web, HttpRequest, HttpResponse};
use base64::Engine;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use uuid::Uuid;

/// Maximum object upload size through the API (100 MiB), matching
/// the storage module. Larger objects must be uploaded through the
/// provider directly or in chunks by the caller.
const MAX_UPLOAD_BYTES: usize = 100 * 1024 * 1024;

/// Default hub download-token lifetime in seconds.
const DEFAULT_SIGNED_URL_SECS: u64 = 3600;
/// Maximum hub download-token lifetime in seconds (7 days).
const MAX_SIGNED_URL_SECS: u64 = 604_800;

/// Development fallback for the credential-encryption secret. The
/// configured `app.secret_key` wins whenever it is set.
const DEV_ENCRYPTION_SECRET: &str = "unified-storage-manager-development-secret";

// ---------------------------------------------------------------------------
// Permissions (capability names checked through the `role.check` hook).
// ---------------------------------------------------------------------------

const PERM_READ: &str = "storage_manager.read";
const PERM_MANAGE_BUCKETS: &str = "storage_manager.manage_buckets";
const PERM_MANAGE_BINDINGS: &str = "storage_manager.manage_bindings";
const PERM_MANAGE_QUOTAS: &str = "storage_manager.manage_quotas";
const PERM_BROWSE: &str = "storage_manager.browse";
const PERM_UPLOAD: &str = "storage_manager.upload";
const PERM_MANAGE_LIFECYCLE: &str = "storage_manager.manage_lifecycle";
const PERM_MANAGE_SIGNED_URLS: &str = "storage_manager.manage_signed_urls";
const PERM_HEALTH: &str = "storage_manager.health";
const PERM_MANAGE_PLANS: &str = "storage_manager.manage_plans";
const PERM_MANAGE_BYO: &str = "storage_manager.manage_byo";
const PERM_BYO_USE: &str = "storage_manager.use_byo";

// ---------------------------------------------------------------------------
// Hook event names.
// ---------------------------------------------------------------------------

pub mod events {
    pub const BUCKET_CREATED: &str = "storage.bucket.created";
    pub const BUCKET_UPDATED: &str = "storage.bucket.updated";
    pub const BUCKET_DELETED: &str = "storage.bucket.deleted";
    pub const BINDING_CHANGED: &str = "storage.binding.changed";
    pub const UPLOAD_REQUESTED: &str = "storage.object.upload_requested";
    pub const OBJECT_STORED: &str = "storage.object.stored";
    pub const OBJECT_DELETED: &str = "storage.object.deleted";
    pub const OBJECT_ARCHIVED: &str = "storage.object.archived";
    pub const QUOTA_EXCEEDED: &str = "storage.quota.exceeded";
    pub const QUOTA_WARNING: &str = "storage.quota.warning";
    pub const HEALTH_FAILED: &str = "storage.health.failed";
    pub const HEALTH_OK: &str = "storage.health.ok";
    pub const SIGNED_URL_ISSUED: &str = "storage.signed_url.issued";
    pub const SIGNED_URL_REVOKED: &str = "storage.signed_url.revoked";
    pub const LIFECYCLE_RUN_COMPLETED: &str = "storage.lifecycle.run.completed";
    pub const LIFECYCLE_RUN_REQUESTED: &str = "storage.lifecycle.run_requested";
    pub const SCHEDULER_ENQUEUE: &str = "scheduler.enqueue";
    pub const PLAN_CREATED: &str = "storage.plan.created";
    pub const PLAN_UPDATED: &str = "storage.plan.updated";
    pub const PLAN_DELETED: &str = "storage.plan.deleted";
    pub const PLAN_ASSIGNED: &str = "storage.plan.assigned";
    pub const PLAN_CHANGED: &str = "storage.plan.changed";
    pub const PLAN_CANCELLED: &str = "storage.plan.cancelled";
    pub const PLAN_QUOTA_REFUSED: &str = "storage.plan.quota_refused";
    pub const METERING_REPORTED: &str = "storage.metering.reported";
    /// Emitted when a download or verification finds bytes whose
    /// SHA-256 does not match the recorded checksum. The corrupt
    /// bytes are never served.
    pub const OBJECT_CHECKSUM_MISMATCH: &str = "storage.object.checksum_mismatch";
    pub const BYO_CREATED: &str = "storage.byo.created";
    pub const BYO_UPDATED: &str = "storage.byo.updated";
    pub const BYO_VALIDATED: &str = "storage.byo.validated";
    pub const BYO_VALIDATION_FAILED: &str = "storage.byo.validation_failed";
    pub const BYO_ACTIVATED: &str = "storage.byo.activated";
    pub const BYO_SUSPENDED: &str = "storage.byo.suspended";
    pub const BYO_REVOKED: &str = "storage.byo.revoked";
    pub const BYO_TERMS_ACCEPTED: &str = "storage.byo.terms_accepted";
    pub const BYO_TERMS_CHANGED: &str = "storage.byo.terms_changed";
    pub const BYO_HEALTH_FAILED: &str = "storage.byo.health_failed";
    pub const BYO_HEALTH_OK: &str = "storage.byo.health_ok";
    pub const BYO_HEALTH_CHECK_REQUESTED: &str = "storage.byo.health_check_requested";
    pub const BYO_QUOTA_EXCEEDED: &str = "storage.byo.quota_exceeded";
}

// ---------------------------------------------------------------------------
// Credential encryption (write-only storage).
//
// Same construction as the AI module (`encrypt_api_key`): a versioned
// `v1.` HMAC-SHA256 keystream (XOR stream cipher) with an HMAC tag
// over nonce + ciphertext. Kept as a local copy on purpose — see the
// module header note about a shared core helper.
// ---------------------------------------------------------------------------

type HmacSha256 = Hmac<Sha256>;

fn hmac_sha256(key: &[u8], message: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC key length is always valid");
    mac.update(message);
    mac.finalize().into_bytes().to_vec()
}

fn sha256_hex_of(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The encryption secret: `app.secret_key`, or the development
/// fallback when none is configured.
fn encryption_secret(ctx: &AppContext) -> String {
    encryption_secret_from(&ctx.config)
}

/// Same as `encryption_secret`, but from a bare config handle, so
/// hook handlers (which have no `AppContext`) can decrypt bucket
/// credentials too. The handle is captured at `init` time.
fn encryption_secret_from(config: &RwLock<AppConfig>) -> String {
    let config = config.read().expect("config lock poisoned");
    if config.app.secret_key.trim().is_empty() {
        DEV_ENCRYPTION_SECRET.to_string()
    } else {
        config.app.secret_key.clone()
    }
}

fn keystream(key: &[u8], nonce: &[u8], len: usize) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(len);
    let mut counter: u32 = 0;
    while out.len() < len {
        let mut message = nonce.to_vec();
        message.extend_from_slice(&counter.to_be_bytes());
        out.extend_from_slice(&hmac_sha256(key, &message));
        counter += 1;
    }
    out.truncate(len);
    out
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Encrypt a credential value. Output format: `v1.<base64url(nonce ||
/// ciphertext || tag)>`. The tag covers nonce + ciphertext.
fn encrypt_secret_value(secret: &str, plaintext: &str) -> String {
    let key = Sha256::digest(secret.as_bytes());
    let nonce: [u8; 16] = rand::random();
    let stream = keystream(&key, &nonce, plaintext.len());
    let ciphertext: Vec<u8> = plaintext
        .as_bytes()
        .iter()
        .zip(stream.iter())
        .map(|(plain, mask)| plain ^ mask)
        .collect();
    let mut tag_message = nonce.to_vec();
    tag_message.extend_from_slice(&ciphertext);
    let tag = hmac_sha256(&key, &tag_message);
    let mut blob = nonce.to_vec();
    blob.extend_from_slice(&ciphertext);
    blob.extend_from_slice(&tag);
    format!(
        "v1.{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(blob)
    )
}

/// Decrypt a value produced by `encrypt_secret_value`. The error
/// message never includes any part of the stored value.
fn decrypt_secret_value(secret: &str, stored: &str) -> UnifiedResult<String> {
    let encoded = stored
        .strip_prefix("v1.")
        .ok_or_else(|| UnifiedError::internal("Stored credential has an unknown format"))?;
    let blob = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| UnifiedError::internal("Stored credential cannot be decoded"))?;
    if blob.len() < 16 + 32 {
        return Err(UnifiedError::internal(
            "Stored credential cannot be decrypted",
        ));
    }
    let (nonce, rest) = blob.split_at(16);
    let (ciphertext, tag) = rest.split_at(rest.len() - 32);
    let key = Sha256::digest(secret.as_bytes());
    let mut tag_message = nonce.to_vec();
    tag_message.extend_from_slice(ciphertext);
    let expected = hmac_sha256(&key, &tag_message);
    if !constant_time_eq(&expected, tag) {
        return Err(UnifiedError::internal(
            "Stored credential failed authentication",
        ));
    }
    let stream = keystream(&key, nonce, ciphertext.len());
    let plaintext: Vec<u8> = ciphertext
        .iter()
        .zip(stream.iter())
        .map(|(cipher, mask)| cipher ^ mask)
        .collect();
    String::from_utf8(plaintext)
        .map_err(|_| UnifiedError::internal("Stored credential cannot be decrypted"))
}

/// Short fingerprint prefix of a credential value, for masked views.
/// Only the first 12 hex characters of the SHA-256 are exposed.
fn secret_fingerprint(plaintext: &str) -> String {
    sha256_hex_of(plaintext.as_bytes()).chars().take(12).collect()
}

// ---------------------------------------------------------------------------
// Stored records (in-memory DashMap stores are authoritative; the
// migration tables are persistence targets — see the module report).
// ---------------------------------------------------------------------------

/// One storage bucket (a container registered with this hub).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageBucket {
    pub id: Uuid,
    pub name: String,
    pub provider_kind: StorageKind,
    /// Endpoint URL (S3 / R2). Empty for local / Drive / SFTP.
    pub endpoint: String,
    /// Region (S3 / R2). `auto` for R2.
    pub region: String,
    /// Bucket / container name (S3 / R2). Empty for other kinds.
    pub container: String,
    /// Base directory (local provider) or base path (SFTP). Empty
    /// means the provider default.
    pub path: String,
    /// SFTP host. Must be allowlisted; see `validate_sftp_host`.
    pub sftp_host: String,
    /// SFTP port. `0` means 22.
    pub sftp_port: u16,
    /// SFTP username.
    pub sftp_username: String,
    /// SFTP auth kind: `"keyfile"` or `"password"`.
    pub sftp_auth_kind: String,
    /// SFTP private-key file path (keyfile auth only). A path, never
    /// key material.
    pub sftp_keyfile: String,
    /// Operator-configured allowlist of exact endpoint hosts, for
    /// self-hosted S3-compatible endpoints and SFTP hosts. Set by an
    /// admin at bucket creation; never taken from a request.
    pub allowed_hosts: Vec<String>,
    /// Encrypted JSON object of the credential fields (access key,
    /// secret key, password, tokens, ...). Never serialized.
    #[serde(skip_serializing)]
    pub credentials_encrypted: Option<String>,
    /// Fingerprint of the credential JSON (first 12 hex of SHA-256).
    /// Lets an operator tell whether credentials changed; it is not
    /// a credential and cannot be used to recover one.
    pub key_fingerprint: Option<String>,
    /// Encryption-at-rest requirement flag. See the module header
    /// for the honest per-provider matrix: the flag is recorded, not
    /// enforced, for S3 / R2 / Drive / SFTP.
    pub encryption_at_rest: bool,
    pub enabled: bool,
    pub company_id: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Public bucket view: credentials are never included, only whether
/// any are configured.
#[derive(Debug, Clone, Serialize)]
pub struct BucketView {
    pub id: Uuid,
    pub name: String,
    pub provider_kind: StorageKind,
    pub endpoint: String,
    pub region: String,
    pub container: String,
    pub path: String,
    pub sftp_host: String,
    pub sftp_port: u16,
    pub sftp_username: String,
    pub sftp_auth_kind: String,
    pub sftp_keyfile: String,
    pub allowed_hosts: Vec<String>,
    pub credentials_configured: bool,
    pub key_fingerprint: Option<String>,
    pub encryption_at_rest: bool,
    /// Honest per-provider statement of what `encryption_at_rest`
    /// actually means for this bucket's kind (see module header).
    pub encryption_at_rest_note: String,
    pub enabled: bool,
    pub company_id: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

fn encryption_note(kind: StorageKind) -> String {
    match kind {
        StorageKind::S3 | StorageKind::R2 => {
            "Flag recorded only: the S3 provider sends no server-side-encryption header, so at-rest encryption must be enabled as the bucket default at the provider console."
                .to_string()
        }
        StorageKind::Local => {
            "OS / filesystem-level only: no application-level object encryption is applied."
                .to_string()
        }
        StorageKind::GoogleDrive => {
            "Transport encryption only (TLS + Google platform encryption); no customer-managed key support here."
                .to_string()
        }
        StorageKind::Sftp => {
            "Transport encryption only (SSH); at-rest encryption depends on the remote server filesystem."
                .to_string()
        }
    }
}

impl From<&StorageBucket> for BucketView {
    fn from(bucket: &StorageBucket) -> Self {
        Self {
            id: bucket.id,
            name: bucket.name.clone(),
            provider_kind: bucket.provider_kind,
            endpoint: bucket.endpoint.clone(),
            region: bucket.region.clone(),
            container: bucket.container.clone(),
            path: bucket.path.clone(),
            sftp_host: bucket.sftp_host.clone(),
            sftp_port: bucket.sftp_port,
            sftp_username: bucket.sftp_username.clone(),
            sftp_auth_kind: bucket.sftp_auth_kind.clone(),
            sftp_keyfile: bucket.sftp_keyfile.clone(),
            allowed_hosts: bucket.allowed_hosts.clone(),
            credentials_configured: bucket.credentials_encrypted.is_some(),
            key_fingerprint: bucket.key_fingerprint.clone(),
            encryption_at_rest: bucket.encryption_at_rest,
            encryption_at_rest_note: encryption_note(bucket.provider_kind),
            enabled: bucket.enabled,
            company_id: bucket.company_id.clone(),
            created_by: bucket.created_by.clone(),
            created_at: bucket.created_at,
            updated_at: bucket.updated_at,
        }
    }
}

/// One per-module binding: which module stores where.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageBinding {
    pub id: Uuid,
    /// Module id that stores here (for example `backup`, `files`).
    pub module_id: String,
    pub bucket_id: Uuid,
    /// Key prefix inside the bucket (no leading slash). Empty means
    /// the bucket root.
    pub key_prefix: String,
    /// `None` means this binding applies to every company; `Some`
    /// scopes it to one company.
    pub company_id: Option<String>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One quota: a soft (warn) and a hard (reject) limit on bytes and
/// object count, for one scope. `None` on a limit means unlimited.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageQuota {
    pub id: Uuid,
    /// `"company"`, `"user"`, or `"module"`.
    pub scope: String,
    pub scope_id: String,
    pub bytes_limit: Option<i64>,
    pub objects_limit: Option<i64>,
    pub soft_bytes_limit: Option<i64>,
    pub soft_objects_limit: Option<i64>,
    pub used_bytes: i64,
    pub used_objects: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One daily usage rollup row. The key is
/// `{date}|{bucket_id}|{company_id}|{user_id}|{module_id}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageDaily {
    pub date: String,
    pub bucket_id: Uuid,
    pub company_id: Option<String>,
    pub user_id: String,
    pub module_id: String,
    pub bytes_total: i64,
    pub objects_total: i64,
    pub bytes_delta: i64,
    pub objects_delta: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One lifecycle rule for one bucket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LifecycleRule {
    pub id: Uuid,
    pub bucket_id: Uuid,
    /// Objects older than this many days are candidates.
    pub retention_days: i64,
    /// Delete candidates.
    pub auto_delete: bool,
    /// Move candidates to the archive prefix instead of deleting.
    pub archive_transition: bool,
    /// Archive prefix inside the bucket (default `archive`).
    pub archive_prefix: String,
    pub enabled: bool,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_run_summary: Option<String>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One issued hub download token. The token itself is a bearer
/// secret: it is returned once at issuance and never listed again.
/// The record stores only its fingerprint-free id; listing shows
/// metadata, never the token value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedUrlRecord {
    pub id: Uuid,
    #[serde(skip_serializing)]
    pub token: String,
    pub bucket_id: Uuid,
    pub key: String,
    pub issued_by: String,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoke_reason: Option<String>,
    /// True when a provider (presigned) URL was also issued alongside
    /// the hub token. Provider URLs cannot be revoked by the hub;
    /// only the hub token is revocable (see the module header).
    pub provider_url_issued: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Public view of a signed-URL record: the token value is never
/// included.
#[derive(Debug, Clone, Serialize)]
pub struct SignedUrlView {
    pub id: Uuid,
    pub bucket_id: Uuid,
    pub key: String,
    pub issued_by: String,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoke_reason: Option<String>,
    pub provider_url_issued: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<&SignedUrlRecord> for SignedUrlView {
    fn from(record: &SignedUrlRecord) -> Self {
        Self {
            id: record.id,
            bucket_id: record.bucket_id,
            key: record.key.clone(),
            issued_by: record.issued_by.clone(),
            expires_at: record.expires_at,
            revoked_at: record.revoked_at,
            revoke_reason: record.revoke_reason.clone(),
            provider_url_issued: record.provider_url_issued,
            created_at: record.created_at,
            updated_at: record.updated_at,
        }
    }
}

/// One health-check run for one bucket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheck {
    pub id: Uuid,
    pub bucket_id: Uuid,
    pub ok: bool,
    pub latency_ms: i64,
    /// Short outcome text. Never contains credentials or provider
    /// error bodies.
    pub detail: String,
    pub checked_at: DateTime<Utc>,
}

/// One object recorded through this hub.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredObject {
    pub bucket_id: Uuid,
    pub key: String,
    pub size_bytes: i64,
    /// SHA-256 hex digest of the bytes, recorded at upload time.
    /// `None` for objects recorded before checksums were kept, or
    /// uploaded outside the hub. Downloads verify against it when
    /// present and refuse corrupt bytes instead of serving them.
    pub sha256_hex: Option<String>,
    pub mime: String,
    pub owner_module: String,
    pub owner_user: String,
    pub company_id: Option<String>,
    pub archived: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Browser view of one object: recorded objects carry full metadata;
/// live-only keys (found by a provider `list()` but never recorded
/// through the hub) carry `recorded: false` and unknown ownership.
#[derive(Debug, Clone, Serialize)]
pub struct ObjectView {
    pub bucket_id: Uuid,
    pub key: String,
    pub size_bytes: Option<i64>,
    /// SHA-256 hex digest recorded at upload time, when known.
    pub sha256_hex: Option<String>,
    pub mime: Option<String>,
    pub owner_module: Option<String>,
    pub owner_user: Option<String>,
    pub company_id: Option<String>,
    pub archived: bool,
    pub recorded: bool,
    pub created_at: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// Shared state.
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct StorageManagerState {
    pub buckets: Arc<DashMap<Uuid, StorageBucket>>,
    pub bindings: Arc<DashMap<Uuid, StorageBinding>>,
    /// Keyed by `{scope}:{scope_id}`.
    pub quotas: Arc<DashMap<String, StorageQuota>>,
    /// Keyed by `{date}|{bucket_id}|{company_id}|{user_id}|{module_id}`.
    pub usage_daily: Arc<DashMap<String, UsageDaily>>,
    pub lifecycle_rules: Arc<DashMap<Uuid, LifecycleRule>>,
    pub signed_urls: Arc<DashMap<Uuid, SignedUrlRecord>>,
    pub health_checks: Arc<DashMap<Uuid, HealthCheck>>,
    /// Keyed by `{bucket_id}:{key}`.
    pub objects: Arc<DashMap<String, StoredObject>>,
    pub global_default: Arc<RwLock<Option<Uuid>>>,
    /// Company id -> default bucket id.
    pub company_defaults: Arc<DashMap<String, Uuid>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
    /// Application config handle, captured at `init`. Hook handlers
    /// have no `AppContext`, so they build providers from this
    /// handle instead (see `encryption_secret_from`).
    pub app_config: Arc<RwLock<Option<Arc<RwLock<AppConfig>>>>>,
    /// Storage plans sold as products (see the plans section).
    pub plans: Arc<DashMap<Uuid, StoragePlan>>,
    /// Active / cancelled plan assignments, keyed by
    /// `{company_id}|{user_id}` (company part empty for personal plans).
    pub plan_assignments: Arc<DashMap<String, PlanAssignment>>,
    /// Metering reports emitted for billing (append-only history).
    pub metering_reports: Arc<DashMap<Uuid, MeteringReport>>,
    /// Bring-your-own storage connections, keyed by connection id.
    pub byo_connections: Arc<DashMap<Uuid, ByoConnection>>,
    /// BYO objects recorded through a connection, keyed by
    /// `{connection_id}:{key}`.
    pub byo_objects: Arc<DashMap<String, ByoObject>>,
    /// BYO terms versions, keyed by version id.
    pub byo_terms: Arc<DashMap<Uuid, ByoTerms>>,
    /// Terms acceptances, keyed by `{user_id}|{terms_version}`.
    pub byo_acceptances: Arc<DashMap<String, ByoTermsAcceptance>>,
    /// BYO health-check runs, keyed by check id.
    pub byo_health_checks: Arc<DashMap<Uuid, ByoHealthCheck>>,
    /// Admin-managed BYO settings (provider allowlist, limits).
    pub byo_settings: Arc<RwLock<ByoSettings>>,
    /// Serializes the quota check + reservation step of hub uploads
    /// (see `check_quotas` / `reserve_quota`). The guard is only
    /// ever held across synchronous work — never across a provider
    /// call — so two concurrent uploads cannot both pass the check
    /// against the same counters and then both store.
    pub quota_guard: Arc<Mutex<()>>,
    /// In-flight plan-quota reservations, keyed by
    /// `{company_id}|{user_id}` (see `assignment_key`): bytes and
    /// file counts reserved by uploads that passed the plan check
    /// but have not recorded their object yet. Plan checks add the
    /// reservation to the recorded usage, so the check-then-store
    /// race cannot over-admit a buyer past their plan quota.
    pub plan_reserved: Arc<DashMap<String, (i64, i64)>>,
}

impl Default for StorageManagerState {
    fn default() -> Self {
        Self {
            buckets: Arc::new(DashMap::new()),
            bindings: Arc::new(DashMap::new()),
            quotas: Arc::new(DashMap::new()),
            usage_daily: Arc::new(DashMap::new()),
            lifecycle_rules: Arc::new(DashMap::new()),
            signed_urls: Arc::new(DashMap::new()),
            health_checks: Arc::new(DashMap::new()),
            objects: Arc::new(DashMap::new()),
            global_default: Arc::new(RwLock::new(None)),
            company_defaults: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
            app_config: Arc::new(RwLock::new(None)),
            plans: Arc::new(DashMap::new()),
            plan_assignments: Arc::new(DashMap::new()),
            metering_reports: Arc::new(DashMap::new()),
            byo_connections: Arc::new(DashMap::new()),
            byo_objects: Arc::new(DashMap::new()),
            byo_terms: Arc::new(DashMap::new()),
            byo_acceptances: Arc::new(DashMap::new()),
            byo_health_checks: Arc::new(DashMap::new()),
            byo_settings: Arc::new(RwLock::new(ByoSettings::default())),
            quota_guard: Arc::new(Mutex::new(())),
            plan_reserved: Arc::new(DashMap::new()),
        }
    }
}

impl StorageManagerState {
    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }
}

// ---------------------------------------------------------------------------
// Caller identity + capability checks (AI-module shape).
// ---------------------------------------------------------------------------

/// `X-User-Id` identifies the actor, `X-Role` carries the role slug
/// the capability check runs against.
struct Caller {
    actor: String,
    role: String,
}

fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let actor = req
        .headers()
        .get("x-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-User-Id header"))?;
    let role = req
        .headers()
        .get("x-role")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-Role header"))?;
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

/// Authorize one request: caller identity + one capability. Returns
/// the caller on success so handlers can use the actor id.
fn authorize(
    ctx: &AppContext,
    req: &HttpRequest,
    permission: &str,
) -> UnifiedResult<Caller> {
    let found = caller(req)?;
    require_permission(ctx, &found.role, permission)?;
    Ok(found)
}

fn err_response(error: UnifiedError, debug: bool) -> HttpResponse {
    json::error_response(&error, debug)
}

fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
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

// ---------------------------------------------------------------------------
// Validation helpers.
// ---------------------------------------------------------------------------

fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
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

fn optional_text(value: &Option<String>, field: &str, max: usize) -> UnifiedResult<Option<String>> {
    match value {
        None => Ok(None),
        Some(text) => Ok(Some(require_text(text, field, max)?)),
    }
}

fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw)
        .map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

/// A scope id (company, user, or module id as supplied by callers):
/// non-empty, at most 128 characters, no control characters.
fn valid_scope_id(value: &str) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > 128 {
        return Err(UnifiedError::bad_request(
            "Scope id must be 1..=128 characters",
        ));
    }
    if trimmed.chars().any(|c| c.is_control()) {
        return Err(UnifiedError::bad_request(
            "Scope id must not contain control characters",
        ));
    }
    Ok(trimmed.to_string())
}

/// A module id: lowercase letters, digits, `_`, `-`, 1..=128 chars.
fn valid_module_id(value: &str) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > 128 {
        return Err(UnifiedError::bad_request(
            "Module id must be 1..=128 characters",
        ));
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        return Err(UnifiedError::bad_request(
            "Module id must contain only lowercase letters, digits, '_' or '-'",
        ));
    }
    Ok(trimmed.to_string())
}

/// A key prefix inside a bucket: empty (bucket root) or a key-like
/// path without a leading or trailing slash.
fn valid_prefix(value: &str) -> UnifiedResult<String> {
    let trimmed = value.trim().trim_matches('/');
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    validate_key(trimmed)?;
    Ok(trimmed.to_string())
}

fn valid_quota_limit(value: Option<i64>, field: &str) -> UnifiedResult<Option<i64>> {
    match value {
        None => Ok(None),
        Some(limit) if limit < 0 => Err(UnifiedError::bad_request(format!(
            "{field} must not be negative (use null for unlimited)"
        ))),
        Some(limit) => Ok(Some(limit)),
    }
}

fn object_key(bucket_id: Uuid, key: &str) -> String {
    format!("{bucket_id}:{key}")
}

fn quota_key(scope: &str, scope_id: &str) -> String {
    format!("{scope}:{scope_id}")
}

fn load_bucket(state: &StorageManagerState, raw_id: &str) -> UnifiedResult<StorageBucket> {
    let id = parse_uuid(raw_id, "bucket")?;
    state
        .buckets
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Storage bucket not found: {id}")))
}

fn load_binding(state: &StorageManagerState, raw_id: &str) -> UnifiedResult<StorageBinding> {
    let id = parse_uuid(raw_id, "binding")?;
    state
        .bindings
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Storage binding not found: {id}")))
}

fn load_quota(state: &StorageManagerState, raw_id: &str) -> UnifiedResult<StorageQuota> {
    let id = parse_uuid(raw_id, "quota")?;
    state
        .quotas
        .iter()
        .find(|entry| entry.value().id == id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Storage quota not found: {id}")))
}

fn load_rule(state: &StorageManagerState, raw_id: &str) -> UnifiedResult<LifecycleRule> {
    let id = parse_uuid(raw_id, "lifecycle rule")?;
    state
        .lifecycle_rules
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Lifecycle rule not found: {id}")))
}

/// Build the `StorageTargetConfig` for a bucket, decrypting its
/// stored credentials. Only the non-secret fields live on the bucket;
/// the secret fields come from the encrypted credential JSON. The
/// returned config is used in memory only and never serialized.
fn target_config_for_bucket(
    ctx: &AppContext,
    bucket: &StorageBucket,
) -> UnifiedResult<StorageTargetConfig> {
    target_config_for_bucket_with(&ctx.config, bucket)
}

/// Same as `target_config_for_bucket`, but from a bare config handle.
fn target_config_for_bucket_with(
    config: &RwLock<AppConfig>,
    bucket: &StorageBucket,
) -> UnifiedResult<StorageTargetConfig> {
    let mut config = StorageTargetConfig {
        name: format!("storage-manager:{}", bucket.id),
        kind: bucket.provider_kind,
        endpoint: bucket.endpoint.clone(),
        region: bucket.region.clone(),
        bucket: bucket.container.clone(),
        base_dir: bucket.path.clone(),
        sftp_host: bucket.sftp_host.clone(),
        sftp_port: bucket.sftp_port,
        sftp_username: bucket.sftp_username.clone(),
        sftp_auth_kind: bucket.sftp_auth_kind.clone(),
        sftp_keyfile: bucket.sftp_keyfile.clone(),
        sftp_base_path: bucket.path.clone(),
        ..StorageTargetConfig::default()
    };
    if let Some(encrypted) = &bucket.credentials_encrypted {
        let secret = encryption_secret_from(config);
        let plaintext = decrypt_secret_value(&secret, encrypted)?;
        let value: Value = serde_json::from_str(&plaintext).map_err(|_| {
            UnifiedError::internal("Stored bucket credentials cannot be decoded")
        })?;
        let get = |field: &str| {
            value
                .get(field)
                .and_then(|item| item.as_str())
                .unwrap_or("")
                .to_string()
        };
        config.access_key = get("access_key");
        config.secret_key = get("secret_key");
        config.client_id = get("client_id");
        config.client_secret = get("client_secret");
        config.refresh_token = get("refresh_token");
        config.access_token = get("access_token");
        config.token_url = get("token_url");
        config.sftp_password = get("sftp_password");
    }
    Ok(config)
}

/// Build a live provider for a bucket from its stored configuration.
/// The provider is built per call (providers are cheap to construct;
/// no provider instance is cached here, so a bucket edit takes
/// effect on the next call). The bucket's operator-configured
/// `allowed_hosts` are passed as the extra-host allowlist; hosts are
/// never taken from a request.
fn provider_for_bucket(
    ctx: &AppContext,
    bucket: &StorageBucket,
) -> UnifiedResult<Arc<dyn StorageProvider>> {
    provider_for_bucket_with(&ctx.config, bucket)
}

/// Same as `provider_for_bucket`, but from a bare config handle.
fn provider_for_bucket_with(
    config: &RwLock<AppConfig>,
    bucket: &StorageBucket,
) -> UnifiedResult<Arc<dyn StorageProvider>> {
    if !bucket.enabled {
        return Err(UnifiedError::conflict(format!(
            "Storage bucket \"{}\" is disabled",
            bucket.name
        )));
    }
    let target = target_config_for_bucket_with(config, bucket)?;
    provider_from_config(&target, &bucket.allowed_hosts)
}

/// Resolve the binding for a module, following the fallback chain:
/// module binding (company-scoped first, then global) -> company
/// default bucket -> global default bucket. Returns the bucket and
/// the key prefix to store under, plus which step resolved it.
fn resolve_binding(
    state: &StorageManagerState,
    module_id: &str,
    company_id: Option<&str>,
) -> Option<(StorageBucket, String, &'static str)> {
    // 1. Module binding, company-scoped first.
    if let Some(company) = company_id {
        if let Some(found) = state.bindings.iter().find(|entry| {
            let binding = entry.value();
            binding.enabled
                && binding.module_id == module_id
                && binding.company_id.as_deref() == Some(company)
        }) {
            let binding = found.value().clone();
            if let Some(bucket) = state.buckets.get(&binding.bucket_id) {
                if bucket.enabled {
                    return Some((bucket.value().clone(), binding.key_prefix, "module"));
                }
            }
        }
    }
    // 2. Module binding, global.
    if let Some(found) = state.bindings.iter().find(|entry| {
        let binding = entry.value();
        binding.enabled && binding.module_id == module_id && binding.company_id.is_none()
    }) {
        let binding = found.value().clone();
        if let Some(bucket) = state.buckets.get(&binding.bucket_id) {
            if bucket.enabled {
                return Some((bucket.value().clone(), binding.key_prefix, "module"));
            }
        }
    }
    // 3. Company default bucket.
    if let Some(company) = company_id {
        if let Some(bucket_id) = state.company_defaults.get(company) {
            let bucket_id = *bucket_id.value();
            if let Some(bucket) = state.buckets.get(&bucket_id) {
                if bucket.enabled {
                    return Some((bucket.value().clone(), String::new(), "company_default"));
                }
            }
        }
    }
    // 4. Global default bucket.
    let global = state
        .global_default
        .read()
        .expect("global default lock poisoned");
    if let Some(bucket_id) = *global {
        if let Some(bucket) = state.buckets.get(&bucket_id) {
            if bucket.enabled {
                return Some((bucket.value().clone(), String::new(), "global_default"));
            }
        }
    }
    None
}

/// Outcome of a quota check for one upload.
struct QuotaCheck {
    /// Hard limit crossed: the upload must be rejected with 409.
    pub exceeded: bool,
    /// Soft limit crossed: warn, but allow the upload.
    pub warning: bool,
    pub scope: String,
    pub scope_id: String,
    pub detail: String,
}

/// The buyer's active storage plan, if any: a company-scoped
/// assignment wins over a personal one. Expired or non-active
/// assignments never apply.
fn active_plan_for_buyer(
    state: &StorageManagerState,
    company_id: Option<&str>,
    user_id: &str,
) -> Option<(StoragePlan, PlanAssignment)> {
    let now = Utc::now();
    let mut personal: Option<PlanAssignment> = None;
    for entry in state.plan_assignments.iter() {
        let assignment = entry.value();
        if assignment.status != "active" {
            continue;
        }
        if assignment
            .expires_at
            .is_some_and(|expires| expires <= now)
        {
            continue;
        }
        if let Some(company) = company_id {
            if assignment.company_id.as_deref() == Some(company) {
                let plan = state.plans.get(&assignment.plan_id)?;
                return Some((plan.value().clone(), assignment.clone()));
            }
        } else if assignment.company_id.is_none() && assignment.user_id == user_id {
            personal = Some(assignment.clone());
        }
    }
    let assignment = personal?;
    let plan = state.plans.get(&assignment.plan_id)?;
    Some((plan.value().clone(), assignment))
}

/// Enforce a buyer's active plan quota on one upload. Returns a
/// hard `QuotaCheck` when the plan would be exceeded, or when the
/// plan does not include the bucket's provider kind; `None` when
/// there is no active plan or the upload fits. Usage is the recorded
/// usage (`buyer_usage`) plus any in-flight reservations for the
/// same buyer, so concurrent uploads cannot over-admit past the plan.
fn check_plan_limits(
    state: &StorageManagerState,
    company_id: Option<&str>,
    user_id: &str,
    provider_kind: Option<&str>,
    delta_bytes: i64,
    delta_files: i64,
) -> Option<QuotaCheck> {
    let (plan, _) = active_plan_for_buyer(state, company_id, user_id)?;
    if let Some(kind) = provider_kind {
        if !plan.included_provider_kinds.is_empty()
            && !plan
                .included_provider_kinds
                .iter()
                .any(|included| included == kind)
        {
            return Some(QuotaCheck {
                exceeded: true,
                warning: false,
                scope: "plan".to_string(),
                scope_id: plan.id.to_string(),
                detail: format!(
                    "Storage plan \"{}\" does not include provider kind \"{kind}\"",
                    plan.name
                ),
            });
        }
    }
    let (used_bytes, used_files) = buyer_usage(state, company_id, user_id);
    let reserved = state
        .plan_reserved
        .get(&assignment_key(company_id, user_id))
        .map(|entry| *entry.value())
        .unwrap_or((0, 0));
    let bytes = used_bytes + reserved.0 + delta_bytes;
    let files = used_files + reserved.1 + delta_files;
    if bytes > plan.quota_bytes {
        return Some(QuotaCheck {
            exceeded: true,
            warning: false,
            scope: "plan".to_string(),
            scope_id: plan.id.to_string(),
            detail: format!(
                "Storage plan \"{}\" quota exceeded: {} bytes stored + {delta_bytes} exceeds the plan quota of {} bytes",
                plan.name, used_bytes, plan.quota_bytes
            ),
        });
    }
    if plan.max_files > 0 && files > plan.max_files {
        return Some(QuotaCheck {
            exceeded: true,
            warning: false,
            scope: "plan".to_string(),
            scope_id: plan.id.to_string(),
            detail: format!(
                "Storage plan \"{}\" file quota exceeded: {} files stored + {delta_files} exceeds the plan limit of {} files",
                plan.name, used_files, plan.max_files
            ),
        });
    }
    None
}

/// Check the quotas that apply to one upload (module, then user,
/// then company, then the buyer's active plan). `delta_bytes` /
/// `delta_files` are the net change the upload makes — an overwrite
/// counts only the size delta and zero new files, so re-uploading
/// the same key cannot double-count. Hard failures win over soft
/// warnings: a soft crossing in an early scope never hides a hard
/// limit in a later scope (the first soft crossing is kept as the
/// warning). The plan check runs last and its hard failure wins
/// over any soft warning.
fn check_quotas(
    state: &StorageManagerState,
    module_id: &str,
    user_id: &str,
    company_id: Option<&str>,
    provider_kind: Option<&str>,
    delta_bytes: i64,
    delta_files: i64,
) -> QuotaCheck {
    let mut scopes: Vec<(&str, String)> = vec![
        ("module", module_id.to_string()),
        ("user", user_id.to_string()),
    ];
    if let Some(company) = company_id {
        scopes.push(("company", company.to_string()));
    }
    let mut warning: Option<QuotaCheck> = None;
    for (scope, scope_id) in scopes {
        let Some(quota) = state.quotas.get(&quota_key(scope, &scope_id)) else {
            continue;
        };
        let quota = quota.value();
        if let Some(limit) = quota.bytes_limit {
            if quota.used_bytes + delta_bytes > limit {
                return QuotaCheck {
                    exceeded: true,
                    warning: false,
                    scope: scope.to_string(),
                    scope_id: scope_id.clone(),
                    detail: format!(
                        "Hard byte quota exceeded for {scope} \"{scope_id}\": used {} + {delta_bytes} exceeds the limit of {limit} bytes",
                        quota.used_bytes
                    ),
                };
            }
        }
        if let Some(limit) = quota.objects_limit {
            if quota.used_objects + delta_files > limit {
                return QuotaCheck {
                    exceeded: true,
                    warning: false,
                    scope: scope.to_string(),
                    scope_id: scope_id.clone(),
                    detail: format!(
                        "Hard object quota exceeded for {scope} \"{scope_id}\": used {} + {delta_files} exceeds the limit of {limit} objects",
                        quota.used_objects
                    ),
                };
            }
        }
        let soft_bytes = quota
            .soft_bytes_limit
            .is_some_and(|limit| quota.used_bytes + delta_bytes > limit);
        let soft_objects = quota
            .soft_objects_limit
            .is_some_and(|limit| quota.used_objects + delta_files > limit);
        if (soft_bytes || soft_objects) && warning.is_none() {
            warning = Some(QuotaCheck {
                exceeded: false,
                warning: true,
                scope: scope.to_string(),
                scope_id: scope_id.clone(),
                detail: format!(
                    "Soft quota crossed for {scope} \"{scope_id}\" (used {} bytes, {} objects)",
                    quota.used_bytes, quota.used_objects
                ),
            });
        }
    }
    if let Some(plan_check) = check_plan_limits(
        state,
        company_id,
        user_id,
        provider_kind,
        delta_bytes,
        delta_files,
    ) {
        return plan_check;
    }
    warning.unwrap_or(QuotaCheck {
        exceeded: false,
        warning: false,
        scope: String::new(),
        scope_id: String::new(),
        detail: String::new(),
    })
}

/// Apply a quota delta to the explicit quota rows that exist for
/// the upload's scopes (module, user, company). Counters are
/// clamped at zero so a release can never drive them negative.
/// Rows that do not exist are skipped: no row means no limit.
fn apply_quota_delta(
    state: &StorageManagerState,
    module_id: &str,
    user_id: &str,
    company_id: Option<&str>,
    delta_bytes: i64,
    delta_files: i64,
) {
    let now = Utc::now();
    for (scope, scope_id) in [
        ("module", module_id.to_string()),
        ("user", user_id.to_string()),
    ] {
        if let Some(mut quota) = state.quotas.get_mut(&quota_key(scope, &scope_id)) {
            quota.used_bytes = (quota.used_bytes + delta_bytes).max(0);
            quota.used_objects = (quota.used_objects + delta_files).max(0);
            quota.updated_at = now;
        }
    }
    if let Some(company) = company_id {
        if let Some(mut quota) = state.quotas.get_mut(&quota_key("company", company)) {
            quota.used_bytes = (quota.used_bytes + delta_bytes).max(0);
            quota.used_objects = (quota.used_objects + delta_files).max(0);
            quota.updated_at = now;
        }
    }
}

/// Reserve quota for one upload: the explicit quota counters and
/// the buyer's plan reservation are both increased by the upload's
/// net delta. Must be called while holding `state.quota_guard`,
/// immediately after a passing `check_quotas`, so the check and the
/// reservation are atomic. `release_quota` undoes it on failure;
/// a successful upload keeps the reservation until the object row
/// is recorded, then releases the plan part (the recorded object
/// counts towards the plan usage from then on).
fn reserve_quota(
    state: &StorageManagerState,
    module_id: &str,
    user_id: &str,
    company_id: Option<&str>,
    delta_bytes: i64,
    delta_files: i64,
) {
    apply_quota_delta(state, module_id, user_id, company_id, delta_bytes, delta_files);
    let buyer = assignment_key(company_id, user_id);
    state
        .plan_reserved
        .entry(buyer)
        .and_modify(|reserved| {
            reserved.0 += delta_bytes;
            reserved.1 += delta_files;
        })
        .or_insert((delta_bytes, delta_files));
}

/// Undo a reservation made by `reserve_quota`. Call it when the
/// provider upload fails. `release_plan_reservation` releases just
/// the plan part (used after a successful `record_stored` with
/// `apply_quota` false: the explicit counters stay reserved because
/// the object row now accounts for them, and the plan usage is
/// counted from the recorded object instead).
fn release_quota(
    state: &StorageManagerState,
    module_id: &str,
    user_id: &str,
    company_id: Option<&str>,
    delta_bytes: i64,
    delta_files: i64,
) {
    apply_quota_delta(
        state,
        module_id,
        user_id,
        company_id,
        -delta_bytes,
        -delta_files,
    );
    release_plan_reservation(state, company_id, user_id, delta_bytes, delta_files);
}

fn release_plan_reservation(
    state: &StorageManagerState,
    company_id: Option<&str>,
    user_id: &str,
    delta_bytes: i64,
    delta_files: i64,
) {
    let buyer = assignment_key(company_id, user_id);
    if let Some(mut reserved) = state.plan_reserved.get_mut(&buyer) {
        reserved.0 = (reserved.0 - delta_bytes).max(0);
        reserved.1 = (reserved.1 - delta_files).max(0);
    }
}

/// Record one stored object: the object row, the quota counters, and
/// the daily rollup. An overwrite of an existing key counts only the
/// size delta (and zero new files) everywhere, and keeps the
/// original `created_at`. When `apply_quota` is false the explicit
/// quota counters are left untouched — the caller reserved them
/// already (see `reserve_quota`) — but the object row and the daily
/// rollup are still recorded.
fn record_stored(
    state: &StorageManagerState,
    bucket_id: Uuid,
    key: &str,
    size_bytes: i64,
    sha256_hex: Option<String>,
    mime: &str,
    owner_module: &str,
    owner_user: &str,
    company_id: Option<&str>,
    apply_quota: bool,
) {
    let now = Utc::now();
    let map_key = object_key(bucket_id, key);
    let previous = state
        .objects
        .get(&map_key)
        .map(|entry| (entry.value().size_bytes, entry.value().created_at));
    let (delta_bytes, delta_files) = match previous {
        Some((old_size, _)) => (size_bytes - old_size, 0),
        None => (size_bytes, 1),
    };
    let created_at = previous.map(|(_, created)| created).unwrap_or(now);
    state.objects.insert(
        map_key,
        StoredObject {
            bucket_id,
            key: key.to_string(),
            size_bytes,
            sha256_hex,
            mime: mime.to_string(),
            owner_module: owner_module.to_string(),
            owner_user: owner_user.to_string(),
            company_id: company_id.map(str::to_string),
            archived: false,
            created_at,
            updated_at: now,
        },
    );
    if apply_quota {
        apply_quota_delta(
            state,
            owner_module,
            owner_user,
            company_id,
            delta_bytes,
            delta_files,
        );
    }
    let date = now.format("%Y-%m-%d").to_string();
    let daily_key = format!(
        "{date}|{bucket_id}|{}|{owner_user}|{owner_module}",
        company_id.unwrap_or("")
    );
    match state.usage_daily.get_mut(&daily_key) {
        Some(mut row) => {
            row.bytes_total = (row.bytes_total + delta_bytes).max(0);
            row.objects_total = (row.objects_total + delta_files).max(0);
            row.bytes_delta += delta_bytes;
            row.objects_delta += delta_files;
            row.updated_at = now;
        }
        None => {
            state.usage_daily.insert(
                daily_key,
                UsageDaily {
                    date,
                    bucket_id,
                    company_id: company_id.map(str::to_string),
                    user_id: owner_user.to_string(),
                    module_id: owner_module.to_string(),
                    // First record of the day for this scope: totals
                    // start from this object, as before.
                    bytes_total: size_bytes.max(0),
                    objects_total: 1,
                    bytes_delta: delta_bytes,
                    objects_delta: delta_files,
                    created_at: now,
                    updated_at: now,
                },
            );
        }
    }
}

/// Remove one recorded object and decrement the quota counters and
/// the daily rollup. Called after a provider delete succeeds.
fn record_deleted(state: &StorageManagerState, bucket_id: Uuid, key: &str) {
    let map_key = object_key(bucket_id, key);
    let Some((_, object)) = state.objects.remove(&map_key) else {
        return;
    };
    let now = Utc::now();
    for (scope, scope_id) in [
        ("module", object.owner_module.clone()),
        ("user", object.owner_user.clone()),
    ] {
        if let Some(mut quota) = state.quotas.get_mut(&quota_key(scope, &scope_id)) {
            quota.used_bytes = (quota.used_bytes - object.size_bytes).max(0);
            quota.used_objects = (quota.used_objects - 1).max(0);
            quota.updated_at = now;
        }
    }
    if let Some(company) = &object.company_id {
        if let Some(mut quota) = state.quotas.get_mut(&quota_key("company", company)) {
            quota.used_bytes = (quota.used_bytes - object.size_bytes).max(0);
            quota.used_objects = (quota.used_objects - 1).max(0);
            quota.updated_at = now;
        }
    }
    let date = now.format("%Y-%m-%d").to_string();
    let daily_key = format!(
        "{date}|{bucket_id}|{}|{}|{}",
        object.company_id.clone().unwrap_or_default(),
        object.owner_user,
        object.owner_module
    );
    if let Some(mut row) = state.usage_daily.get_mut(&daily_key) {
        row.bytes_total = (row.bytes_total - object.size_bytes).max(0);
        row.objects_total = (row.objects_total - 1).max(0);
        row.updated_at = now;
    }
}

// ---------------------------------------------------------------------------
// Request bodies.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CreateBucketRequest {
    name: String,
    provider_kind: StorageKind,
    #[serde(default)]
    endpoint: Option<String>,
    #[serde(default)]
    region: Option<String>,
    #[serde(default)]
    container: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    sftp_host: Option<String>,
    #[serde(default)]
    sftp_port: Option<u16>,
    #[serde(default)]
    sftp_username: Option<String>,
    #[serde(default)]
    sftp_auth_kind: Option<String>,
    #[serde(default)]
    sftp_keyfile: Option<String>,
    #[serde(default)]
    allowed_hosts: Option<Vec<String>>,
    /// Credential fields (access key, secret key, password, tokens).
    /// Write-only: accepted here, encrypted, never returned.
    #[serde(default)]
    credentials: Option<serde_json::Map<String, Value>>,
    #[serde(default)]
    encryption_at_rest: Option<bool>,
    #[serde(default)]
    company_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UpdateBucketRequest {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    endpoint: Option<String>,
    #[serde(default)]
    region: Option<String>,
    #[serde(default)]
    container: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    sftp_host: Option<String>,
    #[serde(default)]
    sftp_port: Option<u16>,
    #[serde(default)]
    sftp_username: Option<String>,
    #[serde(default)]
    sftp_auth_kind: Option<String>,
    #[serde(default)]
    sftp_keyfile: Option<String>,
    #[serde(default)]
    allowed_hosts: Option<Vec<String>>,
    /// Replaces the stored credentials when present (an empty object
    /// clears them). Absent means "keep the stored credentials".
    #[serde(default)]
    credentials: Option<serde_json::Map<String, Value>>,
    #[serde(default)]
    encryption_at_rest: Option<bool>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    company_id: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
struct CreateBindingRequest {
    module_id: String,
    bucket_id: Uuid,
    #[serde(default)]
    key_prefix: Option<String>,
    #[serde(default)]
    company_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UpdateBindingRequest {
    #[serde(default)]
    bucket_id: Option<Uuid>,
    #[serde(default)]
    key_prefix: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct CreateQuotaRequest {
    scope: String,
    scope_id: String,
    #[serde(default)]
    bytes_limit: Option<i64>,
    #[serde(default)]
    objects_limit: Option<i64>,
    #[serde(default)]
    soft_bytes_limit: Option<i64>,
    #[serde(default)]
    soft_objects_limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct UpdateQuotaRequest {
    /// Outer `None` = unchanged; inner `None` = unlimited.
    #[serde(default)]
    bytes_limit: Option<Option<i64>>,
    #[serde(default)]
    objects_limit: Option<Option<i64>>,
    #[serde(default)]
    soft_bytes_limit: Option<Option<i64>>,
    #[serde(default)]
    soft_objects_limit: Option<Option<i64>>,
}

#[derive(Debug, Deserialize)]
struct CreateLifecycleRequest {
    bucket_id: Uuid,
    retention_days: i64,
    #[serde(default)]
    auto_delete: Option<bool>,
    #[serde(default)]
    archive_transition: Option<bool>,
    #[serde(default)]
    archive_prefix: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UpdateLifecycleRequest {
    #[serde(default)]
    retention_days: Option<i64>,
    #[serde(default)]
    auto_delete: Option<bool>,
    #[serde(default)]
    archive_transition: Option<bool>,
    #[serde(default)]
    archive_prefix: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct RunLifecycleRequest {
    #[serde(default)]
    rule_id: Option<Uuid>,
    #[serde(default)]
    bucket_id: Option<Uuid>,
    /// Also ask the scheduler to enqueue a `storage.lifecycle.run`
    /// job. The scheduler declines it until the kind is registered
    /// (see the module header); the run still executes in-process.
    #[serde(default)]
    enqueue: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct IssueSignedUrlRequest {
    key: String,
    #[serde(default)]
    expires_secs: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct MoveObjectRequest {
    key: String,
    new_key: String,
}

#[derive(Debug, Deserialize)]
struct ObjectQuery {
    key: String,
}

#[derive(Debug, Deserialize)]
struct UploadQuery {
    key: String,
    /// Owning module for quota + usage accounting (default `files`).
    #[serde(default)]
    module: Option<String>,
    #[serde(default)]
    company_id: Option<String>,
    #[serde(default)]
    mime: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SetDefaultRequest {
    bucket_id: Uuid,
    #[serde(default)]
    company_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RevokeSignedUrlRequest {
    #[serde(default)]
    reason: Option<String>,
}

// ---------------------------------------------------------------------------
// Status + defaults.
// ---------------------------------------------------------------------------

async fn get_status(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_READ) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let _ = found;
    let buckets = state.buckets.len();
    let enabled_buckets = state
        .buckets
        .iter()
        .filter(|entry| entry.value().enabled)
        .count();
    let kinds: Vec<String> = StorageKind::all()
        .iter()
        .map(|kind| kind.as_str().to_string())
        .collect();
    let global_default = *state
        .global_default
        .read()
        .expect("global default lock poisoned");
    ok(json!({
        "module": "storage_manager",
        "kinds": kinds,
        "buckets": buckets,
        "enabled_buckets": enabled_buckets,
        "bindings": state.bindings.len(),
        "quotas": state.quotas.len(),
        "recorded_objects": state.objects.len(),
        "lifecycle_rules": state.lifecycle_rules.len(),
        "issued_signed_urls": state.signed_urls.len(),
        "plans": state.plans.len(),
        "plan_assignments": state.plan_assignments.len(),
        "metering_reports": state.metering_reports.len(),
        "byo_connections": state.byo_connections.len(),
        "byo_terms_versions": state.byo_terms.len(),
        "current_byo_terms_version": current_byo_terms(&state).map(|terms| terms.version),
        "byo_allowed_provider_kinds": state
            .byo_settings
            .read()
            .expect("BYO settings lock poisoned")
            .allowed_provider_kinds
            .clone(),
        "global_default_bucket_id": global_default,
        "company_defaults": state.company_defaults.len(),
        "available_as_product": true,
        "product_category": "infrastructure",
    }))
}

async fn get_defaults(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_READ) {
        return err_response(error, debug);
    }
    let global_default = *state
        .global_default
        .read()
        .expect("global default lock poisoned");
    let company_defaults: Vec<Value> = state
        .company_defaults
        .iter()
        .map(|entry| {
            json!({ "company_id": entry.key(), "bucket_id": entry.value() })
        })
        .collect();
    ok(json!({
        "global_default_bucket_id": global_default,
        "company_defaults": company_defaults,
    }))
}

async fn set_default(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<SetDefaultRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_MANAGE_BINDINGS) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<Value> = (|| {
        let input = body.into_inner();
        let bucket = state
            .buckets
            .get(&input.bucket_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!(
                    "Storage bucket not found: {}",
                    input.bucket_id
                ))
            })?;
        if !bucket.enabled {
            return Err(UnifiedError::conflict(format!(
                "Storage bucket \"{}\" is disabled and cannot be a default",
                bucket.name
            )));
        }
        match &input.company_id {
            Some(company) => {
                let company = valid_scope_id(company)?;
                state.company_defaults.insert(company.clone(), bucket.id);
                state.emit(
                    events::BINDING_CHANGED,
                    json!({
                        "scope": "company_default",
                        "company_id": company,
                        "bucket_id": bucket.id.to_string(),
                        "changed_by": found.actor,
                    }),
                );
                Ok(json!({
                    "company_id": company,
                    "bucket_id": bucket.id,
                    "bucket_name": bucket.name,
                }))
            }
            None => {
                *state
                    .global_default
                    .write()
                    .expect("global default lock poisoned") = Some(bucket.id);
                state.emit(
                    events::BINDING_CHANGED,
                    json!({
                        "scope": "global_default",
                        "bucket_id": bucket.id.to_string(),
                        "changed_by": found.actor,
                    }),
                );
                Ok(json!({
                    "company_id": Value::Null,
                    "bucket_id": bucket.id,
                    "bucket_name": bucket.name,
                }))
            }
        }
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Buckets.
// ---------------------------------------------------------------------------

/// Encrypt a credential map (plain JSON object) for storage. Only
/// string values are kept; anything else is rejected so a caller
/// cannot smuggle structured data into the credential blob.
fn encrypt_credentials(
    ctx: &AppContext,
    credentials: &serde_json::Map<String, Value>,
) -> UnifiedResult<(String, String)> {
    let mut plain = serde_json::Map::new();
    for (key, value) in credentials {
        let text = value.as_str().ok_or_else(|| {
            UnifiedError::bad_request(format!(
                "Credential field \"{key}\" must be a string"
            ))
        })?;
        if key.trim().is_empty() || key.len() > 128 {
            return Err(UnifiedError::bad_request(
                "Credential field names must be 1..=128 characters",
            ));
        }
        plain.insert(key.clone(), Value::String(text.to_string()));
    }
    let plaintext = serde_json::to_string(&Value::Object(plain)).map_err(|_| {
        UnifiedError::internal("Cannot encode bucket credentials")
    })?;
    let secret = encryption_secret(ctx);
    let encrypted = encrypt_secret_value(&secret, &plaintext);
    let fingerprint = secret_fingerprint(&plaintext);
    Ok((encrypted, fingerprint))
}

fn validate_bucket_fields(
    ctx: &AppContext,
    bucket: &StorageBucket,
) -> UnifiedResult<()> {
    // Building the provider validates the kind-specific fields
    // (endpoint + allowlist for S3/R2, host allowlist for SFTP, and
    // so on). A bucket that cannot build a provider cannot be
    // created: fail here, before anything is stored.
    let config = target_config_for_bucket(ctx, bucket)?;
    provider_from_config(&config, &bucket.allowed_hosts).map(|_| ())
}

async fn list_buckets(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_READ) {
        return err_response(error, debug);
    }
    let params = params.into_inner();
    let mut items: Vec<BucketView> = state
        .buckets
        .iter()
        .map(|entry| BucketView::from(entry.value()))
        .collect();
    if let Some(search) = &params.search {
        let needle = search.to_lowercase();
        items.retain(|bucket| {
            bucket.name.to_lowercase().contains(&needle)
                || bucket.container.to_lowercase().contains(&needle)
        });
    }
    if let Some(kind) = params.filters.get("provider_kind") {
        items.retain(|bucket| bucket.provider_kind.as_str() == kind.as_str());
    }
    if let Some(company) = params.filters.get("company_id") {
        items.retain(|bucket| bucket.company_id.as_deref() == Some(company.as_str()));
    }
    if let Some(enabled) = params.filters.get("enabled") {
        let want = enabled == "true";
        items.retain(|bucket| bucket.enabled == want);
    }
    let sort = params.allowed_sort(&["name", "created_at"], "name");
    let descending = params.normalized().order == "desc";
    items.sort_by(|a, b| {
        let ordering = match sort.as_str() {
            "created_at" => a.created_at.cmp(&b.created_at),
            _ => a.name.cmp(&b.name),
        };
        if descending {
            ordering.reverse()
        } else {
            ordering
        }
    });
    ok(paginate(items, &params))
}

async fn create_bucket(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateBucketRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_MANAGE_BUCKETS) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<BucketView> = (|| {
        let input = body.into_inner();
        let name = require_text(&input.name, "name", 128)?;
        if state
            .buckets
            .iter()
            .any(|entry| entry.value().name == name)
        {
            return Err(UnifiedError::conflict(format!(
                "Storage bucket \"{name}\" already exists"
            )));
        }
        let endpoint = optional_text(&input.endpoint, "endpoint", 1024)?.unwrap_or_default();
        let region = optional_text(&input.region, "region", 128)?.unwrap_or_default();
        let container = optional_text(&input.container, "container", 256)?.unwrap_or_default();
        let path = optional_text(&input.path, "path", 1024)?.unwrap_or_default();
        let sftp_host = optional_text(&input.sftp_host, "sftp_host", 253)?.unwrap_or_default();
        let sftp_username =
            optional_text(&input.sftp_username, "sftp_username", 64)?.unwrap_or_default();
        let sftp_auth_kind = match input.sftp_auth_kind {
            Some(raw) => require_text(&raw, "sftp_auth_kind", 32)?,
            None => String::new(),
        };
        let sftp_keyfile =
            optional_text(&input.sftp_keyfile, "sftp_keyfile", 4096)?.unwrap_or_default();
        let mut allowed_hosts: Vec<String> = Vec::new();
        if let Some(hosts) = input.allowed_hosts {
            for host in hosts {
                let host = require_text(&host, "allowed_hosts entry", 253)?;
                allowed_hosts.push(host.to_ascii_lowercase());
            }
            allowed_hosts.sort();
            allowed_hosts.dedup();
        }
        let company_id = optional_text(&input.company_id, "company_id", 128)?;
        let (credentials_encrypted, key_fingerprint) = match &input.credentials {
            Some(map) => {
                let (encrypted, fingerprint) = encrypt_credentials(&ctx, map)?;
                (Some(encrypted), Some(fingerprint))
            }
            None => (None, None),
        };
        let now = Utc::now();
        let bucket = StorageBucket {
            id: Uuid::now_v7(),
            name,
            provider_kind: input.provider_kind,
            endpoint,
            region,
            container,
            path,
            sftp_host,
            sftp_port: input.sftp_port.unwrap_or(0),
            sftp_username,
            sftp_auth_kind,
            sftp_keyfile,
            allowed_hosts,
            credentials_encrypted,
            key_fingerprint,
            encryption_at_rest: input.encryption_at_rest.unwrap_or(false),
            enabled: true,
            company_id,
            created_by: found.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        // Fail before storing when the bucket cannot build a live
        // provider (bad endpoint host, unallowlisted SFTP host,
        // missing credentials, ...).
        validate_bucket_fields(&ctx, &bucket)?;
        let view = BucketView::from(&bucket);
        state.buckets.insert(bucket.id, bucket.clone());
        state.emit(
            events::BUCKET_CREATED,
            json!({
                "bucket_id": bucket.id.to_string(),
                "name": bucket.name,
                "provider_kind": bucket.provider_kind.as_str(),
                "encryption_at_rest": bucket.encryption_at_rest,
                "created_by": found.actor,
            }),
        );
        Ok(view)
    })();
    match result {
        Ok(view) => created(view),
        Err(error) => err_response(error, debug),
    }
}

async fn get_bucket(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_READ) {
        return err_response(error, debug);
    }
    json::respond(
        load_bucket(&state, &id).map(|bucket| BucketView::from(&bucket)),
        debug,
    )
}

async fn update_bucket(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    body: web::Json<UpdateBucketRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_MANAGE_BUCKETS) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<BucketView> = (|| {
        let input = body.into_inner();
        let mut bucket = load_bucket(&state, &id)?;
        if let Some(raw) = &input.name {
            let name = require_text(raw, "name", 128)?;
            if state
                .buckets
                .iter()
                .any(|entry| entry.value().name == name && entry.value().id != bucket.id)
            {
                return Err(UnifiedError::conflict(format!(
                    "Storage bucket \"{name}\" already exists"
                )));
            }
            bucket.name = name;
        }
        if let Some(raw) = &input.endpoint {
            bucket.endpoint = require_text(raw, "endpoint", 1024)?;
        }
        if let Some(raw) = &input.region {
            bucket.region = require_text(raw, "region", 128)?;
        }
        if let Some(raw) = &input.container {
            bucket.container = require_text(raw, "container", 256)?;
        }
        if let Some(raw) = &input.path {
            bucket.path = require_text(raw, "path", 1024)?;
        }
        if let Some(raw) = &input.sftp_host {
            bucket.sftp_host = require_text(raw, "sftp_host", 253)?;
        }
        if let Some(port) = input.sftp_port {
            bucket.sftp_port = port;
        }
        if let Some(raw) = &input.sftp_username {
            bucket.sftp_username = require_text(raw, "sftp_username", 64)?;
        }
        if let Some(raw) = &input.sftp_auth_kind {
            bucket.sftp_auth_kind = require_text(raw, "sftp_auth_kind", 32)?;
        }
        if let Some(raw) = &input.sftp_keyfile {
            bucket.sftp_keyfile = require_text(raw, "sftp_keyfile", 4096)?;
        }
        if let Some(hosts) = input.allowed_hosts {
            let mut cleaned: Vec<String> = Vec::new();
            for host in hosts {
                cleaned.push(require_text(&host, "allowed_hosts entry", 253)?.to_ascii_lowercase());
            }
            cleaned.sort();
            cleaned.dedup();
            bucket.allowed_hosts = cleaned;
        }
        if let Some(map) = &input.credentials {
            if map.is_empty() {
                bucket.credentials_encrypted = None;
                bucket.key_fingerprint = None;
            } else {
                let (encrypted, fingerprint) = encrypt_credentials(&ctx, map)?;
                bucket.credentials_encrypted = Some(encrypted);
                bucket.key_fingerprint = Some(fingerprint);
            }
        }
        if let Some(flag) = input.encryption_at_rest {
            bucket.encryption_at_rest = flag;
        }
        if let Some(enabled) = input.enabled {
            bucket.enabled = enabled;
        }
        if let Some(company) = input.company_id {
            bucket.company_id = match company {
                Some(raw) => Some(require_text(&raw, "company_id", 128)?),
                None => None,
            };
        }
        validate_bucket_fields(&ctx, &bucket)?;
        bucket.updated_at = Utc::now();
        let view = BucketView::from(&bucket);
        state.buckets.insert(bucket.id, bucket.clone());
        state.emit(
            events::BUCKET_UPDATED,
            json!({
                "bucket_id": bucket.id.to_string(),
                "name": bucket.name,
                "provider_kind": bucket.provider_kind.as_str(),
                "enabled": bucket.enabled,
                "updated_by": found.actor,
            }),
        );
        Ok(view)
    })();
    json::respond(result, debug)
}

async fn delete_bucket(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_MANAGE_BUCKETS) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<Value> = (|| {
        let bucket = load_bucket(&state, &id)?;
        if state
            .bindings
            .iter()
            .any(|entry| entry.value().bucket_id == bucket.id && entry.value().enabled)
        {
            return Err(UnifiedError::conflict(format!(
                "Storage bucket \"{}\" still has enabled bindings; delete or re-point them first",
                bucket.name
            )));
        }
        if state
            .lifecycle_rules
            .iter()
            .any(|entry| entry.value().bucket_id == bucket.id)
        {
            return Err(UnifiedError::conflict(format!(
                "Storage bucket \"{}\" still has lifecycle rules; delete them first",
                bucket.name
            )));
        }
        // Recorded objects are metadata only; the provider objects
        // themselves are left in place (deleting a bucket must never
        // silently delete stored bytes). The metadata rows go with
        // the bucket so the browser cannot show a deleted bucket.
        state
            .objects
            .retain(|_, object| object.bucket_id != bucket.id);
        state.buckets.remove(&bucket.id);
        // Defaults pointing at the deleted bucket fall back to none
        // rather than dangling.
        {
            let mut global = state
                .global_default
                .write()
                .expect("global default lock poisoned");
            if *global == Some(bucket.id) {
                *global = None;
            }
        }
        state
            .company_defaults
            .retain(|_, value| *value != bucket.id);
        state.emit(
            events::BUCKET_DELETED,
            json!({
                "bucket_id": bucket.id.to_string(),
                "name": bucket.name,
                "deleted_by": found.actor,
            }),
        );
        Ok(json!({ "bucket_id": bucket.id, "name": bucket.name, "deleted": true }))
    })();
    json::respond(result, debug)
}

async fn test_bucket(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_MANAGE_BUCKETS) {
        return err_response(error, debug);
    }
    let result: UnifiedResult<Value> = (|| async {
        let bucket = load_bucket(&state, &id)?;
        let provider = provider_for_bucket(&ctx, &bucket)?;
        provider.test_connection().await?;
        Ok(json!({
            "bucket_id": bucket.id,
            "name": bucket.name,
            "provider_kind": bucket.provider_kind.as_str(),
            "ok": true,
        }))
    })()
    .await;
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Health checks.
// ---------------------------------------------------------------------------

/// Run one health check: connectivity plus a write / read / delete
/// probe on a scratch key, then cleanup. The scratch object is
/// removed even when the read fails, so a failed probe never leaves
/// bytes behind. Returns the recorded check.
async fn run_health_check(
    state: &StorageManagerState,
    config: &RwLock<AppConfig>,
    bucket: &StorageBucket,
) -> UnifiedResult<HealthCheck> {
    let started = std::time::Instant::now();
    let outcome: UnifiedResult<()> = (|| async {
        let provider = provider_for_bucket_with(config, bucket)?;
        provider.test_connection().await?;
        let probe_key = format!(".healthcheck-{}/probe.txt", uuid::Uuid::now_v7());
        let probe_body = b"storage-manager-health-probe".to_vec();
        provider.upload(&probe_key, probe_body.clone()).await?;
        let read_back = provider.download(&probe_key).await;
        let delete_result = provider.delete(&probe_key).await;
        let bytes = read_back?;
        if bytes != probe_body {
            // Best-effort cleanup, then report the mismatch.
            let _ = delete_result;
            return Err(UnifiedError::internal(
                "Health probe read back different bytes than were written",
            ));
        }
        delete_result?;
        Ok(())
    })()
    .await;
    let latency_ms = started.elapsed().as_millis() as i64;
    let now = Utc::now();
    let (ok, detail) = match &outcome {
        Ok(()) => (true, "Connectivity and write/read/delete probe passed.".to_string()),
        Err(error) => (false, error.message.clone()),
    };
    let check = HealthCheck {
        id: Uuid::now_v7(),
        bucket_id: bucket.id,
        ok,
        latency_ms,
        detail,
        checked_at: now,
    };
    state.health_checks.insert(check.id, check.clone());
    if ok {
        state.emit(
            events::HEALTH_OK,
            json!({
                "bucket_id": bucket.id.to_string(),
                "name": bucket.name,
                "latency_ms": latency_ms,
                "checked_at": now.to_rfc3339(),
            }),
        );
    } else {
        state.emit(
            events::HEALTH_FAILED,
            json!({
                "bucket_id": bucket.id.to_string(),
                "name": bucket.name,
                "error": check.detail.clone(),
                "latency_ms": latency_ms,
                "checked_at": now.to_rfc3339(),
            }),
        );
    }
    match outcome {
        Ok(()) => Ok(check),
        Err(error) => Err(error),
    }
}

async fn health_bucket(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_HEALTH) {
        return err_response(error, debug);
    }
    let result: UnifiedResult<HealthCheck> = (|| async {
        let bucket = load_bucket(&state, &id)?;
        run_health_check(&state, &ctx.config, &bucket).await
    })()
    .await;
    json::respond(result, debug)
}

async fn list_health(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_HEALTH) {
        return err_response(error, debug);
    }
    let params = params.into_inner();
    let mut items: Vec<HealthCheck> = state
        .health_checks
        .iter()
        .map(|entry| entry.value().clone())
        .collect();
    if let Some(bucket) = params.filters.get("bucket_id") {
        items.retain(|check| check.bucket_id.to_string() == *bucket);
    }
    if let Some(ok) = params.filters.get("ok") {
        let want = ok == "true";
        items.retain(|check| check.ok == want);
    }
    items.sort_by(|a, b| b.checked_at.cmp(&a.checked_at));
    ok(paginate(items, &params))
}

async fn list_bucket_health(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_HEALTH) {
        return err_response(error, debug);
    }
    let bucket = match load_bucket(&state, &id) {
        Ok(bucket) => bucket,
        Err(error) => return err_response(error, debug),
    };
    let params = params.into_inner();
    let mut items: Vec<HealthCheck> = state
        .health_checks
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|check| check.bucket_id == bucket.id)
        .collect();
    items.sort_by(|a, b| b.checked_at.cmp(&a.checked_at));
    ok(paginate(items, &params))
}

// ---------------------------------------------------------------------------
// Objects (file browser + upload / download / delete / move).
// ---------------------------------------------------------------------------

async fn list_objects(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_BROWSE) {
        return err_response(error, debug);
    }
    let bucket = match load_bucket(&state, &id) {
        Ok(bucket) => bucket,
        Err(error) => return err_response(error, debug),
    };
    let params = params.into_inner();
    let prefix = params
        .filters
        .get("prefix")
        .cloned()
        .unwrap_or_default();
    let mut items: Vec<ObjectView> = state
        .objects
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|object| object.bucket_id == bucket.id)
        .filter(|object| prefix.is_empty() || object.key.starts_with(prefix.as_str()))
        .map(|object| ObjectView {
            bucket_id: object.bucket_id,
            key: object.key.clone(),
            size_bytes: Some(object.size_bytes),
            sha256_hex: object.sha256_hex.clone(),
            mime: Some(object.mime.clone()),
            owner_module: Some(object.owner_module.clone()),
            owner_user: Some(object.owner_user.clone()),
            company_id: object.company_id.clone(),
            archived: object.archived,
            recorded: true,
            created_at: Some(object.created_at),
        })
        .collect();
    // Live enumeration where the provider supports it: unrecorded
    // keys are shown with `recorded: false`. When the provider
    // cannot list, the recorded rows are the whole answer (see the
    // module header) — that is reported, not hidden.
    let mut listing_supported = false;
    let mut listing_error: Option<String> = None;
    if let Ok(provider) = provider_for_bucket(&ctx, &bucket) {
        match provider.list(&prefix).await {
            Ok(keys) => {
                listing_supported = true;
                for key in keys {
                    if !items.iter().any(|item| item.key == key) {
                        items.push(ObjectView {
                            bucket_id: bucket.id,
                            key,
                            size_bytes: None,
                            sha256_hex: None,
                            mime: None,
                            owner_module: None,
                            owner_user: None,
                            company_id: None,
                            archived: false,
                            recorded: false,
                            created_at: None,
                        });
                    }
                }
            }
            Err(error) => {
                listing_error = Some(error.message.clone());
            }
        }
    }
    if let Some(search) = &params.search {
        let needle = search.to_lowercase();
        items.retain(|object| object.key.to_lowercase().contains(&needle));
    }
    if let Some(module) = params.filters.get("module") {
        items.retain(|object| object.owner_module.as_deref() == Some(module.as_str()));
    }
    if let Some(user) = params.filters.get("user_id") {
        items.retain(|object| object.owner_user.as_deref() == Some(user.as_str()));
    }
    items.sort_by(|a, b| a.key.cmp(&b.key));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    let mut value = page.to_json();
    if let Value::Object(map) = &mut value {
        map.insert(
            "listing_supported".to_string(),
            Value::Bool(listing_supported),
        );
        map.insert(
            "listing_error".to_string(),
            listing_error.map(Value::String).unwrap_or(Value::Null),
        );
    }
    ok(value)
}

async fn upload_object(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    query: web::Query<UploadQuery>,
    body: web::Bytes,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_UPLOAD) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<Value> = (|| async {
        let bucket = load_bucket(&state, &id)?;
        if body.len() > MAX_UPLOAD_BYTES {
            return Err(UnifiedError::new(
                format!("Upload exceeds the maximum size of {MAX_UPLOAD_BYTES} bytes"),
                413,
            ));
        }
        let key = query.key.trim().to_string();
        validate_key(&key)?;
        let module_id = match &query.module {
            Some(raw) => valid_module_id(raw)?,
            None => "files".to_string(),
        };
        let company_id = match &query.company_id {
            Some(raw) => Some(valid_scope_id(raw)?),
            None => None,
        };
        let mime = query
            .mime
            .clone()
            .unwrap_or_else(|| "application/octet-stream".to_string());
        let size_bytes = body.len() as i64;
        let checksum = sha256_hex(&body);
        // The net change this upload makes: an overwrite of an
        // existing key counts only the size delta and zero new
        // files, so re-uploading the same key cannot double-count.
        let size_key = object_key(bucket.id, &key);
        let previous_size = state
            .objects
            .get(&size_key)
            .map(|entry| entry.value().size_bytes)
            .unwrap_or(0);
        let is_new = !state.objects.contains_key(&size_key);
        let delta_bytes = size_bytes - previous_size;
        let delta_files = i64::from(is_new);
        // Check + reserve under one guard so two concurrent uploads
        // cannot both pass against the same counters (the guard is
        // only held across synchronous work, never the provider
        // call). The reservation is released again when the provider
        // upload fails.
        let check = {
            let _guard = state
                .quota_guard
                .lock()
                .expect("quota guard lock poisoned");
            let check = check_quotas(
                &state,
                &module_id,
                &found.actor,
                company_id.as_deref(),
                Some(bucket.provider_kind.as_str()),
                delta_bytes,
                delta_files,
            );
            if !check.exceeded {
                reserve_quota(
                    &state,
                    &module_id,
                    &found.actor,
                    company_id.as_deref(),
                    delta_bytes,
                    delta_files,
                );
            }
            check
        };
        if check.exceeded {
            state.emit(
                events::QUOTA_EXCEEDED,
                json!({
                    "scope": check.scope,
                    "scope_id": check.scope_id,
                    "module": module_id,
                    "user_id": found.actor,
                    "company_id": company_id,
                    "bucket_id": bucket.id.to_string(),
                    "key": key,
                    "bytes": body.len(),
                    "detail": check.detail,
                }),
            );
            return Err(UnifiedError::conflict(check.detail)
                .with_metadata("quota_exceeded", json!(true))
                .with_metadata("scope", json!(check.scope))
                .with_metadata("scope_id", json!(check.scope_id)));
        }
        let provider = provider_for_bucket(&ctx, &bucket)?;
        let stored_key = match provider.upload(&key, body.to_vec()).await {
            Ok(stored_key) => stored_key,
            Err(error) => {
                release_quota(
                    &state,
                    &module_id,
                    &found.actor,
                    company_id.as_deref(),
                    delta_bytes,
                    delta_files,
                );
                return Err(error);
            }
        };
        record_stored(
            &state,
            bucket.id,
            &stored_key,
            size_bytes,
            Some(checksum.clone()),
            &mime,
            &module_id,
            &found.actor,
            company_id.as_deref(),
            false,
        );
        // The object row now counts towards the plan usage, so the
        // plan part of the reservation is released (the explicit
        // quota counters stay reserved — they were the reservation).
        release_plan_reservation(
            &state,
            company_id.as_deref(),
            &found.actor,
            delta_bytes,
            delta_files,
        );
        if check.warning {
            state.emit(
                events::QUOTA_WARNING,
                json!({
                    "scope": check.scope,
                    "scope_id": check.scope_id,
                    "module": module_id,
                    "user_id": found.actor,
                    "company_id": company_id,
                    "bucket_id": bucket.id.to_string(),
                    "key": stored_key,
                    "bytes": body.len(),
                    "detail": check.detail,
                }),
            );
        }
        state.emit(
            events::OBJECT_STORED,
            json!({
                "bucket_id": bucket.id.to_string(),
                "bucket_name": bucket.name,
                "key": stored_key,
                "size_bytes": body.len(),
                "sha256_hex": checksum,
                "mime": mime,
                "module": module_id,
                "owner_user": found.actor,
                "company_id": company_id,
            }),
        );
        Ok(json!({
            "bucket_id": bucket.id,
            "key": stored_key,
            "size_bytes": body.len(),
            "sha256_hex": checksum,
            "mime": mime,
            "module": module_id,
            "quota_warning": check.warning,
            "quota_warning_detail": if check.warning { Some(check.detail) } else { None },
        }))
    })()
    .await;
    match result {
        Ok(view) => created(view),
        Err(error) => err_response(error, debug),
    }
}

async fn download_object(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    query: web::Query<ObjectQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_BROWSE) {
        return err_response(error, debug);
    }
    let bucket = match load_bucket(&state, &id) {
        Ok(bucket) => bucket,
        Err(error) => return err_response(error, debug),
    };
    if let Err(error) = validate_key(&query.key) {
        return err_response(error, debug);
    }
    let provider = match provider_for_bucket(&ctx, &bucket) {
        Ok(provider) => provider,
        Err(error) => return err_response(error, debug),
    };
    let recorded = state
        .objects
        .get(&object_key(bucket.id, &query.key))
        .and_then(|entry| entry.value().sha256_hex.clone());
    let downloaded = match &recorded {
        Some(expected) => provider.download_verified(&query.key, expected).await,
        None => provider.download(&query.key).await,
    };
    match downloaded {
        Ok(bytes) => {
            let mime = state
                .objects
                .get(&object_key(bucket.id, &query.key))
                .map(|entry| entry.value().mime.clone())
                .unwrap_or_else(|| "application/octet-stream".to_string());
            let file_name = query
                .key
                .rsplit('/')
                .next()
                .unwrap_or(&query.key)
                .replace('"', "");
            // `verified` when the bytes matched a recorded checksum,
            // `unverified` for objects recorded before checksums were
            // kept (see the verify endpoint to backfill them).
            let integrity = if recorded.is_some() {
                "verified"
            } else {
                "unverified"
            };
            HttpResponse::Ok()
                .content_type(mime)
                .insert_header((
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{file_name}\""),
                ))
                .insert_header(("x-object-integrity", integrity))
                .body(bytes)
        }
        Err(error) => {
            if error
                .metadata
                .get("checksum_mismatch")
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
            {
                state.emit(
                    events::OBJECT_CHECKSUM_MISMATCH,
                    json!({
                        "bucket_id": bucket.id.to_string(),
                        "key": query.key,
                        "expected_sha256": recorded,
                    }),
                );
            }
            err_response(error, debug)
        }
    }
}

async fn delete_object(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    query: web::Query<ObjectQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_UPLOAD) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<Value> = (|| async {
        let bucket = load_bucket(&state, &id)?;
        validate_key(&query.key)?;
        let provider = provider_for_bucket(&ctx, &bucket)?;
        provider.delete(&query.key).await?;
        record_deleted(&state, bucket.id, &query.key);
        state.emit(
            events::OBJECT_DELETED,
            json!({
                "bucket_id": bucket.id.to_string(),
                "bucket_name": bucket.name,
                "key": query.key,
                "deleted_by": found.actor,
            }),
        );
        Ok(json!({ "bucket_id": bucket.id, "key": query.key, "deleted": true }))
    })()
    .await;
    json::respond(result, debug)
}

/// Verify one recorded object: download it, hash the bytes, and
/// compare with the recorded SHA-256. A mismatch is reported as
/// data (`"status": "mismatch"`) and emits
/// `storage.object.checksum_mismatch` — the endpoint itself never
/// fails on a mismatch, so a scanner can sweep a whole bucket. An
/// object with no recorded checksum is backfilled from the bytes
/// just read (`"status": "recorded"`), so later downloads verify.
/// A provider-level checksum failure (local / SFTP sidecar) is
/// reported as a mismatch too.
async fn verify_object(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    query: web::Query<ObjectQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_BROWSE) {
        return err_response(error, debug);
    }
    let result: UnifiedResult<Value> = (|| async {
        let bucket = load_bucket(&state, &id)?;
        validate_key(&query.key)?;
        let provider = provider_for_bucket(&ctx, &bucket)?;
        let recorded = state
            .objects
            .get(&object_key(bucket.id, &query.key))
            .and_then(|entry| entry.value().sha256_hex.clone());
        match provider.download(&query.key).await {
            Ok(bytes) => {
                let actual = sha256_hex(&bytes);
                match &recorded {
                    Some(expected) if *expected == actual => Ok(json!({
                        "bucket_id": bucket.id,
                        "key": query.key,
                        "recorded_sha256": expected,
                        "actual_sha256": actual,
                        "match": true,
                        "status": "verified",
                    })),
                    Some(expected) => {
                        state.emit(
                            events::OBJECT_CHECKSUM_MISMATCH,
                            json!({
                                "bucket_id": bucket.id.to_string(),
                                "key": query.key,
                                "expected_sha256": expected,
                                "actual_sha256": actual,
                            }),
                        );
                        Ok(json!({
                            "bucket_id": bucket.id,
                            "key": query.key,
                            "recorded_sha256": expected,
                            "actual_sha256": actual,
                            "match": false,
                            "status": "mismatch",
                        }))
                    }
                    None => {
                        if let Some(mut object) = state
                            .objects
                            .get_mut(&object_key(bucket.id, &query.key))
                        {
                            object.sha256_hex = Some(actual.clone());
                            object.updated_at = Utc::now();
                        }
                        Ok(json!({
                            "bucket_id": bucket.id,
                            "key": query.key,
                            "recorded_sha256": Value::Null,
                            "actual_sha256": actual,
                            "match": Value::Null,
                            "status": "recorded",
                        }))
                    }
                }
            }
            Err(error)
                if error
                    .metadata
                    .get("checksum_mismatch")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false) =>
            {
                state.emit(
                    events::OBJECT_CHECKSUM_MISMATCH,
                    json!({
                        "bucket_id": bucket.id.to_string(),
                        "key": query.key,
                        "expected_sha256": recorded,
                    }),
                );
                Ok(json!({
                    "bucket_id": bucket.id,
                    "key": query.key,
                    "recorded_sha256": recorded,
                    "actual_sha256": Value::Null,
                    "match": false,
                    "status": "mismatch",
                }))
            }
            Err(error) => Err(error),
        }
    })()
    .await;
    json::respond(result, debug)
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
struct OrphanCleanupRequest {
    /// When true (the default) only report the orphans; pass
    /// `false` to actually delete them.
    #[serde(default = "default_true")]
    dry_run: bool,
    /// Only scan keys under this prefix (empty means the whole
    /// bucket).
    #[serde(default)]
    prefix: String,
}

/// Delete provider objects that have no recorded hub row
/// (orphans): objects uploaded outside the hub, or left behind by
/// a failed or partial operation. Dry-run by default — the caller
/// must pass `"dry_run": false` to delete anything. Providers that
/// cannot enumerate objects (S3 / R2 / Drive) answer with an error
/// instead of pretending the bucket is empty.
async fn cleanup_orphans(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    body: web::Json<OrphanCleanupRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_MANAGE_BUCKETS) {
        return err_response(error, debug);
    }
    let result: UnifiedResult<Value> = (|| async {
        let bucket = load_bucket(&state, &id)?;
        let prefix = body.prefix.trim().to_string();
        let provider = provider_for_bucket(&ctx, &bucket)?;
        let keys = provider.list(&prefix).await.map_err(|error| {
            UnifiedError::bad_request(format!(
                "Orphan scan needs a provider listing: {}",
                error.message
            ))
        })?;
        if keys.len() > 10_000 {
            return Err(UnifiedError::bad_request(
                "Orphan scan found more than 10000 keys; narrow it with a prefix",
            ));
        }
        let mut orphans: Vec<String> = Vec::new();
        for key in &keys {
            if !state
                .objects
                .contains_key(&object_key(bucket.id, key))
            {
                orphans.push(key.clone());
            }
        }
        let orphan_count = orphans.len();
        let mut deleted = 0usize;
        let mut errors: Vec<String> = Vec::new();
        if !body.dry_run {
            for key in &orphans {
                match provider.delete(key).await {
                    Ok(()) => deleted += 1,
                    Err(error) => {
                        if errors.len() < 10 {
                            errors.push(format!("\"{key}\": {}", error.message));
                        }
                    }
                }
            }
        }
        Ok(json!({
            "bucket_id": bucket.id,
            "prefix": prefix,
            "dry_run": body.dry_run,
            "examined": keys.len(),
            "orphan_count": orphan_count,
            "orphans": orphans.iter().take(100).cloned().collect::<Vec<_>>(),
            "deleted": deleted,
            "errors": errors,
        }))
    })()
    .await;
    json::respond(result, debug)
}

async fn move_object(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    body: web::Json<MoveObjectRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_UPLOAD) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<Value> = (|| async {
        let bucket = load_bucket(&state, &id)?;
        let input = body.into_inner();
        validate_key(&input.key)?;
        validate_key(&input.new_key)?;
        if input.key == input.new_key {
            return Err(UnifiedError::bad_request(
                "new_key must differ from key",
            ));
        }
        // A move is download + upload + delete through the provider
        // contract (no provider exposes a native rename). The source
        // bytes are held in memory, so moves are capped at the same
        // 100 MiB as uploads.
        if let Some(object) = state.objects.get(&object_key(bucket.id, &input.key)) {
            if object.size_bytes > MAX_UPLOAD_BYTES as i64 {
                return Err(UnifiedError::new(
                    format!(
                        "Object exceeds the maximum move size of {MAX_UPLOAD_BYTES} bytes"
                    ),
                    413,
                ));
            }
        }
        let provider = provider_for_bucket(&ctx, &bucket)?;
        let bytes = provider.download(&input.key).await?;
        if bytes.len() > MAX_UPLOAD_BYTES {
            return Err(UnifiedError::new(
                format!(
                    "Object exceeds the maximum move size of {MAX_UPLOAD_BYTES} bytes"
                ),
                413,
            ));
        }
        provider.upload(&input.new_key, bytes.clone()).await?;
        // The source is deleted only after the destination write
        // succeeded, so a failed upload never loses the object.
        provider.delete(&input.key).await?;
        let (mime, module, user, company) =
            match state.objects.get(&object_key(bucket.id, &input.key)) {
                Some(object) => (
                    object.mime.clone(),
                    object.owner_module.clone(),
                    object.owner_user.clone(),
                    object.company_id.clone(),
                ),
                None => (
                    "application/octet-stream".to_string(),
                    "files".to_string(),
                    found.actor.clone(),
                    None,
                ),
            };
        record_deleted(&state, bucket.id, &input.key);
        record_stored(
            &state,
            bucket.id,
            &input.new_key,
            bytes.len() as i64,
            Some(sha256_hex(&bytes)),
            &mime,
            &module,
            &user,
            company.as_deref(),
            true,
        );
        state.emit(
            events::OBJECT_STORED,
            json!({
                "bucket_id": bucket.id.to_string(),
                "key": input.new_key,
                "size_bytes": bytes.len(),
                "module": module,
                "owner_user": user,
                "company_id": company,
                "moved_from": input.key,
            }),
        );
        state.emit(
            events::OBJECT_DELETED,
            json!({
                "bucket_id": bucket.id.to_string(),
                "key": input.key,
                "deleted_by": found.actor,
                "moved_to": input.new_key,
            }),
        );
        Ok(json!({
            "bucket_id": bucket.id,
            "key": input.new_key,
            "moved_from": input.key,
            "size_bytes": bytes.len(),
        }))
    })()
    .await;
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Signed URLs (revocable hub download tokens).
// ---------------------------------------------------------------------------

async fn issue_signed_url(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    body: web::Json<IssueSignedUrlRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_MANAGE_SIGNED_URLS) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<Value> = (|| async {
        let bucket = load_bucket(&state, &id)?;
        validate_key(&body.key)?;
        let expires_secs = body
            .expires_secs
            .unwrap_or(DEFAULT_SIGNED_URL_SECS)
            .clamp(1, MAX_SIGNED_URL_SECS);
        let expires_at = Utc::now() + chrono::Duration::seconds(expires_secs as i64);
        // A provider (presigned) URL is a bonus when the provider
        // supports it. Local and SFTP honestly refuse; the hub token
        // below is the revocable path for them.
        let provider = provider_for_bucket(&ctx, &bucket)?;
        let (provider_url, provider_url_issued) = match provider.signed_url(&body.key, expires_secs).await {
            Ok(url) => (Some(url), true),
            Err(_) => (None, false),
        };
        // The hub token is a bearer secret: it is returned once here
        // and never listed again. Only the record id is stored in
        // the map key; the token value itself lives on the record
        // (in memory only, never in a log or hook payload).
        let token = Uuid::new_v4().to_string();
        let now = Utc::now();
        let record = SignedUrlRecord {
            id: Uuid::now_v7(),
            token: token.clone(),
            bucket_id: bucket.id,
            key: body.key.clone(),
            issued_by: found.actor.clone(),
            expires_at,
            revoked_at: None,
            revoke_reason: None,
            provider_url_issued,
            created_at: now,
            updated_at: now,
        };
        state.signed_urls.insert(record.id, record.clone());
        state.emit(
            events::SIGNED_URL_ISSUED,
            json!({
                "signed_url_id": record.id.to_string(),
                "bucket_id": bucket.id.to_string(),
                "key": body.key,
                "issued_by": found.actor,
                "expires_at": expires_at.to_rfc3339(),
                "provider_url_issued": provider_url_issued,
            }),
        );
        Ok(json!({
            "signed_url_id": record.id,
            "token": token,
            "download_url": format!("/api/v1/storage-manager/download/{token}"),
            "provider_url": provider_url,
            "bucket_id": bucket.id,
            "key": body.key,
            "expires_secs": expires_secs,
            "expires_at": expires_at,
        }))
    })()
    .await;
    match result {
        Ok(view) => created(view),
        Err(error) => err_response(error, debug),
    }
}

async fn list_signed_urls(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_MANAGE_SIGNED_URLS) {
        return err_response(error, debug);
    }
    let params = params.into_inner();
    let mut items: Vec<SignedUrlView> = state
        .signed_urls
        .iter()
        .map(|entry| SignedUrlView::from(entry.value()))
        .collect();
    if let Some(bucket) = params.filters.get("bucket_id") {
        items.retain(|record| record.bucket_id.to_string() == *bucket);
    }
    if let Some(revoked) = params.filters.get("revoked") {
        let want = revoked == "true";
        items.retain(|record| record.revoked_at.is_some() == want);
    }
    if let Some(issuer) = params.filters.get("issued_by") {
        items.retain(|record| record.issued_by == *issuer);
    }
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    ok(paginate(items, &params))
}

async fn revoke_signed_url(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    body: web::Json<RevokeSignedUrlRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_MANAGE_SIGNED_URLS) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<SignedUrlView> = (|| {
        let record_id = parse_uuid(&id, "signed URL")?;
        // Take the view (and the fields the event needs) out of the
        // guard's scope first: the guard must be dropped before
        // emitting, so a hook handler can never deadlock on the map.
        let (view, bucket_id, key, reason) = {
            let mut record = state
                .signed_urls
                .get_mut(&record_id)
                .ok_or_else(|| {
                    UnifiedError::not_found(format!("Signed URL not found: {record_id}"))
                })?;
            if record.revoked_at.is_some() {
                return Err(UnifiedError::conflict(
                    "This signed URL is already revoked",
                ));
            }
            let reason = match &body.reason {
                Some(raw) => Some(require_text(raw, "reason", 500)?),
                None => None,
            };
            record.revoked_at = Some(Utc::now());
            record.revoke_reason = reason.clone();
            record.updated_at = Utc::now();
            let view = SignedUrlView::from(&*record);
            (
                view,
                record.bucket_id,
                record.key.clone(),
                reason,
            )
        };
        state.emit(
            events::SIGNED_URL_REVOKED,
            json!({
                "signed_url_id": record_id.to_string(),
                "bucket_id": bucket_id.to_string(),
                "key": key,
                "revoked_by": found.actor,
                "reason": reason,
            }),
        );
        Ok(view)
    })();
    json::respond(result, debug)
}

/// Redeem a hub download token. No capability check here by design:
/// possession of the token IS the authorization, exactly like a
/// presigned URL. Expiry and revocation are enforced on every
/// redemption; the token value is never logged.
async fn redeem_download(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    token: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let token = token.into_inner();
    let record = state
        .signed_urls
        .iter()
        .find(|entry| entry.value().token == token)
        .map(|entry| entry.value().clone());
    let Some(record) = record else {
        return err_response(
            UnifiedError::not_found("Download token not found"),
            debug,
        );
    };
    if record.revoked_at.is_some() {
        return err_response(
            UnifiedError::forbidden("This download token has been revoked"),
            debug,
        );
    }
    if record.expires_at <= Utc::now() {
        return err_response(
            UnifiedError::new("This download token has expired", 410),
            debug,
        );
    }
    let Some(bucket) = state.buckets.get(&record.bucket_id) else {
        return err_response(
            UnifiedError::not_found("The bucket for this download token no longer exists"),
            debug,
        );
    };
    let bucket = bucket.value().clone();
    let provider = match provider_for_bucket(&ctx, &bucket) {
        Ok(provider) => provider,
        Err(error) => return err_response(error, debug),
    };
    match provider.download(&record.key).await {
        Ok(bytes) => {
            let file_name = record
                .key
                .rsplit('/')
                .next()
                .unwrap_or(&record.key)
                .replace('"', "");
            HttpResponse::Ok()
                .content_type("application/octet-stream")
                .insert_header((
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{file_name}\""),
                ))
                .body(bytes)
        }
        Err(error) => err_response(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Bindings.
// ---------------------------------------------------------------------------

async fn list_bindings(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_READ) {
        return err_response(error, debug);
    }
    let params = params.into_inner();
    let mut items: Vec<StorageBinding> = state
        .bindings
        .iter()
        .map(|entry| entry.value().clone())
        .collect();
    if let Some(module) = params.filters.get("module_id") {
        items.retain(|binding| binding.module_id == *module);
    }
    if let Some(bucket) = params.filters.get("bucket_id") {
        items.retain(|binding| binding.bucket_id.to_string() == *bucket);
    }
    if let Some(company) = params.filters.get("company_id") {
        items.retain(|binding| binding.company_id.as_deref() == Some(company.as_str()));
    }
    if let Some(enabled) = params.filters.get("enabled") {
        let want = enabled == "true";
        items.retain(|binding| binding.enabled == want);
    }
    if let Some(search) = &params.search {
        let needle = search.to_lowercase();
        items.retain(|binding| binding.module_id.to_lowercase().contains(&needle));
    }
    items.sort_by(|a, b| {
        a.module_id
            .cmp(&b.module_id)
            .then(a.company_id.cmp(&b.company_id))
    });
    ok(paginate(items, &params))
}

async fn create_binding(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateBindingRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_MANAGE_BINDINGS) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<StorageBinding> = (|| {
        let input = body.into_inner();
        let module_id = valid_module_id(&input.module_id)?;
        let bucket = state
            .buckets
            .get(&input.bucket_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!(
                    "Storage bucket not found: {}",
                    input.bucket_id
                ))
            })?;
        if !bucket.enabled {
            return Err(UnifiedError::conflict(format!(
                "Storage bucket \"{}\" is disabled and cannot be bound",
                bucket.name
            )));
        }
        let key_prefix = match &input.key_prefix {
            Some(raw) => valid_prefix(raw)?,
            None => String::new(),
        };
        let company_id = match &input.company_id {
            Some(raw) => Some(valid_scope_id(raw)?),
            None => None,
        };
        if state.bindings.iter().any(|entry| {
            let binding = entry.value();
            binding.enabled
                && binding.module_id == module_id
                && binding.company_id == company_id
        }) {
            return Err(UnifiedError::conflict(format!(
                "Module \"{module_id}\" already has an enabled binding{}; update or delete it first",
                match &company_id {
                    Some(company) => format!(" for company \"{company}\""),
                    None => String::new(),
                }
            )));
        }
        let now = Utc::now();
        let binding = StorageBinding {
            id: Uuid::now_v7(),
            module_id,
            bucket_id: bucket.id,
            key_prefix,
            company_id,
            enabled: true,
            created_at: now,
            updated_at: now,
        };
        state.bindings.insert(binding.id, binding.clone());
        state.emit(
            events::BINDING_CHANGED,
            json!({
                "binding_id": binding.id.to_string(),
                "module_id": binding.module_id,
                "bucket_id": binding.bucket_id.to_string(),
                "key_prefix": binding.key_prefix,
                "company_id": binding.company_id,
                "action": "created",
                "changed_by": found.actor,
            }),
        );
        Ok(binding)
    })();
    match result {
        Ok(view) => created(view),
        Err(error) => err_response(error, debug),
    }
}

async fn get_binding(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_READ) {
        return err_response(error, debug);
    }
    json::respond(load_binding(&state, &id), debug)
}

async fn update_binding(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    body: web::Json<UpdateBindingRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_MANAGE_BINDINGS) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<StorageBinding> = (|| {
        let input = body.into_inner();
        let mut binding = load_binding(&state, &id)?;
        if let Some(bucket_id) = input.bucket_id {
            let bucket = state
                .buckets
                .get(&bucket_id)
                .map(|entry| entry.value().clone())
                .ok_or_else(|| {
                    UnifiedError::not_found(format!("Storage bucket not found: {bucket_id}"))
                })?;
            if !bucket.enabled {
                return Err(UnifiedError::conflict(format!(
                    "Storage bucket \"{}\" is disabled and cannot be bound",
                    bucket.name
                )));
            }
            binding.bucket_id = bucket_id;
        }
        if let Some(raw) = &input.key_prefix {
            binding.key_prefix = valid_prefix(raw)?;
        }
        if let Some(enabled) = input.enabled {
            binding.enabled = enabled;
        }
        binding.updated_at = Utc::now();
        state.bindings.insert(binding.id, binding.clone());
        state.emit(
            events::BINDING_CHANGED,
            json!({
                "binding_id": binding.id.to_string(),
                "module_id": binding.module_id,
                "bucket_id": binding.bucket_id.to_string(),
                "key_prefix": binding.key_prefix,
                "company_id": binding.company_id,
                "enabled": binding.enabled,
                "action": "updated",
                "changed_by": found.actor,
            }),
        );
        Ok(binding)
    })();
    json::respond(result, debug)
}

async fn delete_binding(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_MANAGE_BINDINGS) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<Value> = (|| {
        let binding = load_binding(&state, &id)?;
        state.bindings.remove(&binding.id);
        state.emit(
            events::BINDING_CHANGED,
            json!({
                "binding_id": binding.id.to_string(),
                "module_id": binding.module_id,
                "bucket_id": binding.bucket_id.to_string(),
                "company_id": binding.company_id,
                "action": "deleted",
                "changed_by": found.actor,
            }),
        );
        Ok(json!({ "binding_id": binding.id, "deleted": true }))
    })();
    json::respond(result, debug)
}

async fn resolve_binding_endpoint(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<HashMap<String, String>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_READ) {
        return err_response(error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let module_raw = params.get("module").ok_or_else(|| {
            UnifiedError::bad_request("Query parameter \"module\" is required")
        })?;
        let module_id = valid_module_id(module_raw)?;
        let company_id = params
            .get("company_id")
            .map(|raw| valid_scope_id(raw))
            .transpose()?;
        match resolve_binding(&state, &module_id, company_id.as_deref()) {
            Some((bucket, prefix, source)) => Ok(json!({
                "module_id": module_id,
                "company_id": company_id,
                "resolved": true,
                "source": source,
                "bucket_id": bucket.id,
                "bucket_name": bucket.name,
                "provider_kind": bucket.provider_kind.as_str(),
                "key_prefix": prefix,
            })),
            None => Ok(json!({
                "module_id": module_id,
                "company_id": company_id,
                "resolved": false,
                "source": "none",
                "bucket_id": Value::Null,
                "bucket_name": Value::Null,
                "provider_kind": Value::Null,
                "key_prefix": "",
            })),
        }
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Quotas.
// ---------------------------------------------------------------------------

async fn list_quotas(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_READ) {
        return err_response(error, debug);
    }
    let params = params.into_inner();
    let mut items: Vec<StorageQuota> = state
        .quotas
        .iter()
        .map(|entry| entry.value().clone())
        .collect();
    if let Some(scope) = params.filters.get("scope") {
        items.retain(|quota| quota.scope == *scope);
    }
    if let Some(scope_id) = params.filters.get("scope_id") {
        items.retain(|quota| quota.scope_id == *scope_id);
    }
    if let Some(search) = &params.search {
        let needle = search.to_lowercase();
        items.retain(|quota| quota.scope_id.to_lowercase().contains(&needle));
    }
    items.sort_by(|a, b| a.scope.cmp(&b.scope).then(a.scope_id.cmp(&b.scope_id)));
    ok(paginate(items, &params))
}

async fn create_quota(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateQuotaRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_MANAGE_QUOTAS) {
        return err_response(error, debug);
    }
    let result: UnifiedResult<StorageQuota> = (|| {
        let input = body.into_inner();
        let scope = match input.scope.trim() {
            "company" | "user" | "module" => input.scope.trim().to_string(),
            other => {
                return Err(UnifiedError::bad_request(format!(
                    "Quota scope must be \"company\", \"user\", or \"module\", not \"{other}\""
                )));
            }
        };
        let scope_id = if scope == "module" {
            valid_module_id(&input.scope_id)?
        } else {
            valid_scope_id(&input.scope_id)?
        };
        let bytes_limit = valid_quota_limit(input.bytes_limit, "bytes_limit")?;
        let objects_limit = valid_quota_limit(input.objects_limit, "objects_limit")?;
        let soft_bytes_limit = valid_quota_limit(input.soft_bytes_limit, "soft_bytes_limit")?;
        let soft_objects_limit =
            valid_quota_limit(input.soft_objects_limit, "soft_objects_limit")?;
        if let (Some(soft), Some(hard)) = (soft_bytes_limit, bytes_limit) {
            if soft > hard {
                return Err(UnifiedError::bad_request(
                    "soft_bytes_limit must not exceed bytes_limit",
                ));
            }
        }
        if let (Some(soft), Some(hard)) = (soft_objects_limit, objects_limit) {
            if soft > hard {
                return Err(UnifiedError::bad_request(
                    "soft_objects_limit must not exceed objects_limit",
                ));
            }
        }
        let key = quota_key(&scope, &scope_id);
        if state.quotas.contains_key(&key) {
            return Err(UnifiedError::conflict(format!(
                "A quota already exists for {scope} \"{scope_id}\""
            )));
        }
        let now = Utc::now();
        let quota = StorageQuota {
            id: Uuid::now_v7(),
            scope,
            scope_id,
            bytes_limit,
            objects_limit,
            soft_bytes_limit,
            soft_objects_limit,
            used_bytes: 0,
            used_objects: 0,
            created_at: now,
            updated_at: now,
        };
        state.quotas.insert(key, quota.clone());
        Ok(quota)
    })();
    match result {
        Ok(view) => created(view),
        Err(error) => err_response(error, debug),
    }
}

async fn get_quota(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_READ) {
        return err_response(error, debug);
    }
    json::respond(load_quota(&state, &id), debug)
}

async fn update_quota(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    body: web::Json<UpdateQuotaRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_MANAGE_QUOTAS) {
        return err_response(error, debug);
    }
    let result: UnifiedResult<StorageQuota> = (|| {
        let input = body.into_inner();
        let mut quota = load_quota(&state, &id)?;
        if let Some(limit) = input.bytes_limit {
            quota.bytes_limit = valid_quota_limit(limit, "bytes_limit")?;
        }
        if let Some(limit) = input.objects_limit {
            quota.objects_limit = valid_quota_limit(limit, "objects_limit")?;
        }
        if let Some(limit) = input.soft_bytes_limit {
            quota.soft_bytes_limit = valid_quota_limit(limit, "soft_bytes_limit")?;
        }
        if let Some(limit) = input.soft_objects_limit {
            quota.soft_objects_limit = valid_quota_limit(limit, "soft_objects_limit")?;
        }
        if let (Some(soft), Some(hard)) = (quota.soft_bytes_limit, quota.bytes_limit) {
            if soft > hard {
                return Err(UnifiedError::bad_request(
                    "soft_bytes_limit must not exceed bytes_limit",
                ));
            }
        }
        if let (Some(soft), Some(hard)) = (quota.soft_objects_limit, quota.objects_limit) {
            if soft > hard {
                return Err(UnifiedError::bad_request(
                    "soft_objects_limit must not exceed objects_limit",
                ));
            }
        }
        quota.updated_at = Utc::now();
        let key = quota_key(&quota.scope, &quota.scope_id);
        state.quotas.insert(key, quota.clone());
        Ok(quota)
    })();
    json::respond(result, debug)
}

async fn delete_quota(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_MANAGE_QUOTAS) {
        return err_response(error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let quota = load_quota(&state, &id)?;
        state
            .quotas
            .remove(&quota_key(&quota.scope, &quota.scope_id));
        Ok(json!({ "quota_id": quota.id, "deleted": true }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Stats.
// ---------------------------------------------------------------------------

async fn get_stats(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<HashMap<String, String>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_READ) {
        return err_response(error, debug);
    }
    let bucket_filter = params.get("bucket_id").cloned();
    let company_filter = params.get("company_id").cloned();
    let user_filter = params.get("user_id").cloned();
    let module_filter = params.get("module").cloned();
    let objects: Vec<StoredObject> = state
        .objects
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|object| {
            bucket_filter
                .as_ref()
                .is_none_or(|id| object.bucket_id.to_string() == *id)
                && company_filter
                    .as_ref()
                    .is_none_or(|id| object.company_id.as_deref() == Some(id.as_str()))
                && user_filter
                    .as_ref()
                    .is_none_or(|id| object.owner_user == *id)
                && module_filter
                    .as_ref()
                    .is_none_or(|id| object.owner_module == *id)
        })
        .collect();
    let total_bytes: i64 = objects.iter().map(|object| object.size_bytes).sum();
    let total_objects = objects.len() as i64;
    let mut by_bucket: HashMap<String, (i64, i64)> = HashMap::new();
    let mut by_company: HashMap<String, (i64, i64)> = HashMap::new();
    let mut by_user: HashMap<String, (i64, i64)> = HashMap::new();
    let mut by_module: HashMap<String, (i64, i64)> = HashMap::new();
    for object in &objects {
        let entry = by_bucket
            .entry(object.bucket_id.to_string())
            .or_default();
        entry.0 += object.size_bytes;
        entry.1 += 1;
        let company = object.company_id.clone().unwrap_or_default();
        let entry = by_company.entry(company).or_default();
        entry.0 += object.size_bytes;
        entry.1 += 1;
        let entry = by_user.entry(object.owner_user.clone()).or_default();
        entry.0 += object.size_bytes;
        entry.1 += 1;
        let entry = by_module.entry(object.owner_module.clone()).or_default();
        entry.0 += object.size_bytes;
        entry.1 += 1;
    }
    // `json!` treats a bare identifier key as a literal string, so
    // dynamic dimension names are built as maps by hand.
    let to_list = |map: HashMap<String, (i64, i64)>, key_name: &str| {
        let mut list: Vec<Value> = map
            .into_iter()
            .map(|(key, (bytes, objects))| {
                let mut item = serde_json::Map::new();
                item.insert(key_name.to_string(), Value::String(key));
                item.insert("bytes".to_string(), json!(bytes));
                item.insert("objects".to_string(), json!(objects));
                Value::Object(item)
            })
            .collect();
        list.sort_by(|a, b| {
            b.get("bytes")
                .and_then(Value::as_i64)
                .cmp(&a.get("bytes").and_then(Value::as_i64))
        });
        list
    };
    ok(json!({
        "total_bytes": total_bytes,
        "total_objects": total_objects,
        "buckets": state.buckets.len(),
        "by_bucket": to_list(by_bucket, "bucket_id"),
        "by_company": to_list(by_company, "company_id"),
        "by_user": to_list(by_user, "user_id"),
        "by_module": to_list(by_module, "module_id"),
    }))
}

async fn get_stats_daily(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_READ) {
        return err_response(error, debug);
    }
    let params = params.into_inner();
    let mut items: Vec<UsageDaily> = state
        .usage_daily
        .iter()
        .map(|entry| entry.value().clone())
        .collect();
    if let Some(from) = params.filters.get("from") {
        items.retain(|row| row.date >= *from);
    }
    if let Some(to) = params.filters.get("to") {
        items.retain(|row| row.date <= *to);
    }
    if let Some(bucket) = params.filters.get("bucket_id") {
        items.retain(|row| row.bucket_id.to_string() == *bucket);
    }
    if let Some(company) = params.filters.get("company_id") {
        items.retain(|row| row.company_id.as_deref() == Some(company.as_str()));
    }
    if let Some(user) = params.filters.get("user_id") {
        items.retain(|row| row.user_id == *user);
    }
    if let Some(module) = params.filters.get("module") {
        items.retain(|row| row.module_id == *module);
    }
    items.sort_by(|a, b| {
        b.date
            .cmp(&a.date)
            .then(a.bucket_id.cmp(&b.bucket_id))
            .then(a.module_id.cmp(&b.module_id))
    });
    ok(paginate(items, &params))
}

async fn get_stats_top(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<HashMap<String, String>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_READ) {
        return err_response(error, debug);
    }
    let dimension = params.get("by").map(String::as_str).unwrap_or("module");
    if !matches!(dimension, "module" | "user" | "company" | "bucket") {
        return err_response(
            UnifiedError::bad_request(
                "Query parameter \"by\" must be \"module\", \"user\", \"company\", or \"bucket\"",
            ),
            debug,
        );
    }
    let limit: usize = params
        .get("limit")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(10)
        .clamp(1, 100);
    let mut totals: HashMap<String, (i64, i64)> = HashMap::new();
    for entry in state.objects.iter() {
        let object = entry.value();
        let key = match dimension {
            "user" => object.owner_user.clone(),
            "company" => object.company_id.clone().unwrap_or_default(),
            "bucket" => object.bucket_id.to_string(),
            _ => object.owner_module.clone(),
        };
        let slot = totals.entry(key).or_default();
        slot.0 += object.size_bytes;
        slot.1 += 1;
    }
    let mut list: Vec<Value> = totals
        .into_iter()
        .map(|(key, (bytes, objects))| {
            let mut item = serde_json::Map::new();
            item.insert(dimension.to_string(), Value::String(key));
            item.insert("bytes".to_string(), json!(bytes));
            item.insert("objects".to_string(), json!(objects));
            Value::Object(item)
        })
        .collect();
    list.sort_by(|a, b| {
        b.get("bytes")
            .and_then(Value::as_i64)
            .cmp(&a.get("bytes").and_then(Value::as_i64))
    });
    list.truncate(limit);
    ok(json!({ "by": dimension, "top": list }))
}

// ---------------------------------------------------------------------------
// Lifecycle.
// ---------------------------------------------------------------------------

async fn list_lifecycle(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_READ) {
        return err_response(error, debug);
    }
    let params = params.into_inner();
    let mut items: Vec<LifecycleRule> = state
        .lifecycle_rules
        .iter()
        .map(|entry| entry.value().clone())
        .collect();
    if let Some(bucket) = params.filters.get("bucket_id") {
        items.retain(|rule| rule.bucket_id.to_string() == *bucket);
    }
    if let Some(enabled) = params.filters.get("enabled") {
        let want = enabled == "true";
        items.retain(|rule| rule.enabled == want);
    }
    items.sort_by(|a, b| a.bucket_id.cmp(&b.bucket_id).then(a.created_at.cmp(&b.created_at)));
    ok(paginate(items, &params))
}

async fn create_lifecycle(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateLifecycleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_MANAGE_LIFECYCLE) {
        return err_response(error, debug);
    }
    let result: UnifiedResult<LifecycleRule> = (|| {
        let input = body.into_inner();
        let bucket = state
            .buckets
            .get(&input.bucket_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!(
                    "Storage bucket not found: {}",
                    input.bucket_id
                ))
            })?;
        if input.retention_days < 0 {
            return Err(UnifiedError::bad_request(
                "retention_days must not be negative",
            ));
        }
        let auto_delete = input.auto_delete.unwrap_or(true);
        let archive_transition = input.archive_transition.unwrap_or(false);
        if !auto_delete && !archive_transition {
            return Err(UnifiedError::bad_request(
                "A lifecycle rule must do something: set auto_delete or archive_transition",
            ));
        }
        let archive_prefix = match &input.archive_prefix {
            Some(raw) => valid_prefix(raw)?,
            None => "archive".to_string(),
        };
        let now = Utc::now();
        let rule = LifecycleRule {
            id: Uuid::now_v7(),
            bucket_id: bucket.id,
            retention_days: input.retention_days,
            auto_delete,
            archive_transition,
            archive_prefix,
            enabled: true,
            last_run_at: None,
            last_run_summary: None,
            last_error: None,
            created_at: now,
            updated_at: now,
        };
        state.lifecycle_rules.insert(rule.id, rule.clone());
        Ok(rule)
    })();
    match result {
        Ok(view) => created(view),
        Err(error) => err_response(error, debug),
    }
}

async fn get_lifecycle(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_READ) {
        return err_response(error, debug);
    }
    json::respond(load_rule(&state, &id), debug)
}

async fn update_lifecycle(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    body: web::Json<UpdateLifecycleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_MANAGE_LIFECYCLE) {
        return err_response(error, debug);
    }
    let result: UnifiedResult<LifecycleRule> = (|| {
        let input = body.into_inner();
        let mut rule = load_rule(&state, &id)?;
        if let Some(days) = input.retention_days {
            if days < 0 {
                return Err(UnifiedError::bad_request(
                    "retention_days must not be negative",
                ));
            }
            rule.retention_days = days;
        }
        if let Some(flag) = input.auto_delete {
            rule.auto_delete = flag;
        }
        if let Some(flag) = input.archive_transition {
            rule.archive_transition = flag;
        }
        if let Some(raw) = &input.archive_prefix {
            rule.archive_prefix = valid_prefix(raw)?;
        }
        if let Some(enabled) = input.enabled {
            rule.enabled = enabled;
        }
        if !rule.auto_delete && !rule.archive_transition {
            return Err(UnifiedError::bad_request(
                "A lifecycle rule must do something: set auto_delete or archive_transition",
            ));
        }
        rule.updated_at = Utc::now();
        state.lifecycle_rules.insert(rule.id, rule.clone());
        Ok(rule)
    })();
    json::respond(result, debug)
}

async fn delete_lifecycle(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_MANAGE_LIFECYCLE) {
        return err_response(error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let rule = load_rule(&state, &id)?;
        state.lifecycle_rules.remove(&rule.id);
        Ok(json!({ "lifecycle_rule_id": rule.id, "deleted": true }))
    })();
    json::respond(result, debug)
}

/// Run one lifecycle rule: candidates are recorded objects older
/// than `retention_days`. Auto-delete removes them (provider delete
/// + metadata + quota decrement); archive transition moves them to
/// the archive prefix (download + upload + delete through the
/// provider contract) and marks the record archived. Objects the
/// provider no longer has are cleaned from the metadata instead of
/// failing the whole run. The run never deletes unrecorded objects:
/// the hub only manages what it recorded.
async fn run_one_rule(
    state: &StorageManagerState,
    config: &Arc<RwLock<AppConfig>>,
    rule: &LifecycleRule,
) -> Value {
    let Some(bucket) = state.buckets.get(&rule.bucket_id).map(|entry| entry.value().clone()) else {
        return json!({
            "rule_id": rule.id.to_string(),
            "bucket_id": rule.bucket_id.to_string(),
            "ran": false,
            "error": "Bucket no longer exists",
        });
    };
    let cutoff = Utc::now() - chrono::Duration::days(rule.retention_days);
    let candidates: Vec<StoredObject> = state
        .objects
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|object| object.bucket_id == bucket.id && object.created_at <= cutoff)
        .collect();
    let provider = match provider_for_bucket_with(config, &bucket) {
        Ok(provider) => provider,
        Err(error) => {
            return json!({
                "rule_id": rule.id.to_string(),
                "bucket_id": bucket.id.to_string(),
                "ran": false,
                "error": error.message,
            });
        }
    };
    let mut deleted = 0usize;
    let mut archived = 0usize;
    let mut cleaned = 0usize;
    let mut errors: Vec<String> = Vec::new();
    for object in candidates {
        if rule.archive_transition {
            let archive_key = if rule.archive_prefix.is_empty() {
                object.key.clone()
            } else {
                format!("{}/{}", rule.archive_prefix, object.key)
            };
            match provider.download(&object.key).await {
                Ok(bytes) => {
                    if bytes.len() > MAX_UPLOAD_BYTES {
                        errors.push(format!(
                            "Object \"{}\" exceeds the {} byte move cap; left in place",
                            object.key, MAX_UPLOAD_BYTES
                        ));
                        continue;
                    }
                    match provider.upload(&archive_key, bytes.clone()).await {
                        Ok(_) => match provider.delete(&object.key).await {
                            Ok(()) => {
                                record_deleted(state, bucket.id, &object.key);
                                record_stored(
                                    state,
                                    bucket.id,
                                    &archive_key,
                                    bytes.len() as i64,
                                    Some(sha256_hex(&bytes)),
                                    &object.mime,
                                    &object.owner_module,
                                    &object.owner_user,
                                    object.company_id.as_deref(),
                                    true,
                                );
                                if let Some(mut moved) = state
                                    .objects
                                    .get_mut(&object_key(bucket.id, &archive_key))
                                {
                                    moved.archived = true;
                                    moved.created_at = object.created_at;
                                    moved.updated_at = Utc::now();
                                }
                                state.emit(
                                    events::OBJECT_ARCHIVED,
                                    json!({
                                        "bucket_id": bucket.id.to_string(),
                                        "key": object.key,
                                        "archive_key": archive_key,
                                        "rule_id": rule.id.to_string(),
                                    }),
                                );
                                archived += 1;
                            }
                            Err(error) => errors.push(format!(
                                "Object \"{}\": archive upload succeeded but source delete failed: {}",
                                object.key, error.message
                            )),
                        },
                        Err(error) => errors.push(format!(
                            "Object \"{}\": archive upload failed: {}",
                            object.key, error.message
                        )),
                    }
                }
                Err(error) if error.code == 404 => {
                    // The provider no longer has it: drop the stale
                    // metadata row instead of failing the run.
                    record_deleted(state, bucket.id, &object.key);
                    cleaned += 1;
                }
                Err(error) => errors.push(format!(
                    "Object \"{}\": download failed: {}",
                    object.key, error.message
                )),
            }
        } else if rule.auto_delete {
            match provider.delete(&object.key).await {
                Ok(()) => {
                    record_deleted(state, bucket.id, &object.key);
                    state.emit(
                        events::OBJECT_DELETED,
                        json!({
                            "bucket_id": bucket.id.to_string(),
                            "key": object.key,
                            "rule_id": rule.id.to_string(),
                            "reason": "lifecycle retention expired",
                        }),
                    );
                    deleted += 1;
                }
                Err(error) => errors.push(format!(
                    "Object \"{}\": delete failed: {}",
                    object.key, error.message
                )),
            }
        }
    }
    let summary = format!(
        "Lifecycle rule {} on bucket \"{}\": {} deleted, {} archived, {} stale rows cleaned, {} errors",
        rule.id,
        bucket.name,
        deleted,
        archived,
        cleaned,
        errors.len()
    );
    let now = Utc::now();
    if let Some(mut stored) = state.lifecycle_rules.get_mut(&rule.id) {
        stored.last_run_at = Some(now);
        stored.last_run_summary = Some(summary.clone());
        stored.last_error = if errors.is_empty() {
            None
        } else {
            Some(errors.join("; "))
        };
        stored.updated_at = now;
    }
    json!({
        "rule_id": rule.id.to_string(),
        "bucket_id": bucket.id.to_string(),
        "ran": true,
        "deleted": deleted,
        "archived": archived,
        "cleaned": cleaned,
        "errors": errors,
        "summary": summary,
    })
}

async fn run_lifecycle(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<RunLifecycleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_MANAGE_LIFECYCLE) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let input = body.into_inner();
    // Ask the scheduler first when requested. Until the
    // `storage.lifecycle.run` kind is registered, the scheduler
    // declines and the run executes in-process below — the decline
    // is reported, never hidden.
    let mut scheduler_answer: Value = Value::Null;
    if input.enqueue.unwrap_or(false) {
        let answer = ctx.hooks.emit(
            events::SCHEDULER_ENQUEUE,
            json!({
                "kind": "storage.lifecycle.run",
                "payload": {
                    "rule_id": input.rule_id.map(|id| id.to_string()),
                    "bucket_id": input.bucket_id.map(|id| id.to_string()),
                    "requested_by": found.actor,
                },
            }),
        );
        scheduler_answer = answer
            .results
            .first()
            .cloned()
            .unwrap_or_else(|| json!({ "queued": false, "error": "No scheduler handler answered scheduler.enqueue" }));
    }
    let rules: Vec<LifecycleRule> = match (input.rule_id, input.bucket_id) {
        (Some(rule_id), _) => match state.lifecycle_rules.get(&rule_id) {
            Some(entry) => vec![entry.value().clone()],
            None => {
                return err_response(
                    UnifiedError::not_found(format!("Lifecycle rule not found: {rule_id}")),
                    debug,
                );
            }
        },
        (None, Some(bucket_id)) => state
            .lifecycle_rules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|rule| rule.bucket_id == bucket_id && rule.enabled)
            .collect(),
        (None, None) => state
            .lifecycle_rules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|rule| rule.enabled)
            .collect(),
    };
    let mut results: Vec<Value> = Vec::new();
    for rule in &rules {
        results.push(run_one_rule(&state, &ctx.config, rule).await);
    }
    state.emit(
        events::LIFECYCLE_RUN_COMPLETED,
        json!({
            "rules_run": results.len(),
            "requested_by": found.actor,
            "scheduler_answer": scheduler_answer,
        }),
    );
    ok(json!({
        "rules_run": results.len(),
        "results": results,
        "scheduler_answer": scheduler_answer,
    }))
}

// ---------------------------------------------------------------------------
// Hook handlers.
// ---------------------------------------------------------------------------

/// Answer `storage.object.upload_requested`: resolve the caller's
/// module to a bucket (module binding -> company default -> global
/// default), check the quotas that apply, and hand back the resolved
/// binding plus the hub upload endpoint. No bytes move here; the
/// caller uploads through `POST /buckets/{id}/objects?key=`.
/// The payload must never contain secrets; the result never does.
fn on_upload_requested(state: StorageManagerState, ctx: &mut HookContext) {
    let module_raw = ctx
        .data
        .get("module")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let owner = ctx
        .data
        .get("owner")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let company_id = ctx
        .data
        .get("company_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let key_hint = ctx
        .data
        .get("key_hint")
        .and_then(|value| value.as_str())
        .unwrap_or("upload")
        .to_string();
    let bytes = ctx
        .data
        .get("bytes")
        .and_then(|value| value.as_u64())
        .unwrap_or(0) as i64;
    let module_id = match valid_module_id(&module_raw) {
        Ok(id) => id,
        Err(error) => {
            ctx.add_result(json!({ "resolved": false, "error": error.message }));
            return;
        }
    };
    let Some((bucket, prefix, source)) =
        resolve_binding(&state, &module_id, company_id.as_deref())
    else {
        ctx.add_result(json!({
            "resolved": false,
            "module": module_id,
            "error": "No storage binding or default bucket resolves for this module",
        }));
        return;
    };
    let key = if prefix.is_empty() {
        key_hint.clone()
    } else {
        format!("{prefix}/{key_hint}")
    };
    if validate_key(&key).is_err() {
        ctx.add_result(json!({
            "resolved": false,
            "module": module_id,
            "error": "The resolved storage key is invalid (check the binding prefix and key hint)",
        }));
        return;
    }
    let check = check_quotas(
        &state,
        &module_id,
        &owner,
        company_id.as_deref(),
        Some(bucket.provider_kind.as_str()),
        bytes,
        1,
    );
    if check.exceeded {
        state.emit(
            events::QUOTA_EXCEEDED,
            json!({
                "scope": check.scope,
                "scope_id": check.scope_id,
                "module": module_id,
                "user_id": owner,
                "company_id": company_id,
                "bucket_id": bucket.id.to_string(),
                "key": key,
                "bytes": bytes,
                "detail": check.detail,
            }),
        );
        ctx.add_result(json!({
            "resolved": false,
            "module": module_id,
            "bucket_id": bucket.id.to_string(),
            "quota_exceeded": true,
            "error": check.detail,
        }));
        return;
    }
    if check.warning {
        state.emit(
            events::QUOTA_WARNING,
            json!({
                "scope": check.scope,
                "scope_id": check.scope_id,
                "module": module_id,
                "user_id": owner,
                "company_id": company_id,
                "bucket_id": bucket.id.to_string(),
                "key": key,
                "bytes": bytes,
                "detail": check.detail,
            }),
        );
    }
    ctx.add_result(json!({
        "resolved": true,
        "module": module_id,
        "source": source,
        "bucket_id": bucket.id.to_string(),
        "bucket_name": bucket.name,
        "provider_kind": bucket.provider_kind.as_str(),
        "key": key,
        "prefix": prefix,
        "upload_endpoint": format!("/api/v1/storage-manager/buckets/{}/objects?key={}", bucket.id, key),
        "quota_ok": true,
        "quota_warning": check.warning,
        "quota_warning_detail": if check.warning { Some(check.detail) } else { None },
    }));
}

/// Answer `storage.lifecycle.run_requested`: the event the scheduler
/// emits when a `storage.lifecycle.run` job fires. Hook handlers are
/// synchronous, so the run is spawned on the async runtime (the
/// config handle captured at `init` supplies what provider building
/// needs) and the handler answers immediately with
/// `{"accepted": true, "queued": true}`. A stale rule id is
/// answered, not ignored silently. When `init` never ran (no config
/// handle), the handler answers `accepted: false` instead of
/// pretending the run happened.
fn on_lifecycle_run_requested(state: StorageManagerState, ctx: &mut HookContext) {
    let rule_id = ctx
        .data
        .get("rule_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok());
    let bucket_id = ctx
        .data
        .get("bucket_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok());
    let rules: Vec<LifecycleRule> = match (rule_id, bucket_id) {
        (Some(id), _) => match state.lifecycle_rules.get(&id) {
            Some(entry) => vec![entry.value().clone()],
            None => {
                ctx.add_result(json!({
                    "accepted": false,
                    "error": format!("Lifecycle rule not found: {id}"),
                }));
                return;
            }
        },
        (None, Some(bucket)) => state
            .lifecycle_rules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|rule| rule.bucket_id == bucket && rule.enabled)
            .collect(),
        (None, None) => state
            .lifecycle_rules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|rule| rule.enabled)
            .collect(),
    };
    let Some(config) = state
        .app_config
        .read()
        .expect("app config lock poisoned")
        .clone()
    else {
        ctx.add_result(json!({
            "accepted": false,
            "rules_matched": rules.len(),
            "error": "Storage Manager init has not run, so no config handle is available to build providers",
        }));
        return;
    };
    let run_state = state.clone();
    tokio::spawn(async move {
        let mut results: Vec<Value> = Vec::new();
        for rule in &rules {
            results.push(run_one_rule(&run_state, &config, rule).await);
        }
        run_state.emit(
            events::LIFECYCLE_RUN_COMPLETED,
            json!({
                "rules_run": results.len(),
                "trigger": "storage.lifecycle.run_requested",
                "results": results,
            }),
        );
    });
    ctx.add_result(json!({
        "accepted": true,
        "queued": true,
        "rules_matched": rules.len(),
    }));
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
// Storage plans (storage as a product) + bring-your-own storage (BYO).
//
// Plans are products: the price itself is owned by Product
// Management / billing, so a plan stores only a `product_id`
// billing reference and a billing-cycle hint — never a money
// column. A buyer (company and/or user) holds at most one active
// assignment. Changing a plan re-checks current usage against the
// new quota and refuses with 409 when usage exceeds it. Usage is
// metered from the recorded hub objects plus the BYO objects and
// reported with `storage.metering.reported` for billing.
//
// BYO connections let a buyer connect their own external storage.
// Only provider kinds on the admin-managed allowlist may connect;
// every connection is validated (connect, list a test prefix,
// write + read + delete a probe object, then remove it) before it
// can activate; the owner must accept the current terms version;
// and every object operation is scoped to the connection prefix,
// checked against its allowed operations, file-type allowlists, and
// quotas, with ownership checked on every access (cross-owner
// access answers 404, never 403, so existence does not leak).
// Credentials follow the bucket pattern exactly: write-only,
// encrypted with the same versioned `v1.` construction, never
// returned, logged, or placed in a hook payload.
// ---------------------------------------------------------------------------

/// Canonical provider-kind names accepted by plans and the BYO
/// allowlist. `drive` is the Google Drive kind; `s3-compatible` is
/// the S3 kind with a custom endpoint (an endpoint is required).
const BYO_KIND_NAMES: &[&str] = &["local", "s3", "r2", "drive", "sftp", "s3-compatible"];

/// Billing-cycle hints a plan may carry. The cycle itself is
/// interpreted by billing; this module only records the hint.
const BILLING_CYCLES: &[&str] = &["monthly", "yearly", "one_time", "usage_based"];

/// Module id used for the licensing entitlement gate.
const STORAGE_MODULE_ID: &str = "storage_manager";

fn canonical_provider_kind_name(name: &str) -> UnifiedResult<String> {
    let normalized = name.trim().to_ascii_lowercase();
    if BYO_KIND_NAMES.contains(&normalized.as_str()) {
        Ok(normalized)
    } else {
        Err(UnifiedError::bad_request(format!(
            "Unknown provider kind \"{name}\" (expected one of: {})",
            BYO_KIND_NAMES.join(", ")
        )))
    }
}

/// Map a canonical allowlist name to the `StorageKind` it means.
fn storage_kind_for_name(name: &str) -> UnifiedResult<StorageKind> {
    match name {
        "local" => Ok(StorageKind::Local),
        "s3" | "s3-compatible" => Ok(StorageKind::S3),
        "r2" => Ok(StorageKind::R2),
        "drive" => Ok(StorageKind::GoogleDrive),
        "sftp" => Ok(StorageKind::Sftp),
        other => Err(UnifiedError::bad_request(format!(
            "Unknown provider kind \"{other}\""
        ))),
    }
}

fn validate_provider_kind_list(values: &[String]) -> UnifiedResult<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for value in values {
        let canonical = canonical_provider_kind_name(value)?;
        if !out.contains(&canonical) {
            out.push(canonical);
        }
    }
    if out.is_empty() {
        return Err(UnifiedError::bad_request(
            "At least one included provider kind is required",
        ));
    }
    Ok(out)
}

fn validate_billing_cycle(value: &Option<String>) -> UnifiedResult<Option<String>> {
    match value {
        None => Ok(None),
        Some(raw) => {
            let normalized = raw.trim().to_ascii_lowercase();
            if BILLING_CYCLES.contains(&normalized.as_str()) {
                Ok(Some(normalized))
            } else {
                Err(UnifiedError::bad_request(format!(
                    "Unknown billing cycle \"{raw}\" (expected one of: {})",
                    BILLING_CYCLES.join(", ")
                )))
            }
        }
    }
}

/// One storage plan: a sellable tier. Price is owned by Product
/// Management / billing; only the `product_id` reference and the
/// billing-cycle hint live here. `max_files` 0 means unlimited.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoragePlan {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    /// Quota in bytes (GB is a display unit only; see `PlanView`).
    pub quota_bytes: i64,
    /// Maximum file count; 0 means unlimited.
    pub max_files: i64,
    /// Canonical provider-kind names included in this plan.
    pub included_provider_kinds: Vec<String>,
    /// Billing reference owned by Product Management / billing.
    pub product_id: Option<String>,
    /// Billing-cycle hint (`monthly`, `yearly`, `one_time`,
    /// `usage_based`); interpreted by billing, not by this module.
    pub billing_cycle: Option<String>,
    pub is_public: bool,
    pub sort_order: i64,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Public plan view: the quota is shown in bytes and in GB.
#[derive(Debug, Clone, Serialize)]
pub struct PlanView {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub quota_bytes: i64,
    pub quota_gb: f64,
    pub max_files: i64,
    pub included_provider_kinds: Vec<String>,
    pub product_id: Option<String>,
    pub billing_cycle: Option<String>,
    pub is_public: bool,
    pub sort_order: i64,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<&StoragePlan> for PlanView {
    fn from(plan: &StoragePlan) -> Self {
        Self {
            id: plan.id,
            name: plan.name.clone(),
            description: plan.description.clone(),
            quota_bytes: plan.quota_bytes,
            quota_gb: plan.quota_bytes as f64 / 1_073_741_824.0,
            max_files: plan.max_files,
            included_provider_kinds: plan.included_provider_kinds.clone(),
            product_id: plan.product_id.clone(),
            billing_cycle: plan.billing_cycle.clone(),
            is_public: plan.is_public,
            sort_order: plan.sort_order,
            enabled: plan.enabled,
            created_at: plan.created_at,
            updated_at: plan.updated_at,
        }
    }
}

/// One buyer plan assignment. A buyer is a company, a user, or
/// both; at least one of the two is always set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanAssignment {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub company_id: Option<String>,
    pub user_id: String,
    /// `active`, `cancelled`, or `expired`.
    pub status: String,
    pub assigned_by: String,
    pub assigned_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub cancel_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One metering report: bytes + files stored by one buyer in one
/// period, reported for billing. `period` is `YYYY-MM`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeteringReport {
    pub id: Uuid,
    pub company_id: Option<String>,
    pub user_id: String,
    pub plan_id: Option<Uuid>,
    pub period: String,
    pub bytes_stored: i64,
    pub files_stored: i64,
    pub reported_by: String,
    pub reported_at: DateTime<Utc>,
}

/// Admin-managed BYO settings: the provider allowlist and limits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ByoSettings {
    /// Canonical provider-kind names an owner may connect.
    pub allowed_provider_kinds: Vec<String>,
    /// When true, a connection cannot activate until its owner has
    /// accepted the current terms version.
    pub terms_required: bool,
    /// Maximum active connections per user; 0 means unlimited.
    pub max_connections_per_user: i64,
}

impl Default for ByoSettings {
    fn default() -> Self {
        Self {
            allowed_provider_kinds: BYO_KIND_NAMES
                .iter()
                .map(|name| (*name).to_string())
                .collect(),
            terms_required: true,
            max_connections_per_user: 0,
        }
    }
}

/// One bring-your-own storage connection: a buyer's own external
/// storage, used as a backend under admin rules and terms.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ByoConnection {
    pub id: Uuid,
    pub name: String,
    pub provider_kind: StorageKind,
    /// True when the kind was connected as `s3-compatible`
    /// (S3 kind with a custom endpoint); recorded so the view and
    /// the audit trail keep the distinction.
    pub s3_compatible: bool,
    pub endpoint: String,
    pub region: String,
    pub container: String,
    pub path: String,
    pub sftp_host: String,
    pub sftp_port: u16,
    pub sftp_username: String,
    pub sftp_auth_kind: String,
    pub sftp_keyfile: String,
    pub allowed_hosts: Vec<String>,
    /// Encrypted credential JSON. Never serialized.
    #[serde(skip_serializing)]
    pub credentials_encrypted: Option<String>,
    pub key_fingerprint: Option<String>,
    /// Path prefix inside the target: the connection may ONLY touch
    /// keys under this prefix. Never empty.
    pub path_prefix: String,
    pub owner_user: String,
    pub company_id: Option<String>,
    /// `draft`, `validated`, `active`, `suspended`, `revoked`.
    pub status: String,
    pub status_reason: Option<String>,
    /// Allowed operations through this connection.
    pub allow_read: bool,
    pub allow_write: bool,
    pub allow_delete: bool,
    /// Per-connection quotas; `None` means unlimited.
    pub max_bytes: Option<i64>,
    pub max_files: Option<i64>,
    pub used_bytes: i64,
    pub used_files: i64,
    /// Encryption-at-rest requirement flag. Same honest matrix as
    /// buckets: recorded, not enforced, for S3 / R2 / Drive / SFTP;
    /// see the module header.
    pub encryption_required: bool,
    /// Allowed file extensions (lowercase, no dot) and MIME types.
    /// Empty means no restriction on that dimension.
    pub allowed_extensions: Vec<String>,
    pub allowed_mime_types: Vec<String>,
    /// Terms version the owner accepted for this connection.
    pub terms_version: Option<String>,
    pub terms_accepted_at: Option<DateTime<Utc>>,
    pub last_validated_at: Option<DateTime<Utc>>,
    pub last_validation_ok: Option<bool>,
    pub last_validation_error: Option<String>,
    pub last_health_at: Option<DateTime<Utc>>,
    pub last_health_ok: Option<bool>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Public BYO view: credentials are never included, only whether
/// any are configured.
#[derive(Debug, Clone, Serialize)]
pub struct ByoConnectionView {
    pub id: Uuid,
    pub name: String,
    pub provider_kind: StorageKind,
    pub s3_compatible: bool,
    pub endpoint: String,
    pub region: String,
    pub container: String,
    pub path: String,
    pub sftp_host: String,
    pub sftp_port: u16,
    pub sftp_username: String,
    pub sftp_auth_kind: String,
    pub sftp_keyfile: String,
    pub allowed_hosts: Vec<String>,
    pub credentials_configured: bool,
    pub key_fingerprint: Option<String>,
    pub path_prefix: String,
    pub owner_user: String,
    pub company_id: Option<String>,
    pub status: String,
    pub status_reason: Option<String>,
    pub allow_read: bool,
    pub allow_write: bool,
    pub allow_delete: bool,
    pub max_bytes: Option<i64>,
    pub max_files: Option<i64>,
    pub used_bytes: i64,
    pub used_files: i64,
    pub encryption_required: bool,
    pub encryption_required_note: String,
    pub allowed_extensions: Vec<String>,
    pub allowed_mime_types: Vec<String>,
    pub terms_version: Option<String>,
    pub terms_accepted_at: Option<DateTime<Utc>>,
    pub last_validated_at: Option<DateTime<Utc>>,
    pub last_validation_ok: Option<bool>,
    pub last_validation_error: Option<String>,
    pub last_health_at: Option<DateTime<Utc>>,
    pub last_health_ok: Option<bool>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<&ByoConnection> for ByoConnectionView {
    fn from(connection: &ByoConnection) -> Self {
        Self {
            id: connection.id,
            name: connection.name.clone(),
            provider_kind: connection.provider_kind,
            s3_compatible: connection.s3_compatible,
            endpoint: connection.endpoint.clone(),
            region: connection.region.clone(),
            container: connection.container.clone(),
            path: connection.path.clone(),
            sftp_host: connection.sftp_host.clone(),
            sftp_port: connection.sftp_port,
            sftp_username: connection.sftp_username.clone(),
            sftp_auth_kind: connection.sftp_auth_kind.clone(),
            sftp_keyfile: connection.sftp_keyfile.clone(),
            allowed_hosts: connection.allowed_hosts.clone(),
            credentials_configured: connection.credentials_encrypted.is_some(),
            key_fingerprint: connection.key_fingerprint.clone(),
            path_prefix: connection.path_prefix.clone(),
            owner_user: connection.owner_user.clone(),
            company_id: connection.company_id.clone(),
            status: connection.status.clone(),
            status_reason: connection.status_reason.clone(),
            allow_read: connection.allow_read,
            allow_write: connection.allow_write,
            allow_delete: connection.allow_delete,
            max_bytes: connection.max_bytes,
            max_files: connection.max_files,
            used_bytes: connection.used_bytes,
            used_files: connection.used_files,
            encryption_required: connection.encryption_required,
            encryption_required_note: encryption_note(connection.provider_kind),
            allowed_extensions: connection.allowed_extensions.clone(),
            allowed_mime_types: connection.allowed_mime_types.clone(),
            terms_version: connection.terms_version.clone(),
            terms_accepted_at: connection.terms_accepted_at,
            last_validated_at: connection.last_validated_at,
            last_validation_ok: connection.last_validation_ok,
            last_validation_error: connection.last_validation_error,
            last_health_at: connection.last_health_at,
            last_health_ok: connection.last_health_ok,
            created_at: connection.created_at,
            updated_at: connection.updated_at,
        }
    }
}

/// One object recorded through a BYO connection. The full key
/// (prefix + relative key) is what the provider stores.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ByoObject {
    pub connection_id: Uuid,
    pub key: String,
    pub size_bytes: i64,
    /// SHA-256 hex digest of the bytes, recorded at upload time.
    /// `None` for objects recorded before checksums were kept.
    /// Downloads verify against it when present.
    pub sha256_hex: Option<String>,
    pub mime: String,
    pub owner_user: String,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// One version of the BYO terms of use. Only one version is
/// current at a time; changing the current version requires
/// re-acceptance (see `enforce_byo_reacceptance`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ByoTerms {
    pub id: Uuid,
    /// Version label, for example `1.0`. Unique among versions.
    pub version: String,
    pub title: String,
    /// Full terms text: data residency, liability limits, and the
    /// admin's right to suspend a misbehaving connection.
    pub body: String,
    pub is_current: bool,
    /// After this instant, connections whose owner has not accepted
    /// this version are suspended by the enforcement run.
    pub reacceptance_deadline: Option<DateTime<Utc>>,
    pub enabled: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One recorded acceptance of a BYO terms version by one user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ByoTermsAcceptance {
    pub id: Uuid,
    pub terms_id: Uuid,
    pub terms_version: String,
    pub user_id: String,
    pub company_id: Option<String>,
    pub connection_id: Option<Uuid>,
    pub accepted_at: DateTime<Utc>,
}

/// One BYO health-check run for one connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ByoHealthCheck {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub ok: bool,
    pub latency_ms: i64,
    /// Short outcome text. Never contains credentials or provider
    /// error bodies.
    pub detail: String,
    pub checked_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Plan helpers: loaders, entitlement, usage, metering.
// ---------------------------------------------------------------------------

fn load_plan(state: &StorageManagerState, raw_id: &str) -> UnifiedResult<StoragePlan> {
    let id = parse_uuid(raw_id, "storage plan")?;
    state
        .plans
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Storage plan not found: {id}")))
}

fn assignment_key(company_id: Option<&str>, user_id: &str) -> String {
    format!("{}|{user_id}", company_id.unwrap_or(""))
}

fn load_assignment(
    state: &StorageManagerState,
    raw_id: &str,
) -> UnifiedResult<PlanAssignment> {
    let id = parse_uuid(raw_id, "plan assignment")?;
    state
        .plan_assignments
        .iter()
        .find(|entry| entry.value().id == id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Plan assignment not found: {id}")))
}

/// Bytes + files one buyer currently stores: recorded hub objects
/// plus BYO objects. A company-scoped buyer counts the whole
/// company; a personal buyer counts only their company-less
/// objects. Used for plan-change quota checks and metering.
fn buyer_usage(
    state: &StorageManagerState,
    company_id: Option<&str>,
    user_id: &str,
) -> (i64, i64) {
    let mut bytes: i64 = 0;
    let mut files: i64 = 0;
    for entry in state.objects.iter() {
        let object = entry.value();
        let matches = match company_id {
            Some(company) => object.company_id.as_deref() == Some(company),
            None => object.owner_user == user_id && object.company_id.is_none(),
        };
        if matches {
            bytes += object.size_bytes;
            files += 1;
        }
    }
    for entry in state.byo_objects.iter() {
        let object = entry.value();
        let matches = match company_id {
            Some(company) => object.company_id.as_deref() == Some(company),
            None => object.owner_user == user_id && object.company_id.is_none(),
        };
        if matches {
            bytes += object.size_bytes;
            files += 1;
        }
    }
    (bytes, files)
}

/// Emit one metering report event for billing. The caller also
/// stores the report row; the event is the billing handoff (hook
/// bus only — this module never calls billing directly).
fn emit_metering(
    state: &StorageManagerState,
    report: &MeteringReport,
    source: &str,
) {
    state.emit(
        events::METERING_REPORTED,
        json!({
            "report_id": report.id.to_string(),
            "company_id": report.company_id,
            "user_id": report.user_id,
            "plan_id": report.plan_id.map(|id| id.to_string()),
            "period": report.period,
            "bytes_stored": report.bytes_stored,
            "files_stored": report.files_stored,
            "source": source,
            "reported_at": report.reported_at.to_rfc3339(),
        }),
    );
}

/// Gate a plan-gated action on the Storage Manager module
/// entitlement for one company, through the licensing module.
///
/// When the module is registered (the integrator step), the
/// canonical registry gate `licensing::check_module_entitlement`
/// answers, including its granted / denied events. Before
/// registration the registry lookup would answer 404 for a module
/// that does exist, so the same license match is checked directly
/// with `licensing::has_module_entitlement` instead. Both paths
/// fail closed: no active license means no plan-gated use.
fn require_storage_module_entitlement(
    ctx: &AppContext,
    company_id: &str,
) -> UnifiedResult<()> {
    if ctx.modules.get(STORAGE_MODULE_ID).is_some() {
        return licensing::check_module_entitlement(ctx, company_id, STORAGE_MODULE_ID);
    }
    if licensing::has_module_entitlement(ctx, company_id, STORAGE_MODULE_ID) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Module \"{STORAGE_MODULE_ID}\" requires an active license entitlement for this company"
        ))
        .with_metadata("module_id", serde_json::json!(STORAGE_MODULE_ID))
        .with_metadata("company_id", serde_json::json!(company_id)))
    }
}

/// Gate on one plan's billing product entitlement, through the
/// `product.entitlement.check` hook — the same contract the
/// meetings module uses for paid rooms: the first hook result must
/// carry `{"entitled": true}`. No answer fails closed with 503;
/// an explicit error answers 502; anything else answers 403.
/// Plans without a `product_id` need no product check.
fn check_plan_product_entitlement(
    ctx: &AppContext,
    plan: &StoragePlan,
    user_id: &str,
    company_id: Option<&str>,
) -> UnifiedResult<()> {
    let Some(product_id) = &plan.product_id else {
        return Ok(());
    };
    let answer = ctx.hooks.emit(
        "product.entitlement.check",
        json!({
            "product_id": product_id,
            "user_id": user_id,
            "company_id": company_id,
        }),
    );
    match answer.results.first() {
        Some(result) if result.get("entitled").and_then(Value::as_bool) == Some(true) => Ok(()),
        Some(result) if result.get("error").is_some() => {
            let reason = result
                .get("error")
                .and_then(|value| value.as_str())
                .unwrap_or("entitlement check failed");
            Err(UnifiedError::new(
                format!("Entitlement check failed: {reason}"),
                502,
            ))
        }
        Some(_) => Err(UnifiedError::forbidden(format!(
            "A paid product entitlement for \"{product_id}\" is required to use storage plan \"{}\"",
            plan.name
        ))),
        None => Err(UnifiedError::new(
            "Entitlement service did not answer product.entitlement.check: no handler is registered",
            503,
        )),
    }
}

/// Refuse a plan change when current usage exceeds the new plan's
/// quota. Returns the 409 error to surface, or `Ok(())`.
fn check_plan_change_quota(
    state: &StorageManagerState,
    plan: &StoragePlan,
    company_id: Option<&str>,
    user_id: &str,
) -> UnifiedResult<()> {
    let (bytes, files) = buyer_usage(state, company_id, user_id);
    if bytes > plan.quota_bytes {
        state.emit(
            events::PLAN_QUOTA_REFUSED,
            json!({
                "plan_id": plan.id.to_string(),
                "plan_name": plan.name,
                "company_id": company_id,
                "user_id": user_id,
                "bytes_stored": bytes,
                "quota_bytes": plan.quota_bytes,
                "reason": "bytes exceed the new plan quota",
            }),
        );
        return Err(UnifiedError::conflict(format!(
            "Cannot change to plan \"{}\": {} bytes are stored, which exceeds the plan quota of {} bytes. Delete data or choose a larger plan first.",
            plan.name, bytes, plan.quota_bytes
        )));
    }
    if plan.max_files > 0 && files > plan.max_files {
        state.emit(
            events::PLAN_QUOTA_REFUSED,
            json!({
                "plan_id": plan.id.to_string(),
                "plan_name": plan.name,
                "company_id": company_id,
                "user_id": user_id,
                "files_stored": files,
                "max_files": plan.max_files,
                "reason": "file count exceeds the new plan quota",
            }),
        );
        return Err(UnifiedError::conflict(format!(
            "Cannot change to plan \"{}\": {} files are stored, which exceeds the plan limit of {} files. Delete files or choose a larger plan first.",
            plan.name, files, plan.max_files
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Plan request bodies.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CreatePlanRequest {
    name: String,
    #[serde(default)]
    description: Option<String>,
    quota_bytes: i64,
    #[serde(default)]
    max_files: Option<i64>,
    included_provider_kinds: Vec<String>,
    #[serde(default)]
    product_id: Option<String>,
    #[serde(default)]
    billing_cycle: Option<String>,
    #[serde(default)]
    is_public: Option<bool>,
    #[serde(default)]
    sort_order: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct UpdatePlanRequest {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    quota_bytes: Option<i64>,
    #[serde(default)]
    max_files: Option<i64>,
    #[serde(default)]
    included_provider_kinds: Option<Vec<String>>,
    #[serde(default)]
    product_id: Option<Option<String>>,
    #[serde(default)]
    billing_cycle: Option<Option<String>>,
    #[serde(default)]
    is_public: Option<bool>,
    #[serde(default)]
    sort_order: Option<i64>,
    #[serde(default)]
    enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct AssignPlanRequest {
    /// Company the plan is assigned to (optional for personal plans).
    #[serde(default)]
    company_id: Option<String>,
    /// User the plan is assigned to (required: every assignment has
    /// an owning user, even a company-scoped one).
    user_id: String,
    #[serde(default)]
    expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
struct CancelAssignmentRequest {
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ReportMeteringRequest {
    #[serde(default)]
    company_id: Option<String>,
    user_id: String,
    /// `YYYY-MM`. Defaults to the current month.
    #[serde(default)]
    period: Option<String>,
}

// ---------------------------------------------------------------------------
// Plans.
// ---------------------------------------------------------------------------

async fn list_plans(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_MANAGE_PLANS) {
        return err_response(error, debug);
    }
    let params = params.into_inner();
    let mut items: Vec<PlanView> = state
        .plans
        .iter()
        .map(|entry| PlanView::from(entry.value()))
        .collect();
    if let Some(filter) = params.filters.get("is_public") {
        let want = filter == "true";
        items.retain(|plan| plan.is_public == want);
    }
    if let Some(filter) = params.filters.get("enabled") {
        let want = filter == "true";
        items.retain(|plan| plan.enabled == want);
    }
    if let Some(search) = &params.search {
        let needle = search.to_lowercase();
        items.retain(|plan| {
            plan.name.to_lowercase().contains(&needle)
                || plan.description.to_lowercase().contains(&needle)
        });
    }
    items.sort_by(|a, b| {
        a.sort_order
            .cmp(&b.sort_order)
            .then_with(|| a.name.cmp(&b.name))
    });
    ok(paginate(items, &params))
}

async fn list_public_plans(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_READ) {
        return err_response(error, debug);
    }
    let params = params.into_inner();
    let mut items: Vec<PlanView> = state
        .plans
        .iter()
        .map(|entry| PlanView::from(entry.value()))
        .filter(|plan| plan.is_public && plan.enabled)
        .collect();
    if let Some(search) = &params.search {
        let needle = search.to_lowercase();
        items.retain(|plan| {
            plan.name.to_lowercase().contains(&needle)
                || plan.description.to_lowercase().contains(&needle)
        });
    }
    items.sort_by(|a, b| {
        a.sort_order
            .cmp(&b.sort_order)
            .then_with(|| a.name.cmp(&b.name))
    });
    ok(paginate(items, &params))
}

async fn create_plan(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreatePlanRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_MANAGE_PLANS) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<PlanView> = (|| {
        let input = body.into_inner();
        let name = require_text(&input.name, "name", 128)?;
        if state
            .plans
            .iter()
            .any(|entry| entry.value().name == name)
        {
            return Err(UnifiedError::conflict(format!(
                "Storage plan \"{name}\" already exists"
            )));
        }
        let description = optional_text(&input.description, "description", 2048)?
            .unwrap_or_default();
        if input.quota_bytes < 0 {
            return Err(UnifiedError::bad_request(
                "quota_bytes must not be negative",
            ));
        }
        let max_files = input.max_files.unwrap_or(0);
        if max_files < 0 {
            return Err(UnifiedError::bad_request(
                "max_files must not be negative (use 0 for unlimited)",
            ));
        }
        let included = validate_provider_kind_list(&input.included_provider_kinds)?;
        let product_id = optional_text(&input.product_id, "product_id", 128)?;
        let billing_cycle = validate_billing_cycle(&input.billing_cycle)?;
        let now = Utc::now();
        let plan = StoragePlan {
            id: Uuid::now_v7(),
            name,
            description,
            quota_bytes: input.quota_bytes,
            max_files,
            included_provider_kinds: included,
            product_id,
            billing_cycle,
            is_public: input.is_public.unwrap_or(false),
            sort_order: input.sort_order.unwrap_or(0),
            enabled: true,
            created_at: now,
            updated_at: now,
        };
        state.plans.insert(plan.id, plan.clone());
        state.emit(
            events::PLAN_CREATED,
            json!({
                "plan_id": plan.id.to_string(),
                "name": plan.name,
                "quota_bytes": plan.quota_bytes,
                "product_id": plan.product_id,
                "created_by": found.actor,
            }),
        );
        Ok(PlanView::from(&plan))
    })();
    match result {
        Ok(view) => created(view),
        Err(error) => err_response(error, debug),
    }
}

async fn get_plan(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_READ) {
        return err_response(error, debug);
    }
    json::respond(load_plan(&state, &id).map(|plan| PlanView::from(&plan)), debug)
}

async fn update_plan(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    body: web::Json<UpdatePlanRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_MANAGE_PLANS) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<PlanView> = (|| {
        let input = body.into_inner();
        let mut plan = load_plan(&state, &id)?;
        if let Some(name) = &input.name {
            let name = require_text(name, "name", 128)?;
            if state
                .plans
                .iter()
                .any(|entry| entry.value().id != plan.id && entry.value().name == name)
            {
                return Err(UnifiedError::conflict(format!(
                    "Storage plan \"{name}\" already exists"
                )));
            }
            plan.name = name;
        }
        if let Some(description) = &input.description {
            plan.description = require_text(description, "description", 2048)?;
        }
        if let Some(quota_bytes) = input.quota_bytes {
            if quota_bytes < 0 {
                return Err(UnifiedError::bad_request(
                    "quota_bytes must not be negative",
                ));
            }
            plan.quota_bytes = quota_bytes;
        }
        if let Some(max_files) = input.max_files {
            if max_files < 0 {
                return Err(UnifiedError::bad_request(
                    "max_files must not be negative (use 0 for unlimited)",
                ));
            }
            plan.max_files = max_files;
        }
        if let Some(kinds) = &input.included_provider_kinds {
            plan.included_provider_kinds = validate_provider_kind_list(kinds)?;
        }
        if let Some(product_id) = &input.product_id {
            plan.product_id = match product_id {
                Some(value) => Some(require_text(value, "product_id", 128)?),
                None => None,
            };
        }
        if let Some(cycle) = &input.billing_cycle {
            plan.billing_cycle = match cycle {
                Some(value) => validate_billing_cycle(&Some(value.clone()))?,
                None => None,
            };
        }
        if let Some(is_public) = input.is_public {
            plan.is_public = is_public;
        }
        if let Some(sort_order) = input.sort_order {
            plan.sort_order = sort_order;
        }
        if let Some(enabled) = input.enabled {
            plan.enabled = enabled;
        }
        plan.updated_at = Utc::now();
        state.plans.insert(plan.id, plan.clone());
        state.emit(
            events::PLAN_UPDATED,
            json!({
                "plan_id": plan.id.to_string(),
                "name": plan.name,
                "updated_by": found.actor,
            }),
        );
        Ok(PlanView::from(&plan))
    })();
    json::respond(result, debug)
}

async fn delete_plan(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_MANAGE_PLANS) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<Value> = (|| {
        let plan = load_plan(&state, &id)?;
        if state
            .plan_assignments
            .iter()
            .any(|entry| entry.value().plan_id == plan.id && entry.value().status == "active")
        {
            return Err(UnifiedError::conflict(format!(
                "Storage plan \"{}\" cannot be deleted while buyers hold an active assignment to it",
                plan.name
            )));
        }
        state.plans.remove(&plan.id);
        state.emit(
            events::PLAN_DELETED,
            json!({
                "plan_id": plan.id.to_string(),
                "name": plan.name,
                "deleted_by": found.actor,
            }),
        );
        Ok(json!({ "deleted": true, "plan_id": plan.id }))
    })();
    json::respond(result, debug)
}

/// Assign a plan to a buyer, or change the buyer's current plan.
/// Changing a plan re-checks current usage against the new quota
/// and refuses with 409 when usage exceeds it. A company-scoped
/// assignment requires the module entitlement for that company;
/// a plan with a `product_id` additionally requires the product
/// entitlement for the buyer.
async fn assign_plan(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    body: web::Json<AssignPlanRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_MANAGE_PLANS) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<PlanAssignment> = (|| {
        let input = body.into_inner();
        let plan = load_plan(&state, &id)?;
        if !plan.enabled {
            return Err(UnifiedError::conflict(format!(
                "Storage plan \"{}\" is disabled and cannot be assigned",
                plan.name
            )));
        }
        let company_id = optional_text(&input.company_id, "company_id", 128)?;
        let user_id = valid_scope_id(&input.user_id)?;
        if let Some(expires_at) = input.expires_at {
            if expires_at <= Utc::now() {
                return Err(UnifiedError::bad_request(
                    "expires_at must be in the future",
                ));
            }
        }
        if let Some(company) = &company_id {
            require_storage_module_entitlement(&ctx, company)?;
        }
        check_plan_product_entitlement(&ctx, &plan, &user_id, company_id.as_deref())?;
        // The quota re-check runs before any mutation (and before
        // any map guard is taken), so a refusal leaves the old
        // plan in place.
        check_plan_change_quota(&state, &plan, company_id.as_deref(), &user_id)?;
        let key = assignment_key(company_id.as_deref(), &user_id);
        let now = Utc::now();
        match state.plan_assignments.get_mut(&key) {
            Some(mut existing) if existing.status == "active" => {
                let old_plan_id = existing.plan_id;
                existing.plan_id = plan.id;
                existing.expires_at = input.expires_at;
                existing.assigned_by = found.actor.clone();
                existing.assigned_at = now;
                existing.updated_at = now;
                let assignment = (*existing).clone();
                drop(existing);
                state.emit(
                    events::PLAN_CHANGED,
                    json!({
                        "assignment_id": assignment.id.to_string(),
                        "old_plan_id": old_plan_id.to_string(),
                        "plan_id": plan.id.to_string(),
                        "company_id": company_id,
                        "user_id": user_id,
                        "changed_by": found.actor,
                    }),
                );
                Ok(assignment)
            }
            _ => {
                let assignment = PlanAssignment {
                    id: Uuid::now_v7(),
                    plan_id: plan.id,
                    company_id: company_id.clone(),
                    user_id: user_id.clone(),
                    status: "active".to_string(),
                    assigned_by: found.actor.clone(),
                    assigned_at: now,
                    expires_at: input.expires_at,
                    cancelled_at: None,
                    cancel_reason: None,
                    created_at: now,
                    updated_at: now,
                };
                state
                    .plan_assignments
                    .insert(key, assignment.clone());
                state.emit(
                    events::PLAN_ASSIGNED,
                    json!({
                        "assignment_id": assignment.id.to_string(),
                        "plan_id": plan.id.to_string(),
                        "company_id": company_id,
                        "user_id": user_id,
                        "assigned_by": found.actor,
                    }),
                );
                Ok(assignment)
            }
        }
    })();
    match result {
        Ok(assignment) => created(assignment),
        Err(error) => err_response(error, debug),
    }
}

async fn list_plan_assignments(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_MANAGE_PLANS) {
        return err_response(error, debug);
    }
    let params = params.into_inner();
    let mut items: Vec<PlanAssignment> = state
        .plan_assignments
        .iter()
        .map(|entry| entry.value().clone())
        .collect();
    if let Some(filter) = params.filters.get("company_id") {
        items.retain(|item| item.company_id.as_deref() == Some(filter.as_str()));
    }
    if let Some(filter) = params.filters.get("user_id") {
        items.retain(|item| item.user_id == *filter);
    }
    if let Some(filter) = params.filters.get("plan_id") {
        items.retain(|item| item.plan_id.to_string() == *filter);
    }
    if let Some(filter) = params.filters.get("status") {
        items.retain(|item| item.status == *filter);
    }
    items.sort_by(|a, b| b.assigned_at.cmp(&a.assigned_at));
    ok(paginate(items, &params))
}

async fn get_plan_assignment(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_MANAGE_PLANS) {
        return err_response(error, debug);
    }
    json::respond(load_assignment(&state, &id), debug)
}

async fn cancel_plan_assignment(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    body: web::Json<CancelAssignmentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_MANAGE_PLANS) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<PlanAssignment> = (|| {
        let input = body.into_inner();
        let mut assignment = load_assignment(&state, &id)?;
        if assignment.status != "active" {
            return Err(UnifiedError::conflict(format!(
                "Plan assignment {} is already {} and cannot be cancelled",
                assignment.id, assignment.status
            )));
        }
        assignment.status = "cancelled".to_string();
        assignment.cancelled_at = Some(Utc::now());
        assignment.cancel_reason =
            optional_text(&input.reason, "reason", 512)?;
        assignment.updated_at = Utc::now();
        let key = assignment_key(
            assignment.company_id.as_deref(),
            &assignment.user_id,
        );
        state.plan_assignments.insert(key, assignment.clone());
        state.emit(
            events::PLAN_CANCELLED,
            json!({
                "assignment_id": assignment.id.to_string(),
                "plan_id": assignment.plan_id.to_string(),
                "company_id": assignment.company_id,
                "user_id": assignment.user_id,
                "reason": assignment.cancel_reason,
                "cancelled_by": found.actor,
            }),
        );
        Ok(assignment)
    })();
    json::respond(result, debug)
}

/// A buyer's current plan plus usage against its quota. The buyer
/// is the caller (`X-User-Id`) unless an admin (holding
/// `storage_manager.manage_plans`) asks for another buyer.
async fn get_my_plan(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<HashMap<String, String>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_READ) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = check_capability(&ctx, &found.role, PERM_MANAGE_PLANS);
    let user_id = if is_admin {
        query
            .get("user_id")
            .map(|value| value.to_string())
            .unwrap_or_else(|| found.actor.clone())
    } else {
        found.actor.clone()
    };
    let company_id = if is_admin {
        query.get("company_id").map(|value| value.to_string())
    } else {
        query.get("company_id").map(|value| value.to_string())
    };
    let result: UnifiedResult<Value> = (|| {
        let user_id = valid_scope_id(&user_id)?;
        let company_id = match &company_id {
            Some(value) => Some(valid_scope_id(value)?),
            None => None,
        };
        let key = assignment_key(company_id.as_deref(), &user_id);
        let assignment = state
            .plan_assignments
            .get(&key)
            .map(|entry| entry.value().clone());
        let plan = match &assignment {
            Some(item) if item.status == "active" => state
                .plans
                .get(&item.plan_id)
                .map(|entry| PlanView::from(entry.value())),
            _ => None,
        };
        // An expired assignment reports as expired instead of
        // silently looking like no plan at all.
        let status = match &assignment {
            Some(item) if item.status == "active"
                && item
                    .expires_at
                    .is_some_and(|expires_at| expires_at <= Utc::now()) =>
            {
                "expired"
            }
            Some(item) => item.status.as_str(),
            None => "none",
        };
        let (bytes, files) = buyer_usage(&state, company_id.as_deref(), &user_id);
        let (quota_bytes, max_files) = match &plan {
            Some(view) => (Some(view.quota_bytes), Some(view.max_files)),
            None => (None, None),
        };
        Ok(json!({
            "company_id": company_id,
            "user_id": user_id,
            "assignment": assignment,
            "plan": plan,
            "status": status,
            "usage": {
                "bytes_stored": bytes,
                "files_stored": files,
                "quota_bytes": quota_bytes,
                "max_files": max_files,
                "bytes_remaining": quota_bytes.map(|quota| (quota - bytes).max(0)),
                "files_remaining": max_files.map(|max| if max == 0 { Value::Null } else { Value::from((max - files).max(0)) }).unwrap_or(Value::Null),
                "over_quota": quota_bytes.is_some_and(|quota| bytes > quota)
                    || max_files.is_some_and(|max| max > 0 && files > max),
            },
        }))
    })();
    json::respond(result, debug)
}

/// Record a metering report for one buyer and emit it for billing.
/// Usage is read from the recorded hub objects plus BYO objects,
/// so BYO usage is metered and reported exactly like plan usage.
async fn report_metering(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<ReportMeteringRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_MANAGE_PLANS) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<MeteringReport> = (|| {
        let input = body.into_inner();
        let company_id = optional_text(&input.company_id, "company_id", 128)?;
        let user_id = valid_scope_id(&input.user_id)?;
        let period = match &input.period {
            Some(raw) => {
                let trimmed = raw.trim();
                let valid = trimmed.len() == 7
                    && trimmed.as_bytes().get(4) == Some(&b'-')
                    && trimmed[..4].chars().all(|c| c.is_ascii_digit())
                    && trimmed[5..].chars().all(|c| c.is_ascii_digit())
                    && (1..=12).contains(&trimmed[5..].parse::<u32>().unwrap_or(0));
                if !valid {
                    return Err(UnifiedError::bad_request(
                        "period must be YYYY-MM",
                    ));
                }
                trimmed.to_string()
            }
            None => Utc::now().format("%Y-%m").to_string(),
        };
        let key = assignment_key(company_id.as_deref(), &user_id);
        let plan_id = state
            .plan_assignments
            .get(&key)
            .filter(|entry| entry.value().status == "active")
            .map(|entry| entry.value().plan_id);
        let (bytes, files) = buyer_usage(&state, company_id.as_deref(), &user_id);
        let report = MeteringReport {
            id: Uuid::now_v7(),
            company_id: company_id.clone(),
            user_id: user_id.clone(),
            plan_id,
            period: period.clone(),
            bytes_stored: bytes,
            files_stored: files,
            reported_by: found.actor.clone(),
            reported_at: Utc::now(),
        };
        state
            .metering_reports
            .insert(report.id, report.clone());
        emit_metering(&state, &report, "plan_metering");
        Ok(report)
    })();
    match result {
        Ok(report) => created(report),
        Err(error) => err_response(error, debug),
    }
}

async fn list_metering(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_MANAGE_PLANS) {
        return err_response(error, debug);
    }
    let params = params.into_inner();
    let mut items: Vec<MeteringReport> = state
        .metering_reports
        .iter()
        .map(|entry| entry.value().clone())
        .collect();
    if let Some(filter) = params.filters.get("company_id") {
        items.retain(|item| item.company_id.as_deref() == Some(filter.as_str()));
    }
    if let Some(filter) = params.filters.get("user_id") {
        items.retain(|item| item.user_id == *filter);
    }
    if let Some(filter) = params.filters.get("period") {
        items.retain(|item| item.period == *filter);
    }
    if let Some(filter) = params.filters.get("plan_id") {
        items.retain(|item| item.plan_id.map(|id| id.to_string()) == Some(filter.clone()));
    }
    items.sort_by(|a, b| b.reported_at.cmp(&a.reported_at));
    ok(paginate(items, &params))
}

// ---------------------------------------------------------------------------
// BYO helpers: loaders (404 on cross-owner access), provider build,
// validation probe, health checks, prefix scoping, rule checks.
// ---------------------------------------------------------------------------

fn load_byo_connection(
    state: &StorageManagerState,
    raw_id: &str,
) -> UnifiedResult<ByoConnection> {
    let id = parse_uuid(raw_id, "BYO connection")?;
    state
        .byo_connections
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("BYO connection not found: {id}")))
}

/// Load a BYO connection as one caller may see it. A non-admin
/// caller who does not own the connection gets the same 404 as for
/// a missing connection, so existence never leaks across owners
/// (the per-user 404 pattern the todos module uses).
fn load_byo_connection_for(
    state: &StorageManagerState,
    raw_id: &str,
    actor: &str,
    is_admin: bool,
) -> UnifiedResult<ByoConnection> {
    let connection = load_byo_connection(state, raw_id)?;
    if is_admin || connection.owner_user == actor {
        Ok(connection)
    } else {
        Err(UnifiedError::not_found(format!(
            "BYO connection not found: {raw_id}"
        )))
    }
}

fn load_byo_terms(
    state: &StorageManagerState,
    raw_id: &str,
) -> UnifiedResult<ByoTerms> {
    let id = parse_uuid(raw_id, "BYO terms")?;
    state
        .byo_terms
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("BYO terms not found: {id}")))
}

fn current_byo_terms(state: &StorageManagerState) -> Option<ByoTerms> {
    state
        .byo_terms
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|terms| terms.is_current && terms.enabled)
        .max_by(|a, b| a.created_at.cmp(&b.created_at))
}

fn has_accepted_terms(
    state: &StorageManagerState,
    user_id: &str,
    terms_version: &str,
) -> bool {
    state.byo_acceptances.iter().any(|entry| {
        let acceptance = entry.value();
        acceptance.user_id == user_id && acceptance.terms_version == terms_version
    })
}

fn validate_extensions(values: &Option<Vec<String>>) -> UnifiedResult<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for value in values.clone().unwrap_or_default() {
        let cleaned = value
            .trim()
            .trim_start_matches('.')
            .to_ascii_lowercase();
        if cleaned.is_empty() || cleaned.len() > 32 {
            return Err(UnifiedError::bad_request(
                "Allowed extensions must be 1..=32 characters",
            ));
        }
        if !cleaned
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(UnifiedError::bad_request(
                "Allowed extensions must contain only letters, digits, '-' or '_'",
            ));
        }
        if !out.contains(&cleaned) {
            out.push(cleaned);
        }
    }
    Ok(out)
}

fn validate_mime_types(values: &Option<Vec<String>>) -> UnifiedResult<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for value in values.clone().unwrap_or_default() {
        let cleaned = value.trim().to_ascii_lowercase();
        if cleaned.is_empty() || cleaned.len() > 128 {
            return Err(UnifiedError::bad_request(
                "Allowed MIME types must be 1..=128 characters",
            ));
        }
        if !cleaned.contains('/') {
            return Err(UnifiedError::bad_request(
                "Allowed MIME types must look like \"type/subtype\"",
            ));
        }
        if !out.contains(&cleaned) {
            out.push(cleaned);
        }
    }
    Ok(out)
}

/// Build the provider target config for a BYO connection,
/// decrypting its stored credentials. Same shape as
/// `target_config_for_bucket`; the config lives in memory only.
fn target_config_for_byo(
    config: &RwLock<AppConfig>,
    connection: &ByoConnection,
) -> UnifiedResult<StorageTargetConfig> {
    let mut target = StorageTargetConfig {
        name: format!("storage-manager:byo:{}", connection.id),
        kind: connection.provider_kind,
        endpoint: connection.endpoint.clone(),
        region: connection.region.clone(),
        bucket: connection.container.clone(),
        base_dir: connection.path.clone(),
        sftp_host: connection.sftp_host.clone(),
        sftp_port: connection.sftp_port,
        sftp_username: connection.sftp_username.clone(),
        sftp_auth_kind: connection.sftp_auth_kind.clone(),
        sftp_keyfile: connection.sftp_keyfile.clone(),
        sftp_base_path: connection.path.clone(),
        ..StorageTargetConfig::default()
    };
    if let Some(encrypted) = &connection.credentials_encrypted {
        let secret = encryption_secret_from(config);
        let plaintext = decrypt_secret_value(&secret, encrypted)?;
        let value: Value = serde_json::from_str(&plaintext).map_err(|_| {
            UnifiedError::internal("Stored BYO credentials cannot be decoded")
        })?;
        let get = |field: &str| {
            value
                .get(field)
                .and_then(|item| item.as_str())
                .unwrap_or("")
                .to_string()
        };
        target.access_key = get("access_key");
        target.secret_key = get("secret_key");
        target.client_id = get("client_id");
        target.client_secret = get("client_secret");
        target.refresh_token = get("refresh_token");
        target.access_token = get("access_token");
        target.token_url = get("token_url");
        target.sftp_password = get("sftp_password");
    }
    Ok(target)
}

/// Build a live provider for a BYO connection. Refused unless the
/// connection is active: a draft, suspended, or revoked connection
/// can never move bytes, even with valid credentials.
fn provider_for_byo(
    config: &RwLock<AppConfig>,
    connection: &ByoConnection,
) -> UnifiedResult<Arc<dyn StorageProvider>> {
    if connection.status != "active" {
        return Err(UnifiedError::conflict(format!(
            "BYO connection \"{}\" is {} and cannot be used",
            connection.name, connection.status
        )));
    }
    let target = target_config_for_byo(config, connection)?;
    provider_from_config(&target, &connection.allowed_hosts)
}

/// Build a provider for validation / health probes, which must run
/// before activation. Status is deliberately not checked here: the
/// caller decides which states may be probed.
fn provider_for_byo_probe(
    config: &RwLock<AppConfig>,
    connection: &ByoConnection,
) -> UnifiedResult<Arc<dyn StorageProvider>> {
    let target = target_config_for_byo(config, connection)?;
    provider_from_config(&target, &connection.allowed_hosts)
}

/// Validate a BYO connection: connect, list the test prefix,
/// write + read + delete a probe object under the connection
/// prefix, then remove it. A failed step blocks activation and the
/// reason is recorded on the connection (never any credential or
/// provider error body beyond the short message). Reuses the same
/// probe shape as the bucket health check.
async fn run_byo_validation(
    state: &StorageManagerState,
    config: &RwLock<AppConfig>,
    connection: &ByoConnection,
) -> UnifiedResult<()> {
    let outcome: UnifiedResult<()> = (|| async {
        let provider = provider_for_byo_probe(config, connection)?;
        provider.test_connection().await?;
        let _ = provider.list(&connection.path_prefix).await;
        let probe_key = format!(
            "{}/.byo-probe-{}/probe.txt",
            connection.path_prefix,
            uuid::Uuid::now_v7()
        );
        let probe_body = b"storage-manager-byo-probe".to_vec();
        provider.upload(&probe_key, probe_body.clone()).await?;
        let read_back = provider.download(&probe_key).await;
        let delete_result = provider.delete(&probe_key).await;
        let bytes = read_back?;
        if bytes != probe_body {
            let _ = delete_result;
            return Err(UnifiedError::internal(
                "BYO validation probe read back different bytes than were written",
            ));
        }
        delete_result?;
        Ok(())
    })()
    .await;
    let now = Utc::now();
    match &outcome {
        Ok(()) => {
            if let Some(mut stored) = state.byo_connections.get_mut(&connection.id) {
                stored.last_validated_at = Some(now);
                stored.last_validation_ok = Some(true);
                stored.last_validation_error = None;
                if stored.status == "draft" {
                    stored.status = "validated".to_string();
                }
                stored.updated_at = now;
            }
            state.emit(
                events::BYO_VALIDATED,
                json!({
                    "connection_id": connection.id.to_string(),
                    "name": connection.name,
                    "owner_user": connection.owner_user,
                    "company_id": connection.company_id,
                    "provider_kind": connection.provider_kind.as_str(),
                    "validated_at": now.to_rfc3339(),
                }),
            );
            Ok(())
        }
        Err(error) => {
            let detail = error.message.clone();
            if let Some(mut stored) = state.byo_connections.get_mut(&connection.id) {
                stored.last_validated_at = Some(now);
                stored.last_validation_ok = Some(false);
                stored.last_validation_error = Some(detail.clone());
                stored.updated_at = now;
            }
            state.emit(
                events::BYO_VALIDATION_FAILED,
                json!({
                    "connection_id": connection.id.to_string(),
                    "name": connection.name,
                    "owner_user": connection.owner_user,
                    "error": detail,
                }),
            );
            Err(error)
        }
    }
}

/// Run one BYO health check: connectivity plus a write / read /
/// delete probe on a scratch key under the connection prefix, then
/// cleanup. Mirrors `run_health_check` for buckets.
async fn run_byo_health_check(
    state: &StorageManagerState,
    config: &RwLock<AppConfig>,
    connection: &ByoConnection,
) -> UnifiedResult<ByoHealthCheck> {
    let started = std::time::Instant::now();
    let outcome: UnifiedResult<()> = (|| async {
        let provider = provider_for_byo_probe(config, connection)?;
        provider.test_connection().await?;
        let probe_key = format!(
            "{}/.byo-healthcheck-{}/probe.txt",
            connection.path_prefix,
            uuid::Uuid::now_v7()
        );
        let probe_body = b"storage-manager-byo-health-probe".to_vec();
        provider.upload(&probe_key, probe_body.clone()).await?;
        let read_back = provider.download(&probe_key).await;
        let delete_result = provider.delete(&probe_key).await;
        let bytes = read_back?;
        if bytes != probe_body {
            let _ = delete_result;
            return Err(UnifiedError::internal(
                "BYO health probe read back different bytes than were written",
            ));
        }
        delete_result?;
        Ok(())
    })()
    .await;
    let latency_ms = started.elapsed().as_millis() as i64;
    let now = Utc::now();
    let (ok, detail) = match &outcome {
        Ok(()) => (true, "Connectivity and write/read/delete probe passed.".to_string()),
        Err(error) => (false, error.message.clone()),
    };
    let check = ByoHealthCheck {
        id: Uuid::now_v7(),
        connection_id: connection.id,
        ok,
        latency_ms,
        detail,
        checked_at: now,
    };
    state.byo_health_checks.insert(check.id, check.clone());
    if let Some(mut stored) = state.byo_connections.get_mut(&connection.id) {
        stored.last_health_at = Some(now);
        stored.last_health_ok = Some(ok);
        stored.updated_at = now;
    }
    if ok {
        state.emit(
            events::BYO_HEALTH_OK,
            json!({
                "connection_id": connection.id.to_string(),
                "name": connection.name,
                "latency_ms": latency_ms,
                "checked_at": now.to_rfc3339(),
            }),
        );
    } else {
        state.emit(
            events::BYO_HEALTH_FAILED,
            json!({
                "connection_id": connection.id.to_string(),
                "name": connection.name,
                "error": check.detail.clone(),
                "latency_ms": latency_ms,
                "checked_at": now.to_rfc3339(),
            }),
        );
    }
    match outcome {
        Ok(()) => Ok(check),
        Err(error) => Err(error),
    }
}

/// Join a caller-supplied relative key onto the connection prefix.
/// The result is always under the prefix: `..`, absolute paths,
/// backslashes, and empty segments are rejected by `validate_key`.
fn byo_full_key(connection: &ByoConnection, relative_key: &str) -> UnifiedResult<String> {
    validate_key(relative_key)?;
    Ok(format!("{}/{}", connection.path_prefix, relative_key))
}

/// Check one object operation against the connection rules:
/// status, allowed operation, prefix scoping, file-type allowlists,
/// and quotas. The 409 quota refusal mirrors the bucket quota path.
fn check_byo_operation(
    state: &StorageManagerState,
    connection: &ByoConnection,
    operation: &str,
    relative_key: &str,
    mime: Option<&str>,
    delta_bytes: i64,
    delta_files: i64,
) -> UnifiedResult<String> {
    if connection.status != "active" {
        return Err(UnifiedError::conflict(format!(
            "BYO connection \"{}\" is {} and cannot be used",
            connection.name, connection.status
        )));
    }
    let allowed = match operation {
        "read" => connection.allow_read,
        "write" => connection.allow_write,
        "delete" => connection.allow_delete,
        _ => false,
    };
    if !allowed {
        return Err(UnifiedError::forbidden(format!(
            "BYO connection \"{}\" does not allow {operation} operations",
            connection.name
        )));
    }
    let full_key = byo_full_key(connection, relative_key)?;
    if !connection.allowed_extensions.is_empty() {
        let extension = relative_key
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        let has_extension = relative_key.contains('.') && !extension.is_empty();
        if !has_extension || !connection.allowed_extensions.contains(&extension) {
            return Err(UnifiedError::bad_request(format!(
                "File extension is not allowed for BYO connection \"{}\"",
                connection.name
            )));
        }
    }
    if !connection.allowed_mime_types.is_empty() {
        let mime = mime.unwrap_or("application/octet-stream");
        if !connection
            .allowed_mime_types
            .iter()
            .any(|allowed| allowed == mime)
        {
            return Err(UnifiedError::bad_request(format!(
                "MIME type \"{mime}\" is not allowed for BYO connection \"{}\"",
                connection.name
            )));
        }
    }
    if operation == "write" {
        if let Some(limit) = connection.max_bytes {
            if connection.used_bytes + delta_bytes > limit {
                state.emit(
                    events::BYO_QUOTA_EXCEEDED,
                    json!({
                        "connection_id": connection.id.to_string(),
                        "owner_user": connection.owner_user,
                        "used_bytes": connection.used_bytes,
                        "bytes": delta_bytes,
                        "max_bytes": limit,
                    }),
                );
                return Err(UnifiedError::conflict(format!(
                    "BYO connection quota exceeded: used {} + {delta_bytes} exceeds the limit of {limit} bytes",
                    connection.used_bytes
                )));
            }
        }
        if let Some(limit) = connection.max_files {
            if connection.used_files + delta_files > limit {
                state.emit(
                    events::BYO_QUOTA_EXCEEDED,
                    json!({
                        "connection_id": connection.id.to_string(),
                        "owner_user": connection.owner_user,
                        "used_files": connection.used_files,
                        "max_files": limit,
                    }),
                );
                return Err(UnifiedError::conflict(format!(
                    "BYO connection quota exceeded: used {} + {delta_files} exceeds the limit of {limit} files",
                    connection.used_files
                )));
            }
        }
    }
    Ok(full_key)
}

fn byo_object_key(connection_id: Uuid, key: &str) -> String {
    format!("{connection_id}:{key}")
}

// ---------------------------------------------------------------------------
// BYO request bodies.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct UpdateByoSettingsRequest {
    #[serde(default)]
    allowed_provider_kinds: Option<Vec<String>>,
    #[serde(default)]
    terms_required: Option<bool>,
    #[serde(default)]
    max_connections_per_user: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct CreateByoTermsRequest {
    version: String,
    title: String,
    body: String,
    #[serde(default)]
    is_current: Option<bool>,
    #[serde(default)]
    reacceptance_deadline: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
struct UpdateByoTermsRequest {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    is_current: Option<bool>,
    #[serde(default)]
    reacceptance_deadline: Option<Option<DateTime<Utc>>>,
    #[serde(default)]
    enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct CreateByoConnectionRequest {
    name: String,
    provider_kind: String,
    #[serde(default)]
    endpoint: Option<String>,
    #[serde(default)]
    region: Option<String>,
    #[serde(default)]
    container: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    sftp_host: Option<String>,
    #[serde(default)]
    sftp_port: Option<u16>,
    #[serde(default)]
    sftp_username: Option<String>,
    #[serde(default)]
    sftp_auth_kind: Option<String>,
    #[serde(default)]
    sftp_keyfile: Option<String>,
    #[serde(default)]
    allowed_hosts: Option<Vec<String>>,
    /// Credential fields (access key, secret key, password, tokens).
    /// Write-only: accepted here, encrypted, never returned.
    #[serde(default)]
    credentials: Option<serde_json::Map<String, Value>>,
    /// Path prefix scoping. When omitted, defaults to
    /// `byo/{owner_user}` so a connection is always scoped.
    #[serde(default)]
    path_prefix: Option<String>,
    #[serde(default)]
    company_id: Option<String>,
    #[serde(default)]
    allow_read: Option<bool>,
    #[serde(default)]
    allow_write: Option<bool>,
    #[serde(default)]
    allow_delete: Option<bool>,
    #[serde(default)]
    max_bytes: Option<i64>,
    #[serde(default)]
    max_files: Option<i64>,
    #[serde(default)]
    encryption_required: Option<bool>,
    #[serde(default)]
    allowed_extensions: Option<Vec<String>>,
    #[serde(default)]
    allowed_mime_types: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct UpdateByoConnectionRequest {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    endpoint: Option<String>,
    #[serde(default)]
    region: Option<String>,
    #[serde(default)]
    container: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    sftp_host: Option<String>,
    #[serde(default)]
    sftp_port: Option<u16>,
    #[serde(default)]
    sftp_username: Option<String>,
    #[serde(default)]
    sftp_auth_kind: Option<String>,
    #[serde(default)]
    sftp_keyfile: Option<String>,
    #[serde(default)]
    allowed_hosts: Option<Vec<String>>,
    /// Replaces the stored credentials when present (an empty object
    /// clears them). Absent means "keep the stored credentials".
    /// Replacing credentials resets the connection to `draft`: it
    /// must be validated again before it can activate.
    #[serde(default)]
    credentials: Option<serde_json::Map<String, Value>>,
    #[serde(default)]
    path_prefix: Option<String>,
    #[serde(default)]
    allow_read: Option<bool>,
    #[serde(default)]
    allow_write: Option<bool>,
    #[serde(default)]
    allow_delete: Option<bool>,
    #[serde(default)]
    max_bytes: Option<Option<i64>>,
    #[serde(default)]
    max_files: Option<Option<i64>>,
    #[serde(default)]
    encryption_required: Option<bool>,
    #[serde(default)]
    allowed_extensions: Option<Vec<String>>,
    #[serde(default)]
    allowed_mime_types: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct ByoStatusRequest {
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AcceptTermsRequest {
    #[serde(default)]
    connection_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
struct RunByoHealthRequest {
    #[serde(default)]
    connection_id: Option<Uuid>,
    /// Also ask the scheduler to enqueue a `storage.byo.health_check`
    /// job. The scheduler declines it until the kind is registered
    /// (see the module header); the run still executes in-process.
    #[serde(default)]
    enqueue: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct ByoUploadQuery {
    key: String,
    #[serde(default)]
    mime: Option<String>,
}

// ---------------------------------------------------------------------------
// BYO settings (admin-managed provider allowlist).
// ---------------------------------------------------------------------------

async fn get_byo_settings(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_READ) {
        return err_response(error, debug);
    }
    let settings = state
        .byo_settings
        .read()
        .expect("BYO settings lock poisoned")
        .clone();
    ok(settings)
}

async fn update_byo_settings(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<UpdateByoSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_MANAGE_BYO) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<ByoSettings> = (|| {
        let input = body.into_inner();
        let mut settings = state
            .byo_settings
            .read()
            .expect("BYO settings lock poisoned")
            .clone();
        if let Some(kinds) = &input.allowed_provider_kinds {
            settings.allowed_provider_kinds = validate_provider_kind_list(kinds)?;
        }
        if let Some(terms_required) = input.terms_required {
            settings.terms_required = terms_required;
        }
        if let Some(max) = input.max_connections_per_user {
            if max < 0 {
                return Err(UnifiedError::bad_request(
                    "max_connections_per_user must not be negative (use 0 for unlimited)",
                ));
            }
            settings.max_connections_per_user = max;
        }
        *state
            .byo_settings
            .write()
            .expect("BYO settings lock poisoned") = settings.clone();
        state.emit(
            events::BYO_TERMS_CHANGED,
            json!({
                "scope": "byo_settings",
                "allowed_provider_kinds": settings.allowed_provider_kinds,
                "terms_required": settings.terms_required,
                "changed_by": found.actor,
            }),
        );
        Ok(settings)
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// BYO terms.
// ---------------------------------------------------------------------------

async fn list_byo_terms(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_READ) {
        return err_response(error, debug);
    }
    let params = params.into_inner();
    let mut items: Vec<ByoTerms> = state
        .byo_terms
        .iter()
        .map(|entry| entry.value().clone())
        .collect();
    if let Some(filter) = params.filters.get("is_current") {
        let want = filter == "true";
        items.retain(|terms| terms.is_current == want);
    }
    if let Some(filter) = params.filters.get("enabled") {
        let want = filter == "true";
        items.retain(|terms| terms.enabled == want);
    }
    if let Some(search) = &params.search {
        let needle = search.to_lowercase();
        items.retain(|terms| {
            terms.version.to_lowercase().contains(&needle)
                || terms.title.to_lowercase().contains(&needle)
        });
    }
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    ok(paginate(items, &params))
}

async fn get_current_byo_terms(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_READ) {
        return err_response(error, debug);
    }
    match current_byo_terms(&state) {
        Some(terms) => ok(terms),
        None => err_response(
            UnifiedError::not_found("No current BYO terms version is published"),
            debug,
        ),
    }
}

async fn create_byo_terms(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateByoTermsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_MANAGE_BYO) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<ByoTerms> = (|| {
        let input = body.into_inner();
        let version = require_text(&input.version, "version", 32)?;
        if state
            .byo_terms
            .iter()
            .any(|entry| entry.value().version == version)
        {
            return Err(UnifiedError::conflict(format!(
                "BYO terms version \"{version}\" already exists"
            )));
        }
        let title = require_text(&input.title, "title", 256)?;
        let body_text = require_text(&input.body, "body", 100_000)?;
        let is_current = input.is_current.unwrap_or(false);
        if is_current {
            for mut entry in state.byo_terms.iter_mut() {
                entry.value_mut().is_current = false;
            }
        }
        let now = Utc::now();
        let terms = ByoTerms {
            id: Uuid::now_v7(),
            version: version.clone(),
            title,
            body: body_text,
            is_current,
            reacceptance_deadline: input.reacceptance_deadline,
            enabled: true,
            created_by: found.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.byo_terms.insert(terms.id, terms.clone());
        state.emit(
            events::BYO_TERMS_CHANGED,
            json!({
                "scope": "terms_created",
                "terms_id": terms.id.to_string(),
                "version": version,
                "is_current": is_current,
                "created_by": found.actor,
            }),
        );
        Ok(terms)
    })();
    match result {
        Ok(terms) => created(terms),
        Err(error) => err_response(error, debug),
    }
}

async fn get_byo_terms(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_READ) {
        return err_response(error, debug);
    }
    json::respond(load_byo_terms(&state, &id), debug)
}

async fn update_byo_terms(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    body: web::Json<UpdateByoTermsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_MANAGE_BYO) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<ByoTerms> = (|| {
        let input = body.into_inner();
        let mut terms = load_byo_terms(&state, &id)?;
        if let Some(title) = &input.title {
            terms.title = require_text(title, "title", 256)?;
        }
        if let Some(body_text) = &input.body {
            terms.body = require_text(body_text, "body", 100_000)?;
        }
        if let Some(is_current) = input.is_current {
            if is_current {
                for mut entry in state.byo_terms.iter_mut() {
                    if entry.value().id != terms.id {
                        entry.value_mut().is_current = false;
                    }
                }
            }
            terms.is_current = is_current;
        }
        if let Some(deadline) = input.reacceptance_deadline {
            terms.reacceptance_deadline = deadline;
        }
        if let Some(enabled) = input.enabled {
            terms.enabled = enabled;
        }
        terms.updated_at = Utc::now();
        state.byo_terms.insert(terms.id, terms.clone());
        state.emit(
            events::BYO_TERMS_CHANGED,
            json!({
                "scope": "terms_updated",
                "terms_id": terms.id.to_string(),
                "version": terms.version,
                "is_current": terms.is_current,
                "reacceptance_deadline": terms.reacceptance_deadline.map(|at| at.to_rfc3339()),
                "changed_by": found.actor,
            }),
        );
        Ok(terms)
    })();
    json::respond(result, debug)
}

async fn delete_byo_terms(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_MANAGE_BYO) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<Value> = (|| {
        let terms = load_byo_terms(&state, &id)?;
        if terms.is_current {
            return Err(UnifiedError::conflict(
                "The current BYO terms version cannot be deleted; publish another version as current first",
            ));
        }
        if state
            .byo_acceptances
            .iter()
            .any(|entry| entry.value().terms_id == terms.id)
        {
            return Err(UnifiedError::conflict(format!(
                "BYO terms version \"{}\" cannot be deleted: users have already accepted it",
                terms.version
            )));
        }
        state.byo_terms.remove(&terms.id);
        state.emit(
            events::BYO_TERMS_CHANGED,
            json!({
                "scope": "terms_deleted",
                "terms_id": terms.id.to_string(),
                "version": terms.version,
                "deleted_by": found.actor,
            }),
        );
        Ok(json!({ "deleted": true, "terms_id": terms.id }))
    })();
    json::respond(result, debug)
}

/// Record the caller's acceptance of the current BYO terms
/// version. The acceptance is what activation checks later.
async fn accept_byo_terms(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<AcceptTermsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_BYO_USE) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<ByoTermsAcceptance> = (|| {
        let input = body.into_inner();
        let terms = current_byo_terms(&state).ok_or_else(|| {
            UnifiedError::not_found("No current BYO terms version is published")
        })?;
        if let Some(connection_id) = input.connection_id {
            let connection = load_byo_connection(&state, &connection_id.to_string())?;
            if connection.owner_user != found.actor {
                return Err(UnifiedError::not_found(format!(
                    "BYO connection not found: {connection_id}"
                )));
            }
        }
        let now = Utc::now();
        let acceptance = ByoTermsAcceptance {
            id: Uuid::now_v7(),
            terms_id: terms.id,
            terms_version: terms.version.clone(),
            user_id: found.actor.clone(),
            company_id: None,
            connection_id: input.connection_id,
            accepted_at: now,
        };
        state.byo_acceptances.insert(
            format!("{}|{}", acceptance.user_id, acceptance.terms_version),
            acceptance.clone(),
        );
        if let Some(connection_id) = input.connection_id {
            if let Some(mut connection) = state.byo_connections.get_mut(&connection_id) {
                connection.terms_version = Some(terms.version.clone());
                connection.terms_accepted_at = Some(now);
                connection.updated_at = now;
            }
        }
        state.emit(
            events::BYO_TERMS_ACCEPTED,
            json!({
                "terms_id": terms.id.to_string(),
                "terms_version": terms.version,
                "user_id": found.actor,
                "connection_id": input.connection_id.map(|id| id.to_string()),
                "accepted_at": now.to_rfc3339(),
            }),
        );
        Ok(acceptance)
    })();
    match result {
        Ok(acceptance) => created(acceptance),
        Err(error) => err_response(error, debug),
    }
}

/// Suspend connections whose owner has not accepted the current
/// terms version after its re-acceptance deadline. Existing
/// connections keep working until the deadline; this endpoint (or
/// a scheduler run calling the same check) enforces it afterwards.
/// Only an admin may run it.
async fn enforce_byo_reacceptance(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_MANAGE_BYO) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<Value> = (|| {
        let terms = current_byo_terms(&state).ok_or_else(|| {
            UnifiedError::not_found("No current BYO terms version is published")
        })?;
        let Some(deadline) = terms.reacceptance_deadline else {
            return Ok(json!({
                "terms_version": terms.version,
                "deadline": Value::Null,
                "suspended": 0,
                "suspended_connection_ids": Vec::<String>::new(),
                "note": "No re-acceptance deadline is set for the current terms version",
            }));
        };
        if Utc::now() <= deadline {
            return Ok(json!({
                "terms_version": terms.version,
                "deadline": deadline.to_rfc3339(),
                "suspended": 0,
                "suspended_connection_ids": Vec::<String>::new(),
                "note": "Re-acceptance deadline has not passed yet",
            }));
        }
        // Two passes: collect the connections to suspend first
        // (shared borrows only), then mutate them. Holding a map
        // guard while calling back into state would not compile.
        let mut to_suspend: Vec<Uuid> = Vec::new();
        for entry in state.byo_connections.iter() {
            let connection = entry.value();
            if (connection.status == "active" || connection.status == "validated")
                && !has_accepted_terms(&state, &connection.owner_user, &terms.version)
            {
                to_suspend.push(connection.id);
            }
        }
        let mut suspended: Vec<String> = Vec::new();
        for id in to_suspend {
            let (name, owner_user, company_id) = {
                let Some(mut connection) = state.byo_connections.get_mut(&id) else {
                    continue;
                };
                connection.status = "suspended".to_string();
                connection.status_reason = Some(format!(
                    "Suspended: terms version {} was not accepted before the re-acceptance deadline {}",
                    terms.version,
                    deadline.to_rfc3339()
                ));
                connection.updated_at = Utc::now();
                (
                    connection.name.clone(),
                    connection.owner_user.clone(),
                    connection.company_id.clone(),
                )
            };
            suspended.push(id.to_string());
            state.emit(
                events::BYO_SUSPENDED,
                json!({
                    "connection_id": id.to_string(),
                    "name": name,
                    "owner_user": owner_user,
                    "company_id": company_id,
                    "reason": "terms_reacceptance_deadline",
                    "terms_version": terms.version,
                    "suspended_by": found.actor,
                }),
            );
        }
        Ok(json!({
            "terms_version": terms.version,
            "deadline": deadline.to_rfc3339(),
            "suspended": suspended.len(),
            "suspended_connection_ids": suspended,
        }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// BYO connections.
// ---------------------------------------------------------------------------

async fn list_byo_connections(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_BYO_USE) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = check_capability(&ctx, &found.role, PERM_MANAGE_BYO);
    let params = params.into_inner();
    let mut items: Vec<ByoConnectionView> = state
        .byo_connections
        .iter()
        .map(|entry| ByoConnectionView::from(entry.value()))
        .filter(|view| is_admin || view.owner_user == found.actor)
        .collect();
    if let Some(filter) = params.filters.get("status") {
        items.retain(|view| view.status == *filter);
    }
    if let Some(filter) = params.filters.get("provider_kind") {
        items.retain(|view| view.provider_kind.as_str() == filter.as_str());
    }
    if let Some(filter) = params.filters.get("company_id") {
        items.retain(|view| view.company_id.as_deref() == Some(filter.as_str()));
    }
    if let Some(search) = &params.search {
        let needle = search.to_lowercase();
        items.retain(|view| view.name.to_lowercase().contains(&needle));
    }
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    ok(paginate(items, &params))
}

async fn create_byo_connection(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateByoConnectionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_BYO_USE) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<ByoConnectionView> = (|| {
        let input = body.into_inner();
        let settings = state
            .byo_settings
            .read()
            .expect("BYO settings lock poisoned")
            .clone();
        let kind_name = canonical_provider_kind_name(&input.provider_kind)?;
        if !settings.allowed_provider_kinds.contains(&kind_name) {
            return Err(UnifiedError::forbidden(format!(
                "Provider kind \"{kind_name}\" is not on the BYO allowlist (allowed: {})",
                settings.allowed_provider_kinds.join(", ")
            )));
        }
        let kind = storage_kind_for_name(&kind_name)?;
        let s3_compatible = kind_name == "s3-compatible";
        let name = require_text(&input.name, "name", 128)?;
        if state
            .byo_connections
            .iter()
            .any(|entry| entry.value().owner_user == found.actor && entry.value().name == name)
        {
            return Err(UnifiedError::conflict(format!(
                "BYO connection \"{name}\" already exists for this user"
            )));
        }
        if settings.max_connections_per_user > 0 {
            let active_count = state
                .byo_connections
                .iter()
                .filter(|entry| {
                    entry.value().owner_user == found.actor
                        && entry.value().status != "revoked"
                })
                .count() as i64;
            if active_count >= settings.max_connections_per_user {
                return Err(UnifiedError::conflict(format!(
                    "Maximum BYO connections per user ({}) reached",
                    settings.max_connections_per_user
                )));
            }
        }
        let endpoint = optional_text(&input.endpoint, "endpoint", 1024)?.unwrap_or_default();
        if s3_compatible && endpoint.is_empty() {
            return Err(UnifiedError::bad_request(
                "An endpoint is required for the s3-compatible provider kind",
            ));
        }
        let region = optional_text(&input.region, "region", 128)?.unwrap_or_default();
        let container = optional_text(&input.container, "container", 256)?.unwrap_or_default();
        let path = optional_text(&input.path, "path", 1024)?.unwrap_or_default();
        let sftp_host = optional_text(&input.sftp_host, "sftp_host", 253)?.unwrap_or_default();
        let sftp_username =
            optional_text(&input.sftp_username, "sftp_username", 64)?.unwrap_or_default();
        let sftp_auth_kind = match input.sftp_auth_kind {
            None => String::new(),
            Some(raw) => {
                let value = raw.trim().to_string();
                if !value.is_empty() && value != "keyfile" && value != "password" {
                    return Err(UnifiedError::bad_request(
                        "sftp_auth_kind must be \"keyfile\" or \"password\"",
                    ));
                }
                value
            }
        };
        let sftp_keyfile =
            optional_text(&input.sftp_keyfile, "sftp_keyfile", 1024)?.unwrap_or_default();
        let allowed_hosts = input.allowed_hosts.unwrap_or_default();
        for host in &allowed_hosts {
            if host.trim().is_empty() || host.len() > 253 {
                return Err(UnifiedError::bad_request(
                    "allowed_hosts entries must be 1..=253 characters",
                ));
            }
        }
        let company_id = optional_text(&input.company_id, "company_id", 128)?;
        let path_prefix = match &input.path_prefix {
            Some(raw) => valid_prefix(raw)?,
            None => format!("byo/{}", found.actor),
        };
        if path_prefix.is_empty() {
            return Err(UnifiedError::bad_request(
                "path_prefix must not be empty: a BYO connection is always scoped to a prefix",
            ));
        }
        let max_bytes = valid_quota_limit(input.max_bytes, "max_bytes")?;
        let max_files = valid_quota_limit(input.max_files, "max_files")?;
        let allowed_extensions = validate_extensions(&input.allowed_extensions)?;
        let allowed_mime_types = validate_mime_types(&input.allowed_mime_types)?;
        let (credentials_encrypted, key_fingerprint) = match &input.credentials {
            Some(credentials) => {
                let (encrypted, fingerprint) = encrypt_credentials(&ctx, credentials)?;
                (Some(encrypted), Some(fingerprint))
            }
            None => (None, None),
        };
        let now = Utc::now();
        let connection = ByoConnection {
            id: Uuid::now_v7(),
            name,
            provider_kind: kind,
            s3_compatible,
            endpoint,
            region,
            container,
            path,
            sftp_host,
            sftp_port: input.sftp_port.unwrap_or(0),
            sftp_username,
            sftp_auth_kind,
            sftp_keyfile,
            allowed_hosts,
            credentials_encrypted,
            key_fingerprint,
            path_prefix,
            owner_user: found.actor.clone(),
            company_id,
            status: "draft".to_string(),
            status_reason: None,
            allow_read: input.allow_read.unwrap_or(true),
            allow_write: input.allow_write.unwrap_or(true),
            allow_delete: input.allow_delete.unwrap_or(false),
            max_bytes,
            max_files,
            used_bytes: 0,
            used_files: 0,
            encryption_required: input.encryption_required.unwrap_or(false),
            allowed_extensions,
            allowed_mime_types,
            terms_version: None,
            terms_accepted_at: None,
            last_validated_at: None,
            last_validation_ok: None,
            last_validation_error: None,
            last_health_at: None,
            last_health_ok: None,
            created_at: now,
            updated_at: now,
        };
        // A connection that cannot build a provider cannot be
        // created: fail here, before anything is stored.
        let target = target_config_for_byo(&ctx.config, &connection)?;
        provider_from_config(&target, &connection.allowed_hosts).map(|_| ())?;
        state
            .byo_connections
            .insert(connection.id, connection.clone());
        state.emit(
            events::BYO_CREATED,
            json!({
                "connection_id": connection.id.to_string(),
                "name": connection.name,
                "owner_user": connection.owner_user,
                "company_id": connection.company_id,
                "provider_kind": kind_name,
                "path_prefix": connection.path_prefix,
                "created_by": found.actor,
            }),
        );
        Ok(ByoConnectionView::from(&connection))
    })();
    match result {
        Ok(view) => created(view),
        Err(error) => err_response(error, debug),
    }
}

async fn get_byo_connection(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_BYO_USE) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = check_capability(&ctx, &found.role, PERM_MANAGE_BYO);
    json::respond(
        load_byo_connection_for(&state, &id, &found.actor, is_admin)
            .map(|connection| ByoConnectionView::from(&connection)),
        debug,
    )
}

async fn update_byo_connection(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    body: web::Json<UpdateByoConnectionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_BYO_USE) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = check_capability(&ctx, &found.role, PERM_MANAGE_BYO);
    let result: UnifiedResult<ByoConnectionView> = (|| {
        let input = body.into_inner();
        let mut connection = load_byo_connection_for(&state, &id, &found.actor, is_admin)?;
        if connection.status == "revoked" {
            return Err(UnifiedError::conflict(
                "A revoked BYO connection cannot be updated",
            ));
        }
        if let Some(name) = &input.name {
            let name = require_text(name, "name", 128)?;
            if state.byo_connections.iter().any(|entry| {
                entry.value().id != connection.id
                    && entry.value().owner_user == connection.owner_user
                    && entry.value().name == name
            }) {
                return Err(UnifiedError::conflict(format!(
                    "BYO connection \"{name}\" already exists for this user"
                )));
            }
            connection.name = name;
        }
        if let Some(value) = &input.endpoint {
            connection.endpoint = optional_text(&Some(value.clone()), "endpoint", 1024)?
                .unwrap_or_default();
        }
        if let Some(value) = &input.region {
            connection.region =
                optional_text(&Some(value.clone()), "region", 128)?.unwrap_or_default();
        }
        if let Some(value) = &input.container {
            connection.container =
                optional_text(&Some(value.clone()), "container", 256)?.unwrap_or_default();
        }
        if let Some(value) = &input.path {
            connection.path =
                optional_text(&Some(value.clone()), "path", 1024)?.unwrap_or_default();
        }
        if let Some(value) = &input.sftp_host {
            connection.sftp_host =
                optional_text(&Some(value.clone()), "sftp_host", 253)?.unwrap_or_default();
        }
        if let Some(port) = input.sftp_port {
            connection.sftp_port = port;
        }
        if let Some(value) = &input.sftp_username {
            connection.sftp_username =
                optional_text(&Some(value.clone()), "sftp_username", 64)?.unwrap_or_default();
        }
        if let Some(raw) = &input.sftp_auth_kind {
            let value = raw.trim().to_string();
            if !value.is_empty() && value != "keyfile" && value != "password" {
                return Err(UnifiedError::bad_request(
                    "sftp_auth_kind must be \"keyfile\" or \"password\"",
                ));
            }
            connection.sftp_auth_kind = value;
        }
        if let Some(value) = &input.sftp_keyfile {
            connection.sftp_keyfile =
                optional_text(&Some(value.clone()), "sftp_keyfile", 1024)?.unwrap_or_default();
        }
        if let Some(hosts) = &input.allowed_hosts {
            for host in hosts {
                if host.trim().is_empty() || host.len() > 253 {
                    return Err(UnifiedError::bad_request(
                        "allowed_hosts entries must be 1..=253 characters",
                    ));
                }
            }
            connection.allowed_hosts = hosts.clone();
        }
        // Replacing credentials resets the connection to `draft`:
        // the new credentials must be validated before activation.
        if let Some(credentials) = &input.credentials {
            if credentials.is_empty() {
                connection.credentials_encrypted = None;
                connection.key_fingerprint = None;
            } else {
                let (encrypted, fingerprint) = encrypt_credentials(&ctx, credentials)?;
                connection.credentials_encrypted = Some(encrypted);
                connection.key_fingerprint = Some(fingerprint);
            }
            if connection.status == "active" || connection.status == "validated" {
                connection.status = "draft".to_string();
                connection.status_reason = Some(
                    "Credentials changed: the connection must be validated again before activation"
                        .to_string(),
                );
                connection.last_validation_ok = None;
            }
        }
        if let Some(prefix) = &input.path_prefix {
            let prefix = valid_prefix(prefix)?;
            if prefix.is_empty() {
                return Err(UnifiedError::bad_request(
                    "path_prefix must not be empty: a BYO connection is always scoped to a prefix",
                ));
            }
            connection.path_prefix = prefix;
        }
        if let Some(value) = input.allow_read {
            connection.allow_read = value;
        }
        if let Some(value) = input.allow_write {
            connection.allow_write = value;
        }
        if let Some(value) = input.allow_delete {
            connection.allow_delete = value;
        }
        if let Some(limit) = input.max_bytes {
            connection.max_bytes = valid_quota_limit(limit, "max_bytes")?;
        }
        if let Some(limit) = input.max_files {
            connection.max_files = valid_quota_limit(limit, "max_files")?;
        }
        if let Some(value) = input.encryption_required {
            connection.encryption_required = value;
        }
        if let Some(extensions) = &input.allowed_extensions {
            connection.allowed_extensions = validate_extensions(&Some(extensions.clone()))?;
        }
        if let Some(mime_types) = &input.allowed_mime_types {
            connection.allowed_mime_types = validate_mime_types(&Some(mime_types.clone()))?;
        }
        connection.updated_at = Utc::now();
        let target = target_config_for_byo(&ctx.config, &connection)?;
        provider_from_config(&target, &connection.allowed_hosts).map(|_| ())?;
        state
            .byo_connections
            .insert(connection.id, connection.clone());
        state.emit(
            events::BYO_UPDATED,
            json!({
                "connection_id": connection.id.to_string(),
                "name": connection.name,
                "owner_user": connection.owner_user,
                "status": connection.status,
                "updated_by": found.actor,
            }),
        );
        Ok(ByoConnectionView::from(&connection))
    })();
    json::respond(result, debug)
}

/// Delete a BYO connection. Deleting revokes it first in the audit
/// trail (the connection can no longer move bytes) and removes the
/// recorded objects that belonged to it; the bytes at the provider
/// are left alone, exactly like a bucket delete leaves provider
/// objects alone.
async fn delete_byo_connection(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_BYO_USE) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = check_capability(&ctx, &found.role, PERM_MANAGE_BYO);
    let result: UnifiedResult<Value> = (|| {
        let connection = load_byo_connection_for(&state, &id, &found.actor, is_admin)?;
        state.byo_connections.remove(&connection.id);
        state
            .byo_objects
            .retain(|_, object| object.connection_id != connection.id);
        state.emit(
            events::BYO_REVOKED,
            json!({
                "connection_id": connection.id.to_string(),
                "name": connection.name,
                "owner_user": connection.owner_user,
                "reason": "deleted",
                "revoked_by": found.actor,
            }),
        );
        Ok(json!({ "deleted": true, "connection_id": connection.id }))
    })();
    json::respond(result, debug)
}

async fn validate_byo_connection(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_BYO_USE) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = check_capability(&ctx, &found.role, PERM_MANAGE_BYO);
    let connection = match load_byo_connection_for(&state, &id, &found.actor, is_admin) {
        Ok(connection) => connection,
        Err(error) => return err_response(error, debug),
    };
    if connection.status == "revoked" {
        return err_response(
            UnifiedError::conflict("A revoked BYO connection cannot be validated"),
            debug,
        );
    }
    let result: UnifiedResult<Value> = (|| async {
        run_byo_validation(&state, &ctx.config, &connection).await?;
        let updated = load_byo_connection(&state, &id)?;
        Ok(json!({
            "connection_id": updated.id,
            "name": updated.name,
            "status": updated.status,
            "ok": true,
            "validated_at": updated.last_validated_at,
        }))
    })()
    .await;
    json::respond(result, debug)
}

/// Activate a BYO connection. Requires, in order: the connection
/// is validated, the provider kind is still allowlisted, the owner
/// has accepted the current terms version (when terms are
/// required), and the module entitlement when the connection is
/// company-scoped.
async fn activate_byo_connection(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_BYO_USE) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = check_capability(&ctx, &found.role, PERM_MANAGE_BYO);
    let result: UnifiedResult<ByoConnectionView> = (|| {
        let mut connection = load_byo_connection_for(&state, &id, &found.actor, is_admin)?;
        if connection.status == "active" {
            return Ok(ByoConnectionView::from(&connection));
        }
        if connection.status == "revoked" {
            return Err(UnifiedError::conflict(
                "A revoked BYO connection cannot be activated",
            ));
        }
        if connection.last_validation_ok != Some(true) {
            return Err(UnifiedError::conflict(
                "BYO connection must pass validation before it can activate: run POST /byo/connections/{id}/validate first",
            ));
        }
        let settings = state
            .byo_settings
            .read()
            .expect("BYO settings lock poisoned");
        // Exact canonical-name match: an `s3-compatible` connection
        // needs `s3-compatible` on the allowlist, not just `s3`,
        // and a plain `s3` connection needs `s3`.
        let kind_name = if connection.s3_compatible {
            "s3-compatible".to_string()
        } else {
            connection.provider_kind.as_str().to_string()
        };
        let allowed = settings.allowed_provider_kinds.contains(&kind_name);
        drop(settings);
        if !allowed {
            return Err(UnifiedError::forbidden(format!(
                "Provider kind \"{kind_name}\" is no longer on the BYO allowlist"
            )));
        }
        let terms_required = state
            .byo_settings
            .read()
            .expect("BYO settings lock poisoned")
            .terms_required;
        if terms_required {
            let terms = current_byo_terms(&state).ok_or_else(|| {
                UnifiedError::conflict(
                    "No current BYO terms version is published, so the connection cannot activate",
                )
            })?;
            if !has_accepted_terms(&state, &connection.owner_user, &terms.version) {
                return Err(UnifiedError::conflict(format!(
                    "BYO connection cannot activate until its owner accepts terms version {}",
                    terms.version
                )));
            }
            connection.terms_version = Some(terms.version.clone());
        }
        if let Some(company) = &connection.company_id {
            require_storage_module_entitlement(&ctx, company)?;
        }
        connection.status = "active".to_string();
        connection.status_reason = None;
        connection.updated_at = Utc::now();
        state
            .byo_connections
            .insert(connection.id, connection.clone());
        state.emit(
            events::BYO_ACTIVATED,
            json!({
                "connection_id": connection.id.to_string(),
                "name": connection.name,
                "owner_user": connection.owner_user,
                "company_id": connection.company_id,
                "provider_kind": connection.provider_kind.as_str(),
                "activated_by": found.actor,
            }),
        );
        Ok(ByoConnectionView::from(&connection))
    })();
    json::respond(result, debug)
}

fn set_byo_status(
    state: &StorageManagerState,
    connection: &mut ByoConnection,
    status: &str,
    reason: Option<String>,
    actor: &str,
    event: &str,
) -> ByoConnectionView {
    connection.status = status.to_string();
    connection.status_reason = reason;
    connection.updated_at = Utc::now();
    state
        .byo_connections
        .insert(connection.id, connection.clone());
    state.emit(
        event,
        json!({
            "connection_id": connection.id.to_string(),
            "name": connection.name,
            "owner_user": connection.owner_user,
            "company_id": connection.company_id,
            "status": status,
            "reason": connection.status_reason,
            "changed_by": actor,
        }),
    );
    ByoConnectionView::from(&*connection)
}

async fn suspend_byo_connection(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    body: web::Json<ByoStatusRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_BYO_USE) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = check_capability(&ctx, &found.role, PERM_MANAGE_BYO);
    let result: UnifiedResult<ByoConnectionView> = (|| {
        let input = body.into_inner();
        let mut connection = load_byo_connection_for(&state, &id, &found.actor, is_admin)?;
        if connection.status == "revoked" {
            return Err(UnifiedError::conflict(
                "A revoked BYO connection cannot be suspended",
            ));
        }
        if connection.status == "suspended" {
            return Ok(ByoConnectionView::from(&connection));
        }
        let reason = optional_text(&input.reason, "reason", 512)?;
        Ok(set_byo_status(
            &state,
            &mut connection,
            "suspended",
            reason,
            &found.actor,
            events::BYO_SUSPENDED,
        ))
    })();
    json::respond(result, debug)
}

async fn revoke_byo_connection(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    body: web::Json<ByoStatusRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_BYO_USE) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = check_capability(&ctx, &found.role, PERM_MANAGE_BYO);
    let result: UnifiedResult<ByoConnectionView> = (|| {
        let input = body.into_inner();
        let mut connection = load_byo_connection_for(&state, &id, &found.actor, is_admin)?;
        if connection.status == "revoked" {
            return Ok(ByoConnectionView::from(&connection));
        }
        let reason = optional_text(&input.reason, "reason", 512)?;
        Ok(set_byo_status(
            &state,
            &mut connection,
            "revoked",
            reason,
            &found.actor,
            events::BYO_REVOKED,
        ))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// BYO health checks.
// ---------------------------------------------------------------------------

async fn health_byo_connection(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_HEALTH) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = check_capability(&ctx, &found.role, PERM_MANAGE_BYO);
    let connection = match load_byo_connection_for(&state, &id, &found.actor, is_admin) {
        Ok(connection) => connection,
        Err(error) => return err_response(error, debug),
    };
    if connection.status == "revoked" {
        return err_response(
            UnifiedError::conflict("A revoked BYO connection cannot be health-checked"),
            debug,
        );
    }
    let result: UnifiedResult<ByoHealthCheck> = (|| async {
        run_byo_health_check(&state, &ctx.config, &connection).await
    })()
    .await;
    json::respond(result, debug)
}

async fn list_byo_health(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_HEALTH) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = check_capability(&ctx, &found.role, PERM_MANAGE_BYO);
    let params = params.into_inner();
    let mut items: Vec<ByoHealthCheck> = state
        .byo_health_checks
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|check| {
            if is_admin {
                return true;
            }
            state
                .byo_connections
                .get(&check.connection_id)
                .is_some_and(|entry| entry.value().owner_user == found.actor)
        })
        .collect();
    if let Some(filter) = params.filters.get("connection_id") {
        items.retain(|check| check.connection_id.to_string() == *filter);
    }
    if let Some(filter) = params.filters.get("ok") {
        let want = filter == "true";
        items.retain(|check| check.ok == want);
    }
    items.sort_by(|a, b| b.checked_at.cmp(&a.checked_at));
    ok(paginate(items, &params))
}

async fn list_byo_connection_health(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_HEALTH) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = check_capability(&ctx, &found.role, PERM_MANAGE_BYO);
    let connection = match load_byo_connection_for(&state, &id, &found.actor, is_admin) {
        Ok(connection) => connection,
        Err(error) => return err_response(error, debug),
    };
    let params = params.into_inner();
    let mut items: Vec<ByoHealthCheck> = state
        .byo_health_checks
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|check| check.connection_id == connection.id)
        .collect();
    items.sort_by(|a, b| b.checked_at.cmp(&a.checked_at));
    ok(paginate(items, &params))
}

/// Run BYO health checks now for every active connection (or one
/// connection when `connection_id` is given), synchronously, and
/// optionally ask the scheduler to enqueue a
/// `storage.byo.health_check` job. The scheduler declines it until
/// the kind is registered (see the module header); the run still
/// executes in-process.
async fn run_byo_health(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<RunByoHealthRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_HEALTH) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = check_capability(&ctx, &found.role, PERM_MANAGE_BYO);
    let input = body.into_inner();
    let mut queued = false;
    let mut queue_error: Option<String> = None;
    if input.enqueue.unwrap_or(false) {
        let answer = ctx.hooks.emit(
            events::SCHEDULER_ENQUEUE,
            json!({
                "kind": "storage.byo.health_check",
                "payload": {
                    "connection_id": input.connection_id.map(|id| id.to_string()),
                    "requested_by": found.actor,
                },
            }),
        );
        let first = answer.results.into_iter().next().unwrap_or(Value::Null);
        queued = first.get("queued").and_then(Value::as_bool).unwrap_or(false);
        if !queued {
            queue_error = first
                .get("error")
                .and_then(|value| value.as_str())
                .map(str::to_string);
        }
    }
    let result: UnifiedResult<Value> = (|| async {
        let connections: Vec<ByoConnection> = match input.connection_id {
            Some(id) => vec![load_byo_connection_for(
                &state,
                &id.to_string(),
                &found.actor,
                is_admin,
            )?],
            None => state
                .byo_connections
                .iter()
                .map(|entry| entry.value().clone())
                .filter(|connection| {
                    connection.status == "active"
                        && (is_admin || connection.owner_user == found.actor)
                })
                .collect(),
        };
        let mut results: Vec<Value> = Vec::new();
        for connection in &connections {
            match run_byo_health_check(&state, &ctx.config, connection).await {
                Ok(check) => results.push(json!({
                    "connection_id": connection.id,
                    "ok": true,
                    "latency_ms": check.latency_ms,
                    "checked_at": check.checked_at.to_rfc3339(),
                })),
                Err(error) => results.push(json!({
                    "connection_id": connection.id,
                    "ok": false,
                    "error": error.message,
                })),
            }
        }
        Ok(json!({
            "connections_checked": results.len(),
            "results": results,
            "scheduler_queued": queued,
            "scheduler_error": queue_error,
        }))
    })()
    .await;
    json::respond(result, debug)
}

/// Answer `storage.byo.health_check_requested`: the event the
/// scheduler emits when a `storage.byo.health_check` job fires.
/// Hook handlers are synchronous, so the checks spawn on the async
/// runtime and the handler answers immediately with
/// `{"accepted": true, "queued": true}`. When `init` never ran (no
/// config handle), the handler answers `accepted: false` instead of
/// pretending the checks happened.
fn on_byo_health_check_requested(state: StorageManagerState, ctx: &mut HookContext) {
    let connection_id = ctx
        .data
        .get("connection_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok());
    let connections: Vec<ByoConnection> = match connection_id {
        Some(id) => match state.byo_connections.get(&id) {
            Some(entry) => vec![entry.value().clone()],
            None => {
                ctx.add_result(json!({
                    "accepted": false,
                    "error": format!("BYO connection not found: {id}"),
                }));
                return;
            }
        },
        None => state
            .byo_connections
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|connection| connection.status == "active")
            .collect(),
    };
    let Some(config) = state
        .app_config
        .read()
        .expect("app config lock poisoned")
        .clone()
    else {
        ctx.add_result(json!({
            "accepted": false,
            "connections_matched": connections.len(),
            "error": "Storage Manager init has not run, so no config handle is available to build providers",
        }));
        return;
    };
    let run_state = state.clone();
    tokio::spawn(async move {
        for connection in &connections {
            let _ = run_byo_health_check(&run_state, &config, connection).await;
        }
    });
    ctx.add_result(json!({
        "accepted": true,
        "queued": true,
        "connections_matched": connections.len(),
    }));
}

// ---------------------------------------------------------------------------
// BYO object operations. Every operation re-checks ownership,
// status, the allowed operation, the prefix scope, the file-type
// allowlists, and the quotas — the provider is never reached
// before those checks pass. Keys are relative to the connection
// prefix; the full provider key is always prefix + relative key.
// ---------------------------------------------------------------------------

/// Emit a metering delta for one BYO connection with the same
/// `storage.metering.reported` event the plan reports use, so
/// billing sees BYO usage exactly like plan usage.
fn emit_byo_metering(state: &StorageManagerState, connection: &ByoConnection) {
    state.emit(
        events::METERING_REPORTED,
        json!({
            "connection_id": connection.id.to_string(),
            "company_id": connection.company_id,
            "user_id": connection.owner_user,
            "period": Utc::now().format("%Y-%m").to_string(),
            "bytes_stored": connection.used_bytes,
            "files_stored": connection.used_files,
            "source": "byo_usage",
            "reported_at": Utc::now().to_rfc3339(),
        }),
    );
}

async fn list_byo_objects(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_BYO_USE) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = check_capability(&ctx, &found.role, PERM_MANAGE_BYO);
    let connection = match load_byo_connection_for(&state, &id, &found.actor, is_admin) {
        Ok(connection) => connection,
        Err(error) => return err_response(error, debug),
    };
    // Listing needs only an active connection that allows reads;
    // the full rule check (prefix, extensions, quotas) applies to
    // the individual object operations below.
    if connection.status != "active" {
        return err_response(
            UnifiedError::conflict(format!(
                "BYO connection \"{}\" is {} and cannot be used",
                connection.name, connection.status
            )),
            debug,
        );
    }
    if !connection.allow_read {
        return err_response(
            UnifiedError::forbidden(format!(
                "BYO connection \"{}\" does not allow read operations",
                connection.name
            )),
            debug,
        );
    }
    let params = params.into_inner();
    let prefix = params.filters.get("prefix").cloned().unwrap_or_default();
    let mut items: Vec<Value> = state
        .byo_objects
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|object| object.connection_id == connection.id)
        .filter(|object| prefix.is_empty() || object.key.starts_with(prefix.as_str()))
        .map(|object| {
            json!({
                "connection_id": object.connection_id,
                "key": object.key,
                "size_bytes": object.size_bytes,
                "mime": object.mime,
                "owner_user": object.owner_user,
                "company_id": object.company_id,
                "recorded": true,
                "created_at": object.created_at,
            })
        })
        .collect();
    let mut listing_supported = false;
    let mut listing_error: Option<String> = None;
    if let Ok(provider) = provider_for_byo(&ctx.config, &connection) {
        let list_prefix = if prefix.is_empty() {
            connection.path_prefix.clone()
        } else {
            format!("{}/{}", connection.path_prefix, prefix)
        };
        match provider.list(&list_prefix).await {
            Ok(keys) => {
                listing_supported = true;
                for key in keys {
                    let relative = key
                        .strip_prefix(&format!("{}/", connection.path_prefix))
                        .unwrap_or(&key)
                        .to_string();
                    if !items.iter().any(|item| {
                        item.get("key").and_then(Value::as_str) == Some(relative.as_str())
                    }) {
                        items.push(json!({
                            "connection_id": connection.id,
                            "key": relative,
                            "size_bytes": Value::Null,
                            "mime": Value::Null,
                            "owner_user": Value::Null,
                            "company_id": Value::Null,
                            "recorded": false,
                            "created_at": Value::Null,
                        }));
                    }
                }
            }
            Err(error) => {
                listing_error = Some(error.message.clone());
            }
        }
    }
    if let Some(search) = &params.search {
        let needle = search.to_lowercase();
        items.retain(|item| {
            item.get("key")
                .and_then(Value::as_str)
                .is_some_and(|key| key.to_lowercase().contains(&needle))
        });
    }
    items.sort_by(|a, b| {
        a.get("key")
            .and_then(Value::as_str)
            .cmp(&b.get("key").and_then(Value::as_str))
    });
    let page = paginate(items, &params);
    let mut value = page.to_json();
    if let Value::Object(map) = &mut value {
        map.insert(
            "listing_supported".to_string(),
            Value::Bool(listing_supported),
        );
        map.insert(
            "listing_error".to_string(),
            listing_error.map(Value::String).unwrap_or(Value::Null),
        );
    }
    ok(value)
}

async fn upload_byo_object(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    query: web::Query<ByoUploadQuery>,
    body: web::Bytes,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_BYO_USE) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = check_capability(&ctx, &found.role, PERM_MANAGE_BYO);
    let connection = match load_byo_connection_for(&state, &id, &found.actor, is_admin) {
        Ok(connection) => connection,
        Err(error) => return err_response(error, debug),
    };
    if body.len() > MAX_UPLOAD_BYTES {
        return err_response(
            UnifiedError::bad_request(format!(
                "Upload exceeds the maximum size of {MAX_UPLOAD_BYTES} bytes"
            )),
            debug,
        );
    }
    let mime = query
        .mime
        .clone()
        .unwrap_or_else(|| "application/octet-stream".to_string());
    let size = body.len() as i64;
    let checksum = sha256_hex(&body);
    // The net change this upload makes: an overwrite of an existing
    // key counts only the size delta and zero new files.
    let byo_key = byo_object_key(connection.id, &query.key);
    let previous_size = state
        .byo_objects
        .get(&byo_key)
        .map(|entry| entry.value().size_bytes)
        .unwrap_or(0);
    let is_new = !state.byo_objects.contains_key(&byo_key);
    let delta_bytes = size - previous_size;
    let delta_files = i64::from(is_new);
    let full_key = match check_byo_operation(
        &state,
        &connection,
        "write",
        &query.key,
        Some(&mime),
        delta_bytes,
        delta_files,
    ) {
        Ok(key) => key,
        Err(error) => return err_response(error, debug),
    };
    let provider = match provider_for_byo(&ctx.config, &connection) {
        Ok(provider) => provider,
        Err(error) => return err_response(error, debug),
    };
    // Reserve the connection quota before the provider call so two
    // concurrent uploads cannot both pass against the same counters;
    // the reservation is released again when the upload fails.
    if let Some(mut stored) = state.byo_connections.get_mut(&connection.id) {
        stored.used_bytes = (stored.used_bytes + delta_bytes).max(0);
        stored.used_files = (stored.used_files + delta_files).max(0);
        stored.updated_at = Utc::now();
    }
    let result: UnifiedResult<Value> = (|| async {
        if let Err(error) = provider.upload(&full_key, body.to_vec()).await {
            if let Some(mut stored) = state.byo_connections.get_mut(&connection.id) {
                stored.used_bytes = (stored.used_bytes - delta_bytes).max(0);
                stored.used_files = (stored.used_files - delta_files).max(0);
                stored.updated_at = Utc::now();
            }
            return Err(error);
        }
        let now = Utc::now();
        state.byo_objects.insert(
            byo_object_key(connection.id, &query.key),
            ByoObject {
                connection_id: connection.id,
                key: query.key.clone(),
                size_bytes: size,
                sha256_hex: Some(checksum.clone()),
                mime: mime.clone(),
                owner_user: connection.owner_user.clone(),
                company_id: connection.company_id.clone(),
                created_at: now,
            },
        );
        let updated = load_byo_connection(&state, &id)?;
        state.emit(
            events::OBJECT_STORED,
            json!({
                "connection_id": connection.id.to_string(),
                "key": query.key,
                "bytes": size,
                "sha256_hex": checksum,
                "mime": mime,
                "user_id": found.actor,
                "company_id": connection.company_id,
            }),
        );
        emit_byo_metering(&state, &updated);
        Ok(json!({
            "connection_id": connection.id,
            "key": query.key,
            "size_bytes": size,
            "sha256_hex": checksum,
            "mime": mime,
        }))
    })()
    .await;
    match result {
        Ok(value) => created(value),
        Err(error) => err_response(error, debug),
    }
}

async fn download_byo_object(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    query: web::Query<ObjectQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_BYO_USE) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = check_capability(&ctx, &found.role, PERM_MANAGE_BYO);
    let connection = match load_byo_connection_for(&state, &id, &found.actor, is_admin) {
        Ok(connection) => connection,
        Err(error) => return err_response(error, debug),
    };
    let full_key = match check_byo_operation(&state, &connection, "read", &query.key, None, 0, 0) {
        Ok(key) => key,
        Err(error) => return err_response(error, debug),
    };
    let provider = match provider_for_byo(&ctx.config, &connection) {
        Ok(provider) => provider,
        Err(error) => return err_response(error, debug),
    };
    let recorded = state
        .byo_objects
        .get(&byo_object_key(connection.id, &query.key))
        .and_then(|entry| entry.value().sha256_hex.clone());
    let downloaded = match &recorded {
        Some(expected) => provider.download_verified(&full_key, expected).await,
        None => provider.download(&full_key).await,
    };
    match downloaded {
        Ok(bytes) => {
            let mime = state
                .byo_objects
                .get(&byo_object_key(connection.id, &query.key))
                .map(|entry| entry.value().mime.clone())
                .unwrap_or_else(|| "application/octet-stream".to_string());
            let file_name = query
                .key
                .rsplit('/')
                .next()
                .unwrap_or(&query.key)
                .replace('"', "");
            let integrity = if recorded.is_some() {
                "verified"
            } else {
                "unverified"
            };
            HttpResponse::Ok()
                .content_type(mime)
                .insert_header((
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{file_name}\""),
                ))
                .insert_header(("x-object-integrity", integrity))
                .body(bytes)
        }
        Err(error) => {
            if error
                .metadata
                .get("checksum_mismatch")
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
            {
                state.emit(
                    events::OBJECT_CHECKSUM_MISMATCH,
                    json!({
                        "connection_id": connection.id.to_string(),
                        "key": query.key,
                        "expected_sha256": recorded,
                    }),
                );
            }
            err_response(error, debug)
        }
    }
}

async fn delete_byo_object(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    query: web::Query<ObjectQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let found = match authorize(&ctx, &req, PERM_BYO_USE) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = check_capability(&ctx, &found.role, PERM_MANAGE_BYO);
    let connection = match load_byo_connection_for(&state, &id, &found.actor, is_admin) {
        Ok(connection) => connection,
        Err(error) => return err_response(error, debug),
    };
    let full_key = match check_byo_operation(&state, &connection, "delete", &query.key, None, 0, 0) {
        Ok(key) => key,
        Err(error) => return err_response(error, debug),
    };
    let provider = match provider_for_byo(&ctx.config, &connection) {
        Ok(provider) => provider,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<Value> = (|| async {
        provider.delete(&full_key).await?;
        let removed = state
            .byo_objects
            .remove(&byo_object_key(connection.id, &query.key));
        let now = Utc::now();
        if let Some((_, object)) = removed {
            if let Some(mut stored) = state.byo_connections.get_mut(&connection.id) {
                stored.used_bytes = (stored.used_bytes - object.size_bytes).max(0);
                stored.used_files = (stored.used_files - 1).max(0);
                stored.updated_at = now;
            }
        }
        let updated = load_byo_connection(&state, &id)?;
        state.emit(
            events::OBJECT_DELETED,
            json!({
                "connection_id": connection.id.to_string(),
                "key": query.key,
                "user_id": found.actor,
                "company_id": connection.company_id,
            }),
        );
        emit_byo_metering(&state, &updated);
        Ok(json!({
            "deleted": true,
            "connection_id": connection.id,
            "key": query.key,
        }))
    })()
    .await;
    json::respond(result, debug)
}

// Module definition.
// ---------------------------------------------------------------------------

pub struct StorageManagerModule {
    state: StorageManagerState,
}

impl StorageManagerModule {
    pub fn new() -> Self {
        Self {
            state: StorageManagerState::default(),
        }
    }
}

impl Default for StorageManagerModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for StorageManagerModule {
    fn id(&self) -> &'static str {
        "storage_manager"
    }

    fn name(&self) -> &'static str {
        "Storage Manager"
    }

    fn description(&self) -> &'static str {
        "Storage hub: bucket registry, per-module bindings with a fallback chain, quotas, file browser, usage stats, lifecycle rules, revocable signed URLs, and health checks on top of the local, S3, R2, Google Drive, and SFTP providers. Sold as a product with storage plans (tiers, quotas, metering for billing) and bring-your-own storage connections under admin allowlist, validation, rules, and terms."
    }

    /// Storage Manager is sold as a product: a customer buys a
    /// module entitlement for it (see `crate::modules::licensing`).
    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("infrastructure")
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "company", "notifications"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "company", "notifications", "storage", "scheduler"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        *self
            .state
            .app_config
            .write()
            .expect("app config lock poisoned") = Some(Arc::clone(&ctx.config));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            events::UPLOAD_REQUESTED,
            "storage_manager",
            Arc::new(move |ctx: &mut HookContext| {
                on_upload_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            events::LIFECYCLE_RUN_REQUESTED,
            "storage_manager",
            Arc::new(move |ctx: &mut HookContext| {
                on_lifecycle_run_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            events::BYO_HEALTH_CHECK_REQUESTED,
            "storage_manager",
            Arc::new(move |ctx: &mut HookContext| {
                on_byo_health_check_requested(state.clone(), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/storage-manager")
                .app_data(state)
                .route("", web::get().to(get_status))
                .route("/defaults", web::get().to(get_defaults))
                .route("/defaults", web::post().to(set_default))
                .route("/buckets", web::get().to(list_buckets))
                .route("/buckets", web::post().to(create_bucket))
                .route("/buckets/{id}", web::get().to(get_bucket))
                .route("/buckets/{id}", web::patch().to(update_bucket))
                .route("/buckets/{id}", web::delete().to(delete_bucket))
                .route("/buckets/{id}/test", web::post().to(test_bucket))
                .route("/buckets/{id}/health", web::post().to(health_bucket))
                .route("/buckets/{id}/health", web::get().to(list_bucket_health))
                .route("/health", web::get().to(list_health))
                .route("/buckets/{id}/objects", web::get().to(list_objects))
                .route("/buckets/{id}/objects", web::post().to(upload_object))
                .route("/buckets/{id}/objects/download", web::get().to(download_object))
                .route("/buckets/{id}/objects", web::delete().to(delete_object))
                .route("/buckets/{id}/objects/move", web::post().to(move_object))
                .route("/buckets/{id}/objects/verify", web::post().to(verify_object))
                .route(
                    "/buckets/{id}/objects/orphans/cleanup",
                    web::post().to(cleanup_orphans),
                )
                .route("/buckets/{id}/signed-url", web::post().to(issue_signed_url))
                .route("/signed-urls", web::get().to(list_signed_urls))
                .route("/signed-urls/{id}/revoke", web::post().to(revoke_signed_url))
                .route("/download/{token}", web::get().to(redeem_download))
                .route("/bindings", web::get().to(list_bindings))
                .route("/bindings", web::post().to(create_binding))
                .route("/bindings/resolve", web::get().to(resolve_binding_endpoint))
                .route("/bindings/{id}", web::get().to(get_binding))
                .route("/bindings/{id}", web::patch().to(update_binding))
                .route("/bindings/{id}", web::delete().to(delete_binding))
                .route("/quotas", web::get().to(list_quotas))
                .route("/quotas", web::post().to(create_quota))
                .route("/quotas/{id}", web::get().to(get_quota))
                .route("/quotas/{id}", web::patch().to(update_quota))
                .route("/quotas/{id}", web::delete().to(delete_quota))
                .route("/stats", web::get().to(get_stats))
                .route("/stats/daily", web::get().to(get_stats_daily))
                .route("/stats/top", web::get().to(get_stats_top))
                .route("/lifecycle", web::get().to(list_lifecycle))
                .route("/lifecycle", web::post().to(create_lifecycle))
                .route("/lifecycle/run", web::post().to(run_lifecycle))
                .route("/lifecycle/{id}", web::get().to(get_lifecycle))
                .route("/lifecycle/{id}", web::patch().to(update_lifecycle))
                .route("/lifecycle/{id}", web::delete().to(delete_lifecycle))
                .route("/plans", web::get().to(list_plans))
                .route("/plans", web::post().to(create_plan))
                .route("/plans/public", web::get().to(list_public_plans))
                .route("/plans/my-plan", web::get().to(get_my_plan))
                .route("/plans/assignments", web::get().to(list_plan_assignments))
                .route("/plans/assignments/{id}", web::get().to(get_plan_assignment))
                .route("/plans/assignments/{id}/cancel", web::post().to(cancel_plan_assignment))
                .route("/plans/metering/report", web::post().to(report_metering))
                .route("/plans/metering", web::get().to(list_metering))
                .route("/plans/{id}", web::get().to(get_plan))
                .route("/plans/{id}", web::patch().to(update_plan))
                .route("/plans/{id}", web::delete().to(delete_plan))
                .route("/plans/{id}/assign", web::post().to(assign_plan))
                .route("/byo/settings", web::get().to(get_byo_settings))
                .route("/byo/settings", web::patch().to(update_byo_settings))
                .route("/byo/terms", web::get().to(list_byo_terms))
                .route("/byo/terms", web::post().to(create_byo_terms))
                .route("/byo/terms/current", web::get().to(get_current_byo_terms))
                .route("/byo/terms/{id}", web::get().to(get_byo_terms))
                .route("/byo/terms/{id}", web::patch().to(update_byo_terms))
                .route("/byo/terms/{id}", web::delete().to(delete_byo_terms))
                .route("/byo/terms/accept", web::post().to(accept_byo_terms))
                .route("/byo/terms/enforce-reacceptance", web::post().to(enforce_byo_reacceptance))
                .route("/byo/connections", web::get().to(list_byo_connections))
                .route("/byo/connections", web::post().to(create_byo_connection))
                .route("/byo/connections/{id}", web::get().to(get_byo_connection))
                .route("/byo/connections/{id}", web::patch().to(update_byo_connection))
                .route("/byo/connections/{id}", web::delete().to(delete_byo_connection))
                .route("/byo/connections/{id}/validate", web::post().to(validate_byo_connection))
                .route("/byo/connections/{id}/activate", web::post().to(activate_byo_connection))
                .route("/byo/connections/{id}/suspend", web::post().to(suspend_byo_connection))
                .route("/byo/connections/{id}/revoke", web::post().to(revoke_byo_connection))
                .route("/byo/connections/{id}/health", web::post().to(health_byo_connection))
                .route("/byo/connections/{id}/health", web::get().to(list_byo_connection_health))
                .route("/byo/connections/{id}/objects", web::get().to(list_byo_objects))
                .route("/byo/connections/{id}/objects", web::post().to(upload_byo_object))
                .route("/byo/connections/{id}/objects/download", web::get().to(download_byo_object))
                .route("/byo/connections/{id}/objects", web::delete().to(delete_byo_object))
                .route("/byo/health", web::get().to(list_byo_health))
                .route("/byo/health/run", web::post().to(run_byo_health)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/storage-manager", owner: "storage_manager", description: "Storage Manager status: counts, providers, defaults, product declaration." },
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/defaults", owner: "storage_manager", description: "Global + per-company default buckets." },
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/defaults", owner: "storage_manager", description: "Set a global or per-company default bucket." },
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/buckets", owner: "storage_manager", description: "List buckets (paginated, filter by provider_kind, company_id, enabled)." },
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/buckets", owner: "storage_manager", description: "Create a bucket (credentials write-only, encrypted)." },
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/buckets/{id}", owner: "storage_manager", description: "Get one bucket (secrets never returned)." },
            RouteInfo { method: "PATCH", path: "/api/v1/storage-manager/buckets/{id}", owner: "storage_manager", description: "Update a bucket (credentials replace when present)." },
            RouteInfo { method: "DELETE", path: "/api/v1/storage-manager/buckets/{id}", owner: "storage_manager", description: "Delete a bucket (refused while bindings or lifecycle rules reference it)." },
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/buckets/{id}/test", owner: "storage_manager", description: "Test a bucket connection." },
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/buckets/{id}/health", owner: "storage_manager", description: "Run a health check (connectivity + write/read/delete probe + cleanup)." },
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/buckets/{id}/health", owner: "storage_manager", description: "Recent health checks of one bucket." },
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/health", owner: "storage_manager", description: "Recent health checks (all buckets, filter by bucket_id, ok)." },
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/buckets/{id}/objects", owner: "storage_manager", description: "File browser: recorded objects + live provider listing where supported (paginated, filter by prefix, module, user_id)." },
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/buckets/{id}/objects", owner: "storage_manager", description: "Upload object bytes (?key=, raw body, quota-checked)." },
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/buckets/{id}/objects/download", owner: "storage_manager", description: "Download object bytes (?key=, raw body)." },
            RouteInfo { method: "DELETE", path: "/api/v1/storage-manager/buckets/{id}/objects", owner: "storage_manager", description: "Delete an object (?key=)." },
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/buckets/{id}/objects/move", owner: "storage_manager", description: "Move / rename an object within a bucket." },
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/buckets/{id}/objects/verify", owner: "storage_manager", description: "Verify one object against its recorded SHA-256 (backfills unrecorded checksums; mismatch reported as data)." },
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/buckets/{id}/objects/orphans/cleanup", owner: "storage_manager", description: "Delete provider objects with no recorded hub row (dry-run by default; needs a listable provider)." },
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/buckets/{id}/signed-url", owner: "storage_manager", description: "Issue a revocable hub download token (+ provider URL when supported)." },
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/signed-urls", owner: "storage_manager", description: "List issued signed-URL tokens (token values never shown)." },
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/signed-urls/{id}/revoke", owner: "storage_manager", description: "Revoke a signed-URL token." },
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/download/{token}", owner: "storage_manager", description: "Redeem a hub download token (expiry + revocation enforced; possession is the authorization)." },
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/bindings", owner: "storage_manager", description: "List bindings (paginated, filter by module_id, bucket_id, company_id, enabled)." },
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/bindings", owner: "storage_manager", description: "Create a binding (module -> bucket + key prefix)." },
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/bindings/resolve", owner: "storage_manager", description: "Resolve the binding fallback chain for a module (?module=&company_id=)." },
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/bindings/{id}", owner: "storage_manager", description: "Get one binding." },
            RouteInfo { method: "PATCH", path: "/api/v1/storage-manager/bindings/{id}", owner: "storage_manager", description: "Update a binding (emits storage.binding.changed)." },
            RouteInfo { method: "DELETE", path: "/api/v1/storage-manager/bindings/{id}", owner: "storage_manager", description: "Delete a binding (emits storage.binding.changed)." },
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/quotas", owner: "storage_manager", description: "List quotas (paginated, filter by scope, scope_id)." },
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/quotas", owner: "storage_manager", description: "Create a quota (company / user / module; soft + hard limits)." },
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/quotas/{id}", owner: "storage_manager", description: "Get one quota." },
            RouteInfo { method: "PATCH", path: "/api/v1/storage-manager/quotas/{id}", owner: "storage_manager", description: "Update quota limits." },
            RouteInfo { method: "DELETE", path: "/api/v1/storage-manager/quotas/{id}", owner: "storage_manager", description: "Delete a quota." },
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/stats", owner: "storage_manager", description: "Usage totals + per-bucket / company / user / module breakdowns." },
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/stats/daily", owner: "storage_manager", description: "Daily usage rollups (from, to, bucket_id, company_id, user_id, module)." },
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/stats/top", owner: "storage_manager", description: "Top consumers by bytes (?by=module|user|company|bucket, limit)." },
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/lifecycle", owner: "storage_manager", description: "List lifecycle rules (filter by bucket_id, enabled)." },
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/lifecycle", owner: "storage_manager", description: "Create a lifecycle rule (retention days, auto-delete, archive transition)." },
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/lifecycle/run", owner: "storage_manager", description: "Run lifecycle rules now (sync; optionally also enqueues scheduler job storage.lifecycle.run)." },
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/lifecycle/{id}", owner: "storage_manager", description: "Get one lifecycle rule." },
            RouteInfo { method: "PATCH", path: "/api/v1/storage-manager/lifecycle/{id}", owner: "storage_manager", description: "Update a lifecycle rule." },
            RouteInfo { method: "DELETE", path: "/api/v1/storage-manager/lifecycle/{id}", owner: "storage_manager", description: "Delete a lifecycle rule." },
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/plans", owner: "storage_manager", description: "List storage plans (admin; paginated, filter by is_public, enabled)." },
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/plans", owner: "storage_manager", description: "Create a storage plan (quota, provider kinds, product_id billing reference; never a money column)." },
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/plans/public", owner: "storage_manager", description: "List public enabled storage plans." },
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/plans/my-plan", owner: "storage_manager", description: "Current plan + usage vs quota for the caller (?company_id=&user_id= for admins)." },
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/plans/assignments", owner: "storage_manager", description: "List plan assignments (admin; filter by company_id, user_id, plan_id, status)." },
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/plans/assignments/{id}", owner: "storage_manager", description: "Get one plan assignment." },
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/plans/assignments/{id}/cancel", owner: "storage_manager", description: "Cancel a plan assignment." },
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/plans/metering/report", owner: "storage_manager", description: "Record a metering report for a buyer and emit storage.metering.reported for billing." },
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/plans/metering", owner: "storage_manager", description: "List metering reports (filter by company_id, user_id, period, plan_id)." },
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/plans/{id}", owner: "storage_manager", description: "Get one storage plan." },
            RouteInfo { method: "PATCH", path: "/api/v1/storage-manager/plans/{id}", owner: "storage_manager", description: "Update a storage plan." },
            RouteInfo { method: "DELETE", path: "/api/v1/storage-manager/plans/{id}", owner: "storage_manager", description: "Delete a storage plan (refused while active assignments exist)." },
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/plans/{id}/assign", owner: "storage_manager", description: "Assign or change a buyer's plan (usage re-checked against the new quota; 409 on exceed; entitlement-checked)." },
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/byo/settings", owner: "storage_manager", description: "BYO settings: provider allowlist, terms requirement, per-user connection limit." },
            RouteInfo { method: "PATCH", path: "/api/v1/storage-manager/byo/settings", owner: "storage_manager", description: "Update BYO settings (admin)." },
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/byo/terms", owner: "storage_manager", description: "List BYO terms versions (filter by is_current, enabled)." },
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/byo/terms", owner: "storage_manager", description: "Create a BYO terms version (admin)." },
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/byo/terms/current", owner: "storage_manager", description: "Current BYO terms version." },
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/byo/terms/{id}", owner: "storage_manager", description: "Get one BYO terms version." },
            RouteInfo { method: "PATCH", path: "/api/v1/storage-manager/byo/terms/{id}", owner: "storage_manager", description: "Update a BYO terms version (incl. re-acceptance deadline)." },
            RouteInfo { method: "DELETE", path: "/api/v1/storage-manager/byo/terms/{id}", owner: "storage_manager", description: "Delete a BYO terms version (refused while current or accepted)." },
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/byo/terms/accept", owner: "storage_manager", description: "Accept the current BYO terms version (recorded with version + timestamp)." },
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/byo/terms/enforce-reacceptance", owner: "storage_manager", description: "Suspend connections whose owner missed the terms re-acceptance deadline (admin)." },
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/byo/connections", owner: "storage_manager", description: "List BYO connections (owner-scoped; admins see all; filter by status, provider_kind, company_id)." },
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/byo/connections", owner: "storage_manager", description: "Create a BYO connection (allowlist-checked, credentials write-only, prefix-scoped)." },
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/byo/connections/{id}", owner: "storage_manager", description: "Get one BYO connection (cross-owner access answers 404; secrets never returned)." },
            RouteInfo { method: "PATCH", path: "/api/v1/storage-manager/byo/connections/{id}", owner: "storage_manager", description: "Update a BYO connection (credential replacement resets it to draft for re-validation)." },
            RouteInfo { method: "DELETE", path: "/api/v1/storage-manager/byo/connections/{id}", owner: "storage_manager", description: "Delete (revoke) a BYO connection." },
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/byo/connections/{id}/validate", owner: "storage_manager", description: "Validate a BYO connection (connect + list test prefix + write/read/delete probe)." },
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/byo/connections/{id}/activate", owner: "storage_manager", description: "Activate a BYO connection (requires validation, allowlist, terms acceptance, entitlement)." },
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/byo/connections/{id}/suspend", owner: "storage_manager", description: "Suspend a BYO connection (admin or owner)." },
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/byo/connections/{id}/revoke", owner: "storage_manager", description: "Revoke a BYO connection (terminal; admin or owner)." },
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/byo/connections/{id}/health", owner: "storage_manager", description: "Run a BYO health check (connectivity + write/read/delete probe + cleanup)." },
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/byo/connections/{id}/health", owner: "storage_manager", description: "Recent health checks of one BYO connection." },
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/byo/connections/{id}/objects", owner: "storage_manager", description: "List objects through a BYO connection (prefix-scoped, recorded + live listing where supported)." },
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/byo/connections/{id}/objects", owner: "storage_manager", description: "Upload object bytes through a BYO connection (?key=, prefix-scoped, rules + quotas enforced)." },
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/byo/connections/{id}/objects/download", owner: "storage_manager", description: "Download object bytes through a BYO connection (?key=, prefix-scoped)." },
            RouteInfo { method: "DELETE", path: "/api/v1/storage-manager/byo/connections/{id}/objects", owner: "storage_manager", description: "Delete an object through a BYO connection (?key=, prefix-scoped)." },
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/byo/health", owner: "storage_manager", description: "Recent BYO health checks (owner-scoped; filter by connection_id, ok)." },
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/byo/health/run", owner: "storage_manager", description: "Run BYO health checks now (sync; optionally also enqueues scheduler job storage.byo.health_check)." },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(StorageManagerModule::new()));
}
