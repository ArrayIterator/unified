// Contracts & E-Signature module — contract templates with
// placeholders, contracts with a validated status machine, an
// in-app signing flow with unguessable token links, signing order,
// a full audit trail, expiry / renewal alerts, and a billing
// linkage event when a contract becomes active.
//
// Routes live under `/api/v1/contracts`:
//
//   Templates:
//     GET/POST /templates — list templates / create a template.
//     GET/PATCH/DELETE /templates/{id}.
//     POST /templates/{id}/render — render a template body with
//       supplied values (values are escaped; unknown or missing
//       placeholders are rejected).
//   Contracts:
//     GET/POST / — list contracts / create a contract.
//     GET/PATCH/DELETE /{id}.
//     POST /{id}/send — draft -> sent (signing links are issued).
//     POST /{id}/activate — signed -> active (emits the billing
//       linkage event; no invoice is created here).
//     POST /{id}/void — void a contract (audited; terminal).
//     POST /{id}/terminate — end a contract early (audited).
//     POST /{id}/check-expiry — materialise expiry when the end
//       date passed (also driven by the scheduler listener).
//     POST /{id}/renew — create a new draft contract copying the
//       terms, parties, and value of this one.
//     GET  /{id}/audit — full audit trail of the contract.
//   Signers:
//     GET  /{id}/signers — list signers of a contract.
//     POST /{id}/signers — add a signer (draft / sent only).
//     DELETE /{id}/signers/{signer_id} — remove a pending signer.
//     POST /{id}/signers/{signer_id}/resend — issue a fresh
//       signing token (old token stops working; audited).
//   Signing links (token-gated; see the public-access note):
//     GET  /sign/{token} — read the contract a signer was asked
//       to sign, plus that signer's status.
//     POST /sign/{token} — sign (sequential order enforced).
//     POST /sign/{token}/decline — decline with a reason.
//   Settings:
//     GET/PATCH /settings — expiry alert offsets in days before
//       the end date (default 30/14/7/1).
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory template, contract, signer, and audit-trail
// management; server-side template rendering with escaped values
// and strict placeholder validation; the contract status machine
// (draft -> sent -> partially_signed -> signed -> active ->
// expired / terminated, with voided as an audited terminal state);
// token-based signing links (48 random characters, only the
// SHA-256 hash is stored, 30-day default expiry); sequential and
// parallel signing order; a sealed content hash recorded when the
// last signer signs, after which edits are rejected (the audited
// escape hatch is voiding the contract and renewing it into a new
// draft); an audit trail of every view / sign / decline / send /
// activate / void / terminate / expire / renew action carrying the
// actor, a SHA-256 hash of the IP, a SHA-256 hash of the user
// agent, and a timestamp — raw IPs and raw user agents are never
// stored; expiry alerts queued through `scheduler.enqueue` with
// kind `contracts.expiry_alert` and delivered through
// `notification.requested`; and a `billing.contract_activated`
// event so the billing module can create invoices. What is NOT
// built here: this is an in-app signing flow with a full audit
// trail, NOT a certified third-party e-signature provider — there
// are no qualified certificates and no legal eIDAS / QES claim;
// invoices are never created here (billing owns them); no email
// or push delivery happens here (the notifications module owns
// delivery — this module only emits `notification.requested`);
// and no PDF rendering of the signed contract exists.
//
// Public-access gate: the API Manager module enforces public
// access through its admin-managed `ApiAccessRule` records
// (default deny). The three `/sign/{token}` routes carry no
// capability check on purpose: the unguessable 48-character token
// in the path IS the credential, it expires, and every access is
// written to the audit trail. An administrator must create the
// matching `ApiAccessRule` rows for
// `GET|POST /api/v1/contracts/sign/*` for the links to work at
// all. Do NOT treat the absence of a capability check on those
// routes as an open door: without the rule the API Manager denies
// the call, and without the token nothing is disclosed (an
// unknown or expired token answers 404 either way, so tokens
// cannot be probed apart).
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the ai module. Capability checks use the
// `role.check` hook and fail closed (no answer means not allowed).
//
// Events emitted on the hook bus:
//   - `contracts.created`, `contracts.sent`, `contracts.signed`,
//     `contracts.activated`, `contracts.voided`,
//     `contracts.terminated`, `contracts.expired`,
//     `contracts.renewed`, `contracts.deleted`
//   - `contracts.signer.signed`, `contracts.signer.declined`,
//     `contracts.signer.added`, `contracts.signer.removed`,
//     `contracts.signer.token_resent`
//   - `contracts.template.created`, `contracts.template.updated`,
//     `contracts.template.deleted`
//   - `billing.contract_activated` — payload `{"contract_id",
//     "value", "currency", "counterparty_name",
//     "counterparty_email", "starts_at", "ends_at"}` for the
//     billing module to invoice against. No invoice is created
//     here.
//   - `notification.requested` (expiry alerts) with the
//     notifications module's payload shape.
//   - `scheduler.enqueue` with `{"kind":
//     "contracts.expiry_alert", "payload": {"contract_id",
//     "days_before", "send_at"}}` when a contract is sent or
//     activated.
// Events this module listens to:
//   - `contracts.expiry_alert_requested` — the event the
//     scheduler emits when a `contracts.expiry_alert` job fires,
//     following its naming rule (`backup.run` ->
//     `backup.run_requested`); sends the due alert through
//     `notification.requested`, and marks the contract expired
//     (emitting `contracts.expired`) when its end date passed.
//
// Scheduler note: `contracts.expiry_alert` is NOT in the
// scheduler's `KNOWN_KINDS` list yet, so today the scheduler
// answers `{"queued": false, "error": "Unknown job kind: ..."}` and
// the contract records `expiry_alert_status: "declined"` instead of
// pretending it was scheduled. The contract is still stored and the
// alert can be driven manually through `POST /{id}/check-expiry`;
// the follow-up is to add `"contracts.expiry_alert"` to
// `KNOWN_KINDS` in `scheduler.rs` and arm the dispatch so the job
// emits `contracts.expiry_alert_requested` with the alert payload
// (see the integrator snippet in the module report).

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use rand::{distr::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Digest;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
const MAX_NAME_LEN: usize = 200;
const MAX_TITLE_LEN: usize = 200;
const MAX_BODY_LEN: usize = 100_000;
const MAX_TERMS_LEN: usize = 100_000;
const MAX_EMAIL_LEN: usize = 254;
const MAX_REASON_LEN: usize = 2000;
const MAX_PLACEHOLDERS: usize = 100;
const MAX_PLACEHOLDER_NAME_LEN: usize = 64;
const MAX_SIGNERS_PER_CONTRACT: usize = 20;
const MAX_ALERT_DAYS: usize = 10;
/// Signing token: 48 random alphanumeric characters (~285 bits).
const SIGNING_TOKEN_LEN: usize = 48;
/// Default signing-link lifetime in days.
const DEFAULT_TOKEN_TTL_DAYS: i64 = 30;
/// Default expiry alert offsets (days before the end date).
const DEFAULT_ALERT_DAYS: [i64; 4] = [30, 14, 7, 1];

/// Capability names checked through the `role.check` hook.
const PERM_CONTRACTS_VIEW: &str = "contracts.view";
const PERM_CONTRACTS_MANAGE: &str = "contracts.manage";

/// Hook event names this module listens to.
const EXPIRY_ALERT_REQUESTED: &str = "contracts.expiry_alert_requested";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Lifecycle of a contract: `draft` -> `sent` ->
/// `partially_signed` -> `signed` -> `active` -> `expired` /
/// `terminated`; `voided` is reachable from draft / sent /
/// `partially_signed` / `signed` / `active` and is terminal.
/// `expired` is materialised by the scheduler listener (or the
/// manual check-expiry endpoint) once the end date passed while
/// the contract was `active`; it is also derived at read time so
/// a list never shows a stale `active` past its end date.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContractStatus {
    Draft,
    Sent,
    PartiallySigned,
    Signed,
    Active,
    Expired,
    Terminated,
    Voided,
}

impl Default for ContractStatus {
    fn default() -> Self {
        Self::Draft
    }
}

impl ContractStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Sent => "sent",
            Self::PartiallySigned => "partially_signed",
            Self::Signed => "signed",
            Self::Active => "active",
            Self::Expired => "expired",
            Self::Terminated => "terminated",
            Self::Voided => "voided",
        }
    }
}

/// Per-signer status: `pending` -> `viewed` -> `signed`, or
/// `declined` (terminal for that signer; the contract stays
/// signable by the remaining signers unless it is voided).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignerStatus {
    Pending,
    Viewed,
    Signed,
    Declined,
}

impl Default for SignerStatus {
    fn default() -> Self {
        Self::Pending
    }
}

impl SignerStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Viewed => "viewed",
            Self::Signed => "signed",
            Self::Declined => "declined",
        }
    }
}

/// Signing order: `sequential` signs in `order_index` order
/// (a signer may only sign once every lower index signed);
/// `parallel` lets every signer sign in any order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SigningOrder {
    Sequential,
    Parallel,
}

impl Default for SigningOrder {
    fn default() -> Self {
        Self::Sequential
    }
}

impl SigningOrder {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Sequential => "sequential",
            Self::Parallel => "parallel",
        }
    }
}

/// One contract template. `body` holds the template text with
/// `{{placeholder}}` markers; `placeholders` is the closed list
/// of names the body may use — any other marker in the body is
/// rejected at save time. Values are substituted server-side and
/// HTML-escaped at render time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractTemplate {
    pub id: Uuid,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub body: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub placeholders: Vec<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contract {
    pub id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_id: Option<Uuid>,
    pub title: String,
    /// The company side of the contract.
    pub company_party: String,
    pub counterparty_name: String,
    pub counterparty_email: String,
    pub terms: String,
    /// Monetary value in `currency` (NUMERIC(19,4) in the
    /// migration; non-negative, rounded to 4 decimals here).
    pub value: f64,
    pub currency: String,
    pub signing_order: SigningOrder,
    pub status: ContractStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starts_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ends_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed_at: Option<DateTime<Utc>>,
    /// SHA-256 hash of the sealed content, recorded when the last
    /// signer signs. While set, the contract cannot be edited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub void_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminate_reason: Option<String>,
    /// Id of the contract this one was renewed from, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renewed_from_id: Option<Uuid>,
    /// Expiry alert offsets (days before `ends_at`) armed for
    /// this contract; defaults to the module settings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alert_days: Vec<i64>,
    /// `not_scheduled`, `queued`, or `declined` (see the scheduler
    /// note in the module header).
    pub expiry_alert_status: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One signer of a contract. Only the SHA-256 hash of the signing
/// token is stored — the raw token is returned once, at creation
/// and at resend, and never persisted or logged.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractSigner {
    pub id: Uuid,
    pub contract_id: Uuid,
    pub name: String,
    pub email: String,
    /// 0-based position in the signing order.
    pub order_index: i64,
    pub status: SignerStatus,
    /// SHA-256 hex of the current signing token. Serialized as
    /// `token_hash` (never the raw token).
    pub token_hash: String,
    pub token_expires_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declined_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One audit-trail entry. `ip_hash` and `user_agent_hash` are
/// SHA-256 hex digests; the raw IP address and raw user agent are
/// never stored.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractAuditEntry {
    pub id: Uuid,
    pub contract_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer_id: Option<Uuid>,
    pub actor: String,
    pub action: String,
    pub ip_hash: String,
    pub user_agent_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
    pub created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateTemplateRequest {
    pub name: String,
    pub description: Option<String>,
    pub body: String,
    #[serde(default)]
    pub placeholders: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateTemplateRequest {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub body: Option<String>,
    pub placeholders: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct RenderTemplateRequest {
    #[serde(default)]
    pub values: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateContractRequest {
    pub template_id: Option<Uuid>,
    pub title: String,
    pub company_party: String,
    pub counterparty_name: String,
    pub counterparty_email: String,
    pub terms: String,
    pub value: f64,
    pub currency: Option<String>,
    pub signing_order: Option<SigningOrder>,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub alert_days: Option<Vec<i64>>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateContractRequest {
    pub title: Option<String>,
    pub company_party: Option<String>,
    pub counterparty_name: Option<String>,
    pub counterparty_email: Option<String>,
    pub terms: Option<String>,
    pub value: Option<f64>,
    pub currency: Option<String>,
    pub signing_order: Option<SigningOrder>,
    pub starts_at: Option<Option<DateTime<Utc>>>,
    pub ends_at: Option<Option<DateTime<Utc>>>,
    pub alert_days: Option<Vec<i64>>,
}

#[derive(Debug, Deserialize)]
pub struct AddSignerRequest {
    pub name: String,
    pub email: String,
    #[serde(default)]
    pub order_index: Option<i64>,
    /// Signing-link lifetime in days (1..=90, default 30).
    pub expires_in_days: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct SignRequest {
    /// Optional confirmation that the signer reviewed the terms.
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Debug, Deserialize)]
pub struct DeclineRequest {
    pub reason: String,
}

#[derive(Debug, Deserialize)]
pub struct VoidRequest {
    pub reason: String,
}

#[derive(Debug, Deserialize)]
pub struct TerminateRequest {
    pub reason: String,
}

#[derive(Debug, Deserialize)]
pub struct RenewRequest {
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSettingsRequest {
    pub alert_days: Vec<i64>,
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for contracts. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers tolerate a
/// missing bus by skipping event emission. Durable rows for these
/// types are defined by migration
/// `2026-11-22-000048_contracts`; until that migration is
/// executed, contracts do not survive a restart.
#[derive(Clone)]
pub struct ContractsState {
    pub templates: Arc<DashMap<Uuid, ContractTemplate>>,
    pub contracts: Arc<DashMap<Uuid, Contract>>,
    pub signers: Arc<DashMap<Uuid, ContractSigner>>,
    pub audit_trail: Arc<DashMap<Uuid, ContractAuditEntry>>,
    pub alert_days: Arc<RwLock<Vec<i64>>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for ContractsState {
    fn default() -> Self {
        Self {
            templates: Arc::new(DashMap::new()),
            contracts: Arc::new(DashMap::new()),
            signers: Arc::new(DashMap::new()),
            audit_trail: Arc::new(DashMap::new()),
            alert_days: Arc::new(RwLock::new(DEFAULT_ALERT_DAYS.to_vec())),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl ContractsState {
    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    /// Emit a `notification.requested` event in the notifications
    /// module's payload shape.
    fn notify(&self, kind: &str, user_id: &str, title: &str, body: &str, link: Option<String>) {
        self.emit(
            "notification.requested",
            json!({
                "kind": kind,
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "contracts",
                "category": "contracts",
                "priority": "normal",
                "link": link,
            }),
        );
    }

    fn default_alert_days(&self) -> Vec<i64> {
        self.alert_days
            .read()
            .expect("alert days lock poisoned")
            .clone()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Caller identity: `X-User-Id` (required) plus `X-Role` (required
/// for capability checks), exactly like the ai module.
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
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the ai module's capability check).
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

fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

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

fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

fn err(error: UnifiedError, debug: bool) -> HttpResponse {
    json::error_response(&error, debug)
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

fn valid_email(email: &str) -> bool {
    if email.is_empty() || email.len() > MAX_EMAIL_LEN || email.contains(' ') {
        return false;
    }
    let mut parts = email.split('@');
    let (Some(local), Some(domain)) = (parts.next(), parts.next()) else {
        return false;
    };
    if parts.next().is_some() || local.is_empty() || domain.is_empty() {
        return false;
    }
    domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
}

/// Money: finite, non-negative, rounded to 4 decimal places.
fn valid_price(price: f64) -> UnifiedResult<f64> {
    if !price.is_finite() || price < 0.0 {
        return Err(UnifiedError::bad_request("price must be a finite number >= 0"));
    }
    Ok((price * 10_000.0).round() / 10_000.0)
}

/// Currency: 3-letter ISO code, uppercased.
fn valid_currency(currency: &str) -> UnifiedResult<String> {
    let upper = currency.trim().to_uppercase();
    if upper.len() != 3 || !upper.chars().all(|c| c.is_ascii_uppercase()) {
        return Err(UnifiedError::bad_request(
            "currency must be a 3-letter ISO code such as \"USD\"",
        ));
    }
    Ok(upper)
}

/// SHA-256 hex digest. Used for signing-token storage, content
/// sealing, and IP / user-agent hashing in the audit trail — the
/// raw value is never stored or logged.
fn hash_value(value: &str) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(value.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// One unguessable signing token (48 random alphanumeric
/// characters). Only its SHA-256 hash is ever stored.
fn generate_token() -> String {
    rand::rng()
        .sample_iter(&Alphanumeric)
        .take(SIGNING_TOKEN_LEN)
        .map(char::from)
        .collect()
}

/// Escape a substituted value for safe inclusion in rendered
/// template output.
fn escape_html(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(ch),
        }
    }
    out
}

/// A placeholder name is `{{name}}` with `name` made of letters,
/// digits, and underscores only.
fn valid_placeholder_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_PLACEHOLDER_NAME_LEN
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Extract every `{{name}}` marker from `body`, in order of first
/// appearance. Malformed markers (unclosed `{{`, empty name,
/// illegal characters) are rejected.
fn extract_placeholders(body: &str) -> UnifiedResult<Vec<String>> {
    let mut found: Vec<String> = Vec::new();
    let mut rest = body;
    while let Some(open) = rest.find("{{") {
        let after_open = &rest[open + 2..];
        let Some(close) = after_open.find("}}") else {
            return Err(UnifiedError::bad_request(
                "Template body contains an unclosed \"{{\" placeholder",
            ));
        };
        let name = after_open[..close].trim();
        if !valid_placeholder_name(name) {
            return Err(UnifiedError::bad_request(format!(
                "Invalid placeholder name \"{name}\": use letters, digits, and underscores only"
            )));
        }
        if !found.iter().any(|entry| entry == name) {
            found.push(name.to_string());
        }
        rest = &after_open[close + 2..];
    }
    Ok(found)
}

/// Validate a template body against its declared placeholder list:
/// every marker in the body must be declared, and the declared
/// list must be within bounds. Unknown placeholders are rejected
/// at save time, never silently dropped.
fn validate_template_body(body: &str, placeholders: &[String]) -> UnifiedResult<()> {
    if body.len() > MAX_BODY_LEN {
        return Err(UnifiedError::bad_request(format!(
            "body must be at most {MAX_BODY_LEN} characters"
        )));
    }
    if placeholders.len() > MAX_PLACEHOLDERS {
        return Err(UnifiedError::bad_request(format!(
            "at most {MAX_PLACEHOLDERS} placeholders are allowed"
        )));
    }
    for name in placeholders {
        if !valid_placeholder_name(name) {
            return Err(UnifiedError::bad_request(format!(
                "Invalid placeholder name \"{name}\": use letters, digits, and underscores only"
            )));
        }
    }
    let used = extract_placeholders(body)?;
    for name in used {
        if !placeholders.iter().any(|declared| declared == &name) {
            return Err(UnifiedError::bad_request(format!(
                "Placeholder \"{{{{{name}}}}}\" is used in the body but not declared in \"placeholders\""
            )));
        }
    }
    Ok(())
}

/// Render a template body with `values`: every declared
/// placeholder must be supplied, unknown keys are rejected, and
/// every substituted value is HTML-escaped.
fn render_template(body: &str, placeholders: &[String], values: &HashMap<String, String>) -> UnifiedResult<String> {
    for key in values.keys() {
        if !placeholders.iter().any(|declared| declared == key) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown placeholder \"{key}\" for this template"
            )));
        }
    }
    for name in placeholders {
        if !values.contains_key(name) {
            return Err(UnifiedError::bad_request(format!(
                "Missing value for placeholder \"{name}\""
            )));
        }
    }
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(open) = rest.find("{{") {
        out.push_str(&rest[..open]);
        let after_open = &rest[open + 2..];
        let close = after_open
            .find("}}")
            .expect("placeholder validated before render");
        let name = after_open[..close].trim();
        let value = values.get(name).cloned().unwrap_or_default();
        out.push_str(&escape_html(&value));
        rest = &after_open[close + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Validate an alert-days list: 1..=365, at most 10 entries,
/// strictly descending (largest offset first), no duplicates.
fn validate_alert_days(days: &[i64]) -> UnifiedResult<Vec<i64>> {
    if days.is_empty() || days.len() > MAX_ALERT_DAYS {
        return Err(UnifiedError::bad_request(format!(
            "alert_days must contain 1..={MAX_ALERT_DAYS} entries"
        )));
    }
    let mut sorted = days.to_vec();
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    sorted.dedup();
    if sorted.len() != days.len() {
        return Err(UnifiedError::bad_request(
            "alert_days must not contain duplicates",
        ));
    }
    if days != sorted.as_slice() {
        return Err(UnifiedError::bad_request(
            "alert_days must be sorted in descending order (largest offset first)",
        ));
    }
    for day in days {
        if *day < 1 || *day > 365 {
            return Err(UnifiedError::bad_request(
                "alert_days entries must be between 1 and 365",
            ));
        }
    }
    Ok(days.to_vec())
}

/// Best-effort client address for audit hashing: the first entry
/// of `X-Forwarded-For` when present, else the peer address. The
/// caller hashes the result; the raw value is never stored.
fn request_ip(req: &HttpRequest) -> String {
    if let Some(forwarded) = req.headers().get("x-forwarded-for") {
        if let Ok(value) = forwarded.to_str() {
            if let Some(first) = value.split(',').next() {
                let trimmed = first.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
            }
        }
    }
    req.connection_info()
        .realip_remote_addr()
        .unwrap_or("")
        .to_string()
}

fn request_user_agent(req: &HttpRequest) -> String {
    req.headers()
        .get("user-agent")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string()
}

/// Append one audit-trail entry. `req` may be `None` for
/// scheduler-driven entries (IP / user-agent hashes are then the
/// hash of the empty string, which is still unambiguous in the
/// trail because the actor is `scheduler`).
fn record_audit(
    state: &ContractsState,
    contract_id: Uuid,
    signer_id: Option<Uuid>,
    actor: &str,
    action: &str,
    req: Option<&HttpRequest>,
    details: Option<String>,
) {
    let (ip_hash, user_agent_hash) = match req {
        Some(request) => (
            hash_value(&request_ip(request)),
            hash_value(&request_user_agent(request)),
        ),
        None => (hash_value(""), hash_value("")),
    };
    let entry = ContractAuditEntry {
        id: Uuid::now_v7(),
        contract_id,
        signer_id,
        actor: actor.to_string(),
        action: action.to_string(),
        ip_hash,
        user_agent_hash,
        details,
        created_at: Utc::now(),
    };
    state.audit_trail.insert(entry.id, entry);
}

fn load_template(state: &ContractsState, raw_id: &str) -> UnifiedResult<ContractTemplate> {
    let id = parse_uuid(raw_id, "template")?;
    state
        .templates
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Template not found: {id}")))
}

fn load_contract(state: &ContractsState, raw_id: &str) -> UnifiedResult<Contract> {
    let id = parse_uuid(raw_id, "contract")?;
    state
        .contracts
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Contract not found: {id}")))
}

fn load_signer(state: &ContractsState, contract_id: Uuid, raw_id: &str) -> UnifiedResult<ContractSigner> {
    let id = parse_uuid(raw_id, "signer")?;
    state
        .signers
        .get(&id)
        .map(|entry| entry.value().clone())
        .filter(|signer| signer.contract_id == contract_id)
        .ok_or_else(|| UnifiedError::not_found(format!("Signer not found: {id}")))
}

fn signers_of(state: &ContractsState, contract_id: Uuid) -> Vec<ContractSigner> {
    let mut signers: Vec<ContractSigner> = state
        .signers
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|signer| signer.contract_id == contract_id)
        .collect();
    signers.sort_by_key(|signer| (signer.order_index, signer.created_at));
    signers
}

/// Effective status at read time: an `active` contract whose end
/// date passed reads as `expired` (the stored status is
/// unchanged; the scheduler listener or the check-expiry endpoint
/// materialises it).
fn effective_status(contract: &Contract, now: DateTime<Utc>) -> ContractStatus {
    if contract.status == ContractStatus::Active {
        if let Some(ends_at) = contract.ends_at {
            if now >= ends_at {
                return ContractStatus::Expired;
            }
        }
    }
    contract.status
}

/// Canonical content hash recorded when a contract is sealed
/// (last signer signs). Any later content change would produce a
/// different hash, so the stored hash proves the sealed content.
fn seal_content_hash(contract: &Contract) -> String {
    hash_value(&format!(
        "{}|{}|{}|{}|{}|{:.4}|{}|{}|{}",
        contract.id,
        contract.title,
        contract.company_party,
        contract.counterparty_name,
        contract.counterparty_email,
        contract.value,
        contract.currency,
        contract.terms,
        contract
            .starts_at
            .map(|value| value.to_rfc3339())
            .unwrap_or_default(),
    ))
}

/// Validate a status transition. Returns the new status or a 409
/// conflict naming the illegal move.
fn transition(
    contract: &Contract,
    target: ContractStatus,
    action: &str,
) -> UnifiedResult<ContractStatus> {
    let allowed = match contract.status {
        ContractStatus::Draft => [ContractStatus::Sent, ContractStatus::Voided].contains(&target),
        ContractStatus::Sent => [
            ContractStatus::PartiallySigned,
            ContractStatus::Terminated,
            ContractStatus::Voided,
        ]
        .contains(&target),
        ContractStatus::PartiallySigned => [
            ContractStatus::Signed,
            ContractStatus::Terminated,
            ContractStatus::Voided,
        ]
        .contains(&target),
        ContractStatus::Signed => [
            ContractStatus::Active,
            ContractStatus::Terminated,
            ContractStatus::Voided,
        ]
        .contains(&target),
        ContractStatus::Active => [
            ContractStatus::Expired,
            ContractStatus::Terminated,
            ContractStatus::Voided,
        ]
        .contains(&target),
        ContractStatus::Expired | ContractStatus::Terminated | ContractStatus::Voided => false,
    };
    if !allowed {
        return Err(UnifiedError::conflict(format!(
            "Cannot {action} a contract in status \"{}\" (target \"{}\")",
            contract.status.as_str(),
            target.as_str(),
        )));
    }
    Ok(target)
}

/// Queue one expiry alert per armed offset through
/// `scheduler.enqueue` with kind `contracts.expiry_alert`. Mirrors
/// the events module's `schedule_reminder`: the scheduler answers
/// `{"queued": true, "job_id": ...}` when it knows the kind, and
/// `{"queued": false, "error": "Unknown job kind: ..."}` today,
/// because `contracts.expiry_alert` is not in `KNOWN_KINDS` yet.
/// The contract records `expiry_alert_status` honestly (`queued`
/// / `declined`) instead of pretending it was scheduled.
fn schedule_expiry_alerts(state: &ContractsState, contract: &mut Contract) {
    let Some(ends_at) = contract.ends_at else {
        contract.expiry_alert_status = "not_scheduled".to_string();
        return;
    };
    let Some(bus) = state.bus() else {
        contract.expiry_alert_status = "declined".to_string();
        return;
    };
    let mut any_queued = false;
    let mut any_declined = false;
    for days_before in &contract.alert_days {
        let send_at = ends_at - Duration::days(*days_before);
        if send_at <= Utc::now() {
            continue;
        }
        let answer = bus.emit(
            "scheduler.enqueue",
            json!({
                "kind": "contracts.expiry_alert",
                "payload": {
                    "contract_id": contract.id.to_string(),
                    "days_before": days_before,
                    "send_at": send_at.to_rfc3339(),
                },
            }),
        );
        match answer.results.first() {
            Some(result) if result.get("queued").and_then(Value::as_bool) == Some(true) => {
                any_queued = true;
            }
            _ => {
                any_declined = true;
            }
        }
    }
    contract.expiry_alert_status = if any_queued {
        "queued".to_string()
    } else {
        "declined".to_string()
    };
}

/// Materialise expiry for one contract: when the end date passed
/// while `active`, move it to `expired`, emit
/// `contracts.expired`, notify the creator, and audit the move.
/// Returns true when the contract was expired by this call.
fn materialise_expiry(state: &ContractsState, contract: &mut Contract) -> bool {
    let now = Utc::now();
    if contract.status != ContractStatus::Active {
        return false;
    }
    let Some(ends_at) = contract.ends_at else {
        return false;
    };
    if now < ends_at {
        return false;
    }
    contract.status = ContractStatus::Expired;
    contract.updated_at = now;
    state.emit(
        "contracts.expired",
        json!({
            "contract_id": contract.id.to_string(),
            "title": contract.title,
            "ends_at": ends_at.to_rfc3339(),
        }),
    );
    state.notify(
        "contract.expired",
        &contract.created_by,
        &format!("Contract expired: {}", contract.title),
        &format!(
            "Contract \"{}\" with {} reached its end date and is now expired.",
            contract.title, contract.counterparty_name
        ),
        Some(format!("/api/v1/contracts/{}", contract.id)),
    );
    record_audit(
        state,
        contract.id,
        None,
        "scheduler",
        "expired",
        None,
        Some(format!("End date {} passed", ends_at.to_rfc3339())),
    );
    true
}

/// Public view of a contract: the sealed hash is exposed (it
/// proves the sealed content) but no signer token material ever
/// appears here — tokens live only on signer rows as hashes.
fn contract_view(state: &ContractsState, contract: &Contract) -> Value {
    let now = Utc::now();
    let signers = signers_of(state, contract.id);
    let signed_count = signers
        .iter()
        .filter(|signer| signer.status == SignerStatus::Signed)
        .count();
    json!({
        "id": contract.id.to_string(),
        "template_id": contract.template_id.map(|id| id.to_string()),
        "title": contract.title,
        "company_party": contract.company_party,
        "counterparty_name": contract.counterparty_name,
        "counterparty_email": contract.counterparty_email,
        "terms": contract.terms,
        "value": contract.value,
        "currency": contract.currency,
        "signing_order": contract.signing_order.as_str(),
        "status": effective_status(contract, now).as_str(),
        "stored_status": contract.status.as_str(),
        "starts_at": contract.starts_at.map(|value| value.to_rfc3339()),
        "ends_at": contract.ends_at.map(|value| value.to_rfc3339()),
        "signed_at": contract.signed_at.map(|value| value.to_rfc3339()),
        "content_hash": contract.content_hash,
        "void_reason": contract.void_reason,
        "terminate_reason": contract.terminate_reason,
        "renewed_from_id": contract.renewed_from_id.map(|id| id.to_string()),
        "alert_days": contract.alert_days,
        "expiry_alert_status": contract.expiry_alert_status,
        "signers_total": signers.len(),
        "signers_signed": signed_count,
        "created_by": contract.created_by,
        "created_at": contract.created_at.to_rfc3339(),
        "updated_at": contract.updated_at.to_rfc3339(),
    })
}

/// Public view of a signer: the token hash is exposed (it is a
/// hash, not a credential); the raw token is never returned except
/// once, inside the create / resend response, under `token`.
fn signer_view(signer: &ContractSigner) -> Value {
    json!({
        "id": signer.id.to_string(),
        "contract_id": signer.contract_id.to_string(),
        "name": signer.name,
        "email": signer.email,
        "order_index": signer.order_index,
        "status": signer.status.as_str(),
        "token_hash": signer.token_hash,
        "token_expires_at": signer.token_expires_at.to_rfc3339(),
        "signed_at": signer.signed_at.map(|value| value.to_rfc3339()),
        "declined_reason": signer.declined_reason,
        "viewed_at": signer.viewed_at.map(|value| value.to_rfc3339()),
        "created_at": signer.created_at.to_rfc3339(),
        "updated_at": signer.updated_at.to_rfc3339(),
    })
}

// ---------------------------------------------------------------------------
// Template handlers
// ---------------------------------------------------------------------------

async fn list_templates(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_VIEW)?;
        let params = query.into_inner();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Value> = state
            .templates
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|template| {
                if search.is_empty() {
                    return true;
                }
                let haystack = format!(
                    "{} {}",
                    template.name,
                    template.description.clone().unwrap_or_default()
                )
                .to_lowercase();
                haystack.contains(&search)
            })
            .map(|template| json!(template))
            .collect();
        let sort = params.allowed_sort(&["name", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "name" => a
                    .get("name")
                    .and_then(Value::as_str)
                    .cmp(&b.get("name").and_then(Value::as_str)),
                _ => a
                    .get("created_at")
                    .and_then(Value::as_str)
                    .cmp(&b.get("created_at").and_then(Value::as_str)),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_template(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    body: web::Json<CreateTemplateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ContractTemplate> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let description = match &input.description {
            Some(raw) => Some(require_text(raw, "description", MAX_TERMS_LEN)?),
            None => None,
        };
        let body_text = input.body.trim().to_string();
        if body_text.is_empty() {
            return Err(UnifiedError::bad_request("body must not be empty"));
        }
        validate_template_body(&body_text, &input.placeholders)?;
        let now = Utc::now();
        let template = ContractTemplate {
            id: Uuid::now_v7(),
            name,
            description,
            body: body_text,
            placeholders: input.placeholders,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.templates.insert(template.id, template.clone());
        state.emit(
            "contracts.template.created",
            json!({
                "template_id": template.id.to_string(),
                "name": template.name,
                "placeholders": template.placeholders,
                "created_by": caller.actor,
            }),
        );
        Ok(template)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn get_template(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ContractTemplate> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_VIEW)?;
        load_template(&state, &path.into_inner())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_template(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateTemplateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ContractTemplate> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_MANAGE)?;
        let mut template = load_template(&state, &path.into_inner())?;
        let input = body.into_inner();
        if let Some(name) = &input.name {
            template.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(description) = &input.description {
            template.description = match description {
                Some(raw) => Some(require_text(raw, "description", MAX_TERMS_LEN)?),
                None => None,
            };
        }
        // Body and placeholder list are validated together so a
        // body can never reference an undeclared placeholder.
        let new_body = match &input.body {
            Some(raw) => {
                let trimmed = raw.trim().to_string();
                if trimmed.is_empty() {
                    return Err(UnifiedError::bad_request("body must not be empty"));
                }
                trimmed
            }
            None => template.body.clone(),
        };
        let new_placeholders = match &input.placeholders {
            Some(list) => list.clone(),
            None => template.placeholders.clone(),
        };
        validate_template_body(&new_body, &new_placeholders)?;
        template.body = new_body;
        template.placeholders = new_placeholders;
        template.updated_at = Utc::now();
        state.templates.insert(template.id, template.clone());
        state.emit(
            "contracts.template.updated",
            json!({
                "template_id": template.id.to_string(),
                "name": template.name,
                "updated_by": caller.actor,
            }),
        );
        Ok(template)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn delete_template(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_MANAGE)?;
        let template = load_template(&state, &path.into_inner())?;
        if state
            .contracts
            .iter()
            .any(|entry| entry.value().template_id == Some(template.id))
        {
            return Err(UnifiedError::conflict(
                "Template is used by at least one contract and cannot be deleted",
            ));
        }
        state.templates.remove(&template.id);
        state.emit(
            "contracts.template.deleted",
            json!({
                "template_id": template.id.to_string(),
                "name": template.name,
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "template_id": template.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn render_template_handler(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RenderTemplateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_VIEW)?;
        let template = load_template(&state, &path.into_inner())?;
        let input = body.into_inner();
        for (key, value) in &input.values {
            if value.len() > MAX_TERMS_LEN {
                return Err(UnifiedError::bad_request(format!(
                    "Value for placeholder \"{key}\" is too long"
                )));
            }
        }
        let rendered = render_template(&template.body, &template.placeholders, &input.values)?;
        Ok(json!({
            "template_id": template.id.to_string(),
            "rendered": rendered,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Contract handlers
// ---------------------------------------------------------------------------

async fn list_contracts(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let template_filter = params.filters.get("template_id").cloned();
        let counterparty_filter = params.filters.get("counterparty_email").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let now = Utc::now();
        let mut items: Vec<Value> = state
            .contracts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|contract| {
                if let Some(status) = &status_filter {
                    if effective_status(contract, now).as_str() != status.as_str() {
                        return false;
                    }
                }
                if let Some(template_id) = &template_filter {
                    if contract.template_id.map(|id| id.to_string()) != Some(template_id.clone()) {
                        return false;
                    }
                }
                if let Some(email) = &counterparty_filter {
                    if contract.counterparty_email.to_lowercase() != email.to_lowercase() {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {} {} {}",
                        contract.title,
                        contract.company_party,
                        contract.counterparty_name,
                        contract.counterparty_email
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .map(|contract| contract_view(&state, &contract))
            .collect();
        let sort = params.allowed_sort(&["title", "value", "ends_at", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "title" => a
                    .get("title")
                    .and_then(Value::as_str)
                    .cmp(&b.get("title").and_then(Value::as_str)),
                "value" => a
                    .get("value")
                    .and_then(Value::as_f64)
                    .partial_cmp(&b.get("value").and_then(Value::as_f64).unwrap_or(0.0))
                    .unwrap_or(std::cmp::Ordering::Equal),
                "ends_at" => a
                    .get("ends_at")
                    .and_then(Value::as_str)
                    .cmp(&b.get("ends_at").and_then(Value::as_str)),
                _ => a
                    .get("created_at")
                    .and_then(Value::as_str)
                    .cmp(&b.get("created_at").and_then(Value::as_str)),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_contract(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    body: web::Json<CreateContractRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_MANAGE)?;
        let input = body.into_inner();
        if let Some(template_id) = input.template_id {
            if !state.templates.contains_key(&template_id) {
                return Err(UnifiedError::not_found(format!(
                    "Template not found: {template_id}"
                )));
            }
        }
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let company_party = require_text(&input.company_party, "company_party", MAX_NAME_LEN)?;
        let counterparty_name =
            require_text(&input.counterparty_name, "counterparty_name", MAX_NAME_LEN)?;
        let counterparty_email = require_text(
            &input.counterparty_email,
            "counterparty_email",
            MAX_EMAIL_LEN,
        )?;
        if !valid_email(&counterparty_email) {
            return Err(UnifiedError::bad_request(
                "counterparty_email must be a valid email address",
            ));
        }
        let terms = require_text(&input.terms, "terms", MAX_TERMS_LEN)?;
        let value = valid_price(input.value)?;
        let currency = valid_currency(input.currency.as_deref().unwrap_or("USD"))?;
        if let (Some(starts_at), Some(ends_at)) = (input.starts_at, input.ends_at) {
            if ends_at <= starts_at {
                return Err(UnifiedError::bad_request(
                    "ends_at must be after starts_at",
                ));
            }
        }
        let alert_days = match input.alert_days {
            Some(days) => validate_alert_days(&days)?,
            None => state.default_alert_days(),
        };
        let now = Utc::now();
        let contract = Contract {
            id: Uuid::now_v7(),
            template_id: input.template_id,
            title,
            company_party,
            counterparty_name,
            counterparty_email,
            terms,
            value,
            currency,
            signing_order: input.signing_order.unwrap_or_default(),
            status: ContractStatus::Draft,
            starts_at: input.starts_at,
            ends_at: input.ends_at,
            signed_at: None,
            content_hash: None,
            void_reason: None,
            terminate_reason: None,
            renewed_from_id: None,
            alert_days,
            expiry_alert_status: "not_scheduled".to_string(),
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.contracts.insert(contract.id, contract.clone());
        record_audit(
            &state,
            contract.id,
            None,
            &caller.actor,
            "created",
            Some(&req),
            None,
        );
        state.emit(
            "contracts.created",
            json!({
                "contract_id": contract.id.to_string(),
                "title": contract.title,
                "status": "draft",
                "value": contract.value,
                "currency": contract.currency,
                "created_by": caller.actor,
            }),
        );
        Ok(contract_view(&state, &contract))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn get_contract(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_VIEW)?;
        let contract = load_contract(&state, &path.into_inner())?;
        Ok(contract_view(&state, &contract))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// A sealed contract (signed or later) cannot be edited: the
/// content hash proves the sealed content. The audited escape
/// hatch is voiding the contract and renewing it into a new
/// draft — voided contracts themselves stay immutable so the
/// audit trail can never be rewritten.
async fn update_contract(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateContractRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_MANAGE)?;
        let mut contract = load_contract(&state, &path.into_inner())?;
        if contract.status != ContractStatus::Draft {
            return Err(UnifiedError::conflict(format!(
                "Contract in status \"{}\" cannot be edited: it is sealed. Void it and renew it into a new draft to change the terms.",
                contract.status.as_str(),
            )));
        }
        let input = body.into_inner();
        if let Some(title) = &input.title {
            contract.title = require_text(title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(company_party) = &input.company_party {
            contract.company_party =
                require_text(company_party, "company_party", MAX_NAME_LEN)?;
        }
        if let Some(counterparty_name) = &input.counterparty_name {
            contract.counterparty_name =
                require_text(counterparty_name, "counterparty_name", MAX_NAME_LEN)?;
        }
        if let Some(counterparty_email) = &input.counterparty_email {
            let email = require_text(counterparty_email, "counterparty_email", MAX_EMAIL_LEN)?;
            if !valid_email(&email) {
                return Err(UnifiedError::bad_request(
                    "counterparty_email must be a valid email address",
                ));
            }
            contract.counterparty_email = email;
        }
        if let Some(terms) = &input.terms {
            contract.terms = require_text(terms, "terms", MAX_TERMS_LEN)?;
        }
        if let Some(value) = input.value {
            contract.value = valid_price(value)?;
        }
        if let Some(currency) = &input.currency {
            contract.currency = valid_currency(currency)?;
        }
        if let Some(signing_order) = input.signing_order {
            contract.signing_order = signing_order;
        }
        if let Some(starts_at) = input.starts_at {
            contract.starts_at = starts_at;
        }
        if let Some(ends_at) = input.ends_at {
            contract.ends_at = ends_at;
        }
        if let (Some(starts_at), Some(ends_at)) = (contract.starts_at, contract.ends_at) {
            if ends_at <= starts_at {
                return Err(UnifiedError::bad_request(
                    "ends_at must be after starts_at",
                ));
            }
        }
        if let Some(alert_days) = &input.alert_days {
            contract.alert_days = validate_alert_days(alert_days)?;
        }
        contract.updated_at = Utc::now();
        state.contracts.insert(contract.id, contract.clone());
        record_audit(
            &state,
            contract.id,
            None,
            &caller.actor,
            "updated",
            Some(&req),
            None,
        );
        Ok(contract_view(&state, &contract))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn delete_contract(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_MANAGE)?;
        let contract = load_contract(&state, &path.into_inner())?;
        if contract.status != ContractStatus::Draft {
            return Err(UnifiedError::conflict(format!(
                "Only a draft contract can be deleted (current status \"{}\"). Void it instead to keep the audit trail.",
                contract.status.as_str(),
            )));
        }
        for signer in signers_of(&state, contract.id) {
            state.signers.remove(&signer.id);
        }
        state
            .audit_trail
            .retain(|_, entry| entry.contract_id != contract.id);
        state.contracts.remove(&contract.id);
        state.emit(
            "contracts.deleted",
            json!({
                "contract_id": contract.id.to_string(),
                "title": contract.title,
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "contract_id": contract.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn send_contract(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_MANAGE)?;
        let mut contract = load_contract(&state, &path.into_inner())?;
        transition(&contract, ContractStatus::Sent, "send")?;
        let signers = signers_of(&state, contract.id);
        if signers.is_empty() {
            return Err(UnifiedError::bad_request(
                "Add at least one signer before sending a contract",
            ));
        }
        if signers
            .iter()
            .any(|signer| signer.status == SignerStatus::Declined)
        {
            return Err(UnifiedError::bad_request(
                "A signer declined this contract: remove or replace them before sending",
            ));
        }
        contract.status = ContractStatus::Sent;
        contract.updated_at = Utc::now();
        schedule_expiry_alerts(&state, &mut contract);
        state.contracts.insert(contract.id, contract.clone());
        record_audit(
            &state,
            contract.id,
            None,
            &caller.actor,
            "sent",
            Some(&req),
            Some(format!("{} signer(s)", signers.len())),
        );
        state.emit(
            "contracts.sent",
            json!({
                "contract_id": contract.id.to_string(),
                "title": contract.title,
                "signers": signers.len(),
                "sent_by": caller.actor,
            }),
        );
        Ok(contract_view(&state, &contract))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn activate_contract(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_MANAGE)?;
        let mut contract = load_contract(&state, &path.into_inner())?;
        transition(&contract, ContractStatus::Active, "activate")?;
        contract.status = ContractStatus::Active;
        contract.updated_at = Utc::now();
        schedule_expiry_alerts(&state, &mut contract);
        state.contracts.insert(contract.id, contract.clone());
        record_audit(
            &state,
            contract.id,
            None,
            &caller.actor,
            "activated",
            Some(&req),
            None,
        );
        state.emit(
            "contracts.activated",
            json!({
                "contract_id": contract.id.to_string(),
                "title": contract.title,
                "value": contract.value,
                "currency": contract.currency,
                "activated_by": caller.actor,
            }),
        );
        // Billing linkage: the billing module may invoice against
        // an active contract. No invoice is created here.
        state.emit(
            "billing.contract_activated",
            json!({
                "contract_id": contract.id.to_string(),
                "value": contract.value,
                "currency": contract.currency,
                "counterparty_name": contract.counterparty_name,
                "counterparty_email": contract.counterparty_email,
                "starts_at": contract.starts_at.map(|value| value.to_rfc3339()),
                "ends_at": contract.ends_at.map(|value| value.to_rfc3339()),
            }),
        );
        Ok(contract_view(&state, &contract))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn void_contract(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<VoidRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_MANAGE)?;
        let mut contract = load_contract(&state, &path.into_inner())?;
        transition(&contract, ContractStatus::Voided, "void")?;
        let reason = require_text(&body.reason, "reason", MAX_REASON_LEN)?;
        contract.status = ContractStatus::Voided;
        contract.void_reason = Some(reason.clone());
        contract.updated_at = Utc::now();
        state.contracts.insert(contract.id, contract.clone());
        record_audit(
            &state,
            contract.id,
            None,
            &caller.actor,
            "voided",
            Some(&req),
            Some(reason),
        );
        state.emit(
            "contracts.voided",
            json!({
                "contract_id": contract.id.to_string(),
                "title": contract.title,
                "voided_by": caller.actor,
            }),
        );
        Ok(contract_view(&state, &contract))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn terminate_contract(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<TerminateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_MANAGE)?;
        let mut contract = load_contract(&state, &path.into_inner())?;
        transition(&contract, ContractStatus::Terminated, "terminate")?;
        let reason = require_text(&body.reason, "reason", MAX_REASON_LEN)?;
        contract.status = ContractStatus::Terminated;
        contract.terminate_reason = Some(reason.clone());
        contract.updated_at = Utc::now();
        state.contracts.insert(contract.id, contract.clone());
        record_audit(
            &state,
            contract.id,
            None,
            &caller.actor,
            "terminated",
            Some(&req),
            Some(reason),
        );
        state.emit(
            "contracts.terminated",
            json!({
                "contract_id": contract.id.to_string(),
                "title": contract.title,
                "terminated_by": caller.actor,
            }),
        );
        Ok(contract_view(&state, &contract))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn check_expiry_contract(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_MANAGE)?;
        let mut contract = load_contract(&state, &path.into_inner())?;
        let expired = materialise_expiry(&state, &mut contract);
        if expired {
            state.contracts.insert(contract.id, contract.clone());
        }
        Ok(json!({
            "contract_id": contract.id.to_string(),
            "status": effective_status(&contract, Utc::now()).as_str(),
            "expired_by_check": expired,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn renew_contract(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RenewRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_MANAGE)?;
        let source = load_contract(&state, &path.into_inner())?;
        if matches!(
            source.status,
            ContractStatus::Draft | ContractStatus::Sent | ContractStatus::PartiallySigned
        ) {
            return Err(UnifiedError::conflict(
                "Only a signed, active, expired, terminated, or voided contract can be renewed",
            ));
        }
        let input = body.into_inner();
        let starts_at = input.starts_at.or(source.starts_at);
        let ends_at = input.ends_at.or(source.ends_at);
        if let (Some(starts_at), Some(ends_at)) = (starts_at, ends_at) {
            if ends_at <= starts_at {
                return Err(UnifiedError::bad_request(
                    "ends_at must be after starts_at",
                ));
            }
        }
        let now = Utc::now();
        let renewed = Contract {
            id: Uuid::now_v7(),
            template_id: source.template_id,
            title: source.title.clone(),
            company_party: source.company_party.clone(),
            counterparty_name: source.counterparty_name.clone(),
            counterparty_email: source.counterparty_email.clone(),
            terms: source.terms.clone(),
            value: source.value,
            currency: source.currency.clone(),
            signing_order: source.signing_order,
            status: ContractStatus::Draft,
            starts_at,
            ends_at,
            signed_at: None,
            content_hash: None,
            void_reason: None,
            terminate_reason: None,
            renewed_from_id: Some(source.id),
            alert_days: source.alert_days.clone(),
            expiry_alert_status: "not_scheduled".to_string(),
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.contracts.insert(renewed.id, renewed.clone());
        record_audit(
            &state,
            source.id,
            None,
            &caller.actor,
            "renewed",
            Some(&req),
            Some(format!("Renewed into draft contract {}", renewed.id)),
        );
        state.emit(
            "contracts.renewed",
            json!({
                "contract_id": source.id.to_string(),
                "renewed_contract_id": renewed.id.to_string(),
                "renewed_by": caller.actor,
            }),
        );
        state.emit(
            "contracts.created",
            json!({
                "contract_id": renewed.id.to_string(),
                "title": renewed.title,
                "status": "draft",
                "value": renewed.value,
                "currency": renewed.currency,
                "renewed_from_id": source.id.to_string(),
                "created_by": caller.actor,
            }),
        );
        Ok(contract_view(&state, &renewed))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn list_audit(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_VIEW)?;
        let contract = load_contract(&state, &path.into_inner())?;
        let params = query.into_inner();
        let action_filter = params.filters.get("action").cloned();
        let actor_filter = params.filters.get("actor").cloned();
        let mut items: Vec<Value> = state
            .audit_trail
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| entry.contract_id == contract.id)
            .filter(|entry| {
                if let Some(action) = &action_filter {
                    if entry.action != *action {
                        return false;
                    }
                }
                if let Some(actor) = &actor_filter {
                    if entry.actor != *actor {
                        return false;
                    }
                }
                true
            })
            .map(|entry| json!(entry))
            .collect();
        items.sort_by(|a, b| {
            b.get("created_at")
                .and_then(Value::as_str)
                .cmp(&a.get("created_at").and_then(Value::as_str))
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Signer handlers
// ---------------------------------------------------------------------------

async fn list_signers(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_VIEW)?;
        let contract = load_contract(&state, &path.into_inner())?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let items: Vec<Value> = signers_of(&state, contract.id)
            .into_iter()
            .filter(|signer| {
                if let Some(status) = &status_filter {
                    if signer.status.as_str() != status.as_str() {
                        return false;
                    }
                }
                true
            })
            .map(|signer| signer_view(&signer))
            .collect();
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn add_signer(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AddSignerRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    // The raw token is returned exactly once, inside this response
    // under `token`; it is never stored, only its SHA-256 hash.
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_MANAGE)?;
        let contract = load_contract(&state, &path.into_inner())?;
        if !matches!(
            contract.status,
            ContractStatus::Draft | ContractStatus::Sent
        ) {
            return Err(UnifiedError::conflict(format!(
                "Signers can only be added while a contract is draft or sent (current status \"{}\")",
                contract.status.as_str(),
            )));
        }
        let existing = signers_of(&state, contract.id);
        if existing.len() >= MAX_SIGNERS_PER_CONTRACT {
            return Err(UnifiedError::bad_request(format!(
                "At most {MAX_SIGNERS_PER_CONTRACT} signers per contract"
            )));
        }
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let email = require_text(&input.email, "email", MAX_EMAIL_LEN)?;
        if !valid_email(&email) {
            return Err(UnifiedError::bad_request(
                "email must be a valid email address",
            ));
        }
        if existing
            .iter()
            .any(|signer| signer.email.to_lowercase() == email.to_lowercase())
        {
            return Err(UnifiedError::conflict(format!(
                "Signer with email \"{email}\" already exists on this contract"
            )));
        }
        let order_index = match input.order_index {
            Some(index) => {
                if index < 0 || index > existing.len() as i64 {
                    return Err(UnifiedError::bad_request(format!(
                        "order_index must be between 0 and {}",
                        existing.len()
                    )));
                }
                index
            }
            None => existing.len() as i64,
        };
        let ttl_days = input.expires_in_days.unwrap_or(DEFAULT_TOKEN_TTL_DAYS);
        if !(1..=90).contains(&ttl_days) {
            return Err(UnifiedError::bad_request(
                "expires_in_days must be between 1 and 90",
            ));
        }
        let token = generate_token();
        let now = Utc::now();
        let signer = ContractSigner {
            id: Uuid::now_v7(),
            contract_id: contract.id,
            name,
            email,
            order_index,
            status: SignerStatus::Pending,
            token_hash: hash_value(&token),
            token_expires_at: now + Duration::days(ttl_days),
            signed_at: None,
            declined_reason: None,
            viewed_at: None,
            created_at: now,
            updated_at: now,
        };
        state.signers.insert(signer.id, signer.clone());
        record_audit(
            &state,
            contract.id,
            Some(signer.id),
            &caller.actor,
            "signer_added",
            Some(&req),
            Some(format!("Signer {} <{}>", signer.name, signer.email)),
        );
        state.emit(
            "contracts.signer.added",
            json!({
                "contract_id": contract.id.to_string(),
                "signer_id": signer.id.to_string(),
                "email": signer.email,
                "order_index": signer.order_index,
            }),
        );
        let mut view = signer_view(&signer);
        if let Value::Object(map) = &mut view {
            map.insert("token".to_string(), Value::String(token));
        }
        Ok(view)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn remove_signer(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_MANAGE)?;
        let (contract_id, signer_id) = path.into_inner();
        let contract = load_contract(&state, &contract_id)?;
        let signer = load_signer(&state, contract.id, &signer_id)?;
        if contract.status != ContractStatus::Draft && contract.status != ContractStatus::Sent {
            return Err(UnifiedError::conflict(format!(
                "Signers can only be removed while a contract is draft or sent (current status \"{}\")",
                contract.status.as_str(),
            )));
        }
        if signer.status == SignerStatus::Signed {
            return Err(UnifiedError::conflict(
                "A signer who already signed cannot be removed; void the contract instead",
            ));
        }
        state.signers.remove(&signer.id);
        record_audit(
            &state,
            contract.id,
            Some(signer.id),
            &caller.actor,
            "signer_removed",
            Some(&req),
            Some(format!("Signer {} <{}>", signer.name, signer.email)),
        );
        state.emit(
            "contracts.signer.removed",
            json!({
                "contract_id": contract.id.to_string(),
                "signer_id": signer.id.to_string(),
                "email": signer.email,
            }),
        );
        Ok(json!({ "deleted": true, "signer_id": signer.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn resend_signer_token(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    // The fresh raw token is returned exactly once, inside this
    // response under `token`; the old token stops working because
    // only the new hash is stored.
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_MANAGE)?;
        let (contract_id, signer_id) = path.into_inner();
        let contract = load_contract(&state, &contract_id)?;
        let mut signer = load_signer(&state, contract.id, &signer_id)?;
        if !matches!(
            contract.status,
            ContractStatus::Sent | ContractStatus::PartiallySigned
        ) {
            return Err(UnifiedError::conflict(format!(
                "Signing tokens can only be re-issued while a contract is sent or partially signed (current status \"{}\")",
                contract.status.as_str(),
            )));
        }
        if signer.status == SignerStatus::Signed || signer.status == SignerStatus::Declined {
            return Err(UnifiedError::conflict(format!(
                "Cannot re-issue a token for a signer who already {}",
                signer.status.as_str(),
            )));
        }
        let token = generate_token();
        let now = Utc::now();
        signer.token_hash = hash_value(&token);
        signer.token_expires_at = now + Duration::days(DEFAULT_TOKEN_TTL_DAYS);
        signer.updated_at = now;
        state.signers.insert(signer.id, signer.clone());
        record_audit(
            &state,
            contract.id,
            Some(signer.id),
            &caller.actor,
            "token_resent",
            Some(&req),
            Some(format!("New signing token issued to {}", signer.email)),
        );
        state.emit(
            "contracts.signer.token_resent",
            json!({
                "contract_id": contract.id.to_string(),
                "signer_id": signer.id.to_string(),
                "email": signer.email,
            }),
        );
        let mut view = signer_view(&signer);
        if let Value::Object(map) = &mut view {
            map.insert("token".to_string(), Value::String(token));
        }
        Ok(view)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Token-gated signing links (no capability check — the token is the
// credential; see the public-access note in the module header)
// ---------------------------------------------------------------------------

/// Resolve a raw signing token to its signer. Unknown, expired,
/// and revoked tokens all answer 404 with the same message, so
/// tokens cannot be probed apart.
fn load_signer_by_token(
    state: &ContractsState,
    token: &str,
) -> UnifiedResult<(Contract, ContractSigner)> {
    let token_hash = hash_value(token);
    let now = Utc::now();
    let signer = state
        .signers
        .iter()
        .map(|entry| entry.value().clone())
        .find(|entry| entry.token_hash == token_hash)
        .ok_or_else(|| UnifiedError::not_found("Signing link not found or expired"))?;
    if signer.token_expires_at <= now {
        return Err(UnifiedError::not_found("Signing link not found or expired"));
    }
    let contract = state
        .contracts
        .get(&signer.contract_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Signing link not found or expired"))?;
    Ok((contract, signer))
}

async fn get_signing_link(
    state: web::Data<ContractsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = false;
    let result: UnifiedResult<Value> = (|| {
        let token = path.into_inner();
        if token.len() != SIGNING_TOKEN_LEN
            || !token.chars().all(|c| c.is_ascii_alphanumeric())
        {
            return Err(UnifiedError::not_found("Signing link not found or expired"));
        }
        let (contract, mut signer) = load_signer_by_token(&state, &token)?;
        if !matches!(
            contract.status,
            ContractStatus::Sent | ContractStatus::PartiallySigned
        ) {
            return Err(UnifiedError::not_found("Signing link not found or expired"));
        }
        // A first view is audited and moves the signer to `viewed`.
        if signer.status == SignerStatus::Pending {
            let now = Utc::now();
            signer.status = SignerStatus::Viewed;
            signer.viewed_at = Some(now);
            signer.updated_at = now;
            state.signers.insert(signer.id, signer.clone());
            record_audit(
                &state,
                contract.id,
                Some(signer.id),
                &signer.email,
                "viewed",
                Some(&req),
                None,
            );
        }
        let signers = signers_of(&state, contract.id);
        Ok(json!({
            "contract": contract_view(&state, &contract),
            "signer": signer_view(&signer),
            "signers": signers.iter().map(signer_view).collect::<Vec<Value>>(),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn sign_via_link(
    state: web::Data<ContractsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<SignRequest>,
) -> HttpResponse {
    let debug = false;
    let result: UnifiedResult<Value> = (|| {
        let token = path.into_inner();
        if token.len() != SIGNING_TOKEN_LEN
            || !token.chars().all(|c| c.is_ascii_alphanumeric())
        {
            return Err(UnifiedError::not_found("Signing link not found or expired"));
        }
        let (mut contract, mut signer) = load_signer_by_token(&state, &token)?;
        if !matches!(
            contract.status,
            ContractStatus::Sent | ContractStatus::PartiallySigned
        ) {
            return Err(UnifiedError::conflict(format!(
                "Contract in status \"{}\" cannot be signed",
                contract.status.as_str(),
            )));
        }
        if signer.status == SignerStatus::Signed {
            return Err(UnifiedError::conflict("This signing link was already used"));
        }
        if signer.status == SignerStatus::Declined {
            return Err(UnifiedError::conflict(
                "This signer declined the contract; the link cannot be used to sign",
            ));
        }
        if !body.confirm {
            return Err(UnifiedError::bad_request(
                "Signing requires \"confirm\": true — the signer must confirm they reviewed the terms",
            ));
        }
        // Sequential order: every lower order_index must have
        // signed already.
        if contract.signing_order == SigningOrder::Sequential {
            let signers = signers_of(&state, contract.id);
            let blocked = signers.iter().any(|other| {
                other.order_index < signer.order_index && other.status != SignerStatus::Signed
            });
            if blocked {
                return Err(UnifiedError::conflict(
                    "Sequential signing: an earlier signer has not signed yet",
                ));
            }
        }
        let now = Utc::now();
        signer.status = SignerStatus::Signed;
        signer.signed_at = Some(now);
        signer.updated_at = now;
        state.signers.insert(signer.id, signer.clone());
        record_audit(
            &state,
            contract.id,
            Some(signer.id),
            &signer.email,
            "signed",
            Some(&req),
            Some(format!("Signer {} <{}> signed", signer.name, signer.email)),
        );
        state.emit(
            "contracts.signer.signed",
            json!({
                "contract_id": contract.id.to_string(),
                "signer_id": signer.id.to_string(),
                "email": signer.email,
                "order_index": signer.order_index,
            }),
        );
        // Advance the contract: last signature seals it.
        let signers = signers_of(&state, contract.id);
        let all_signed = !signers.is_empty()
            && signers
                .iter()
                .all(|entry| entry.status == SignerStatus::Signed);
        if all_signed {
            contract.status = ContractStatus::Signed;
            contract.signed_at = Some(now);
            contract.content_hash = Some(seal_content_hash(&contract));
            contract.updated_at = now;
            state.contracts.insert(contract.id, contract.clone());
            record_audit(
                &state,
                contract.id,
                Some(signer.id),
                &signer.email,
                "sealed",
                Some(&req),
                Some(format!(
                    "All signers signed; content sealed with hash {}",
                    contract.content_hash.clone().unwrap_or_default()
                )),
            );
            state.emit(
                "contracts.signed",
                json!({
                    "contract_id": contract.id.to_string(),
                    "title": contract.title,
                    "content_hash": contract.content_hash,
                    "signed_at": now.to_rfc3339(),
                }),
            );
            state.notify(
                "contract.signed",
                &contract.created_by,
                &format!("Contract fully signed: {}", contract.title),
                &format!(
                    "All signers signed contract \"{}\". It is sealed and ready to activate.",
                    contract.title
                ),
                Some(format!("/api/v1/contracts/{}", contract.id)),
            );
        } else {
            contract.status = ContractStatus::PartiallySigned;
            contract.updated_at = now;
            state.contracts.insert(contract.id, contract.clone());
        }
        Ok(json!({
            "contract": contract_view(&state, &contract),
            "signer": signer_view(&signer),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn decline_via_link(
    state: web::Data<ContractsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<DeclineRequest>,
) -> HttpResponse {
    let debug = false;
    let result: UnifiedResult<Value> = (|| {
        let token = path.into_inner();
        if token.len() != SIGNING_TOKEN_LEN
            || !token.chars().all(|c| c.is_ascii_alphanumeric())
        {
            return Err(UnifiedError::not_found("Signing link not found or expired"));
        }
        let (contract, mut signer) = load_signer_by_token(&state, &token)?;
        if !matches!(
            contract.status,
            ContractStatus::Sent | ContractStatus::PartiallySigned
        ) {
            return Err(UnifiedError::conflict(format!(
                "Contract in status \"{}\" cannot be declined",
                contract.status.as_str(),
            )));
        }
        if signer.status == SignerStatus::Signed {
            return Err(UnifiedError::conflict("This signer already signed the contract"));
        }
        if signer.status == SignerStatus::Declined {
            return Err(UnifiedError::conflict("This signer already declined the contract"));
        }
        let reason = require_text(&body.reason, "reason", MAX_REASON_LEN)?;
        let now = Utc::now();
        signer.status = SignerStatus::Declined;
        signer.declined_reason = Some(reason.clone());
        signer.updated_at = now;
        state.signers.insert(signer.id, signer.clone());
        record_audit(
            &state,
            contract.id,
            Some(signer.id),
            &signer.email,
            "declined",
            Some(&req),
            Some(reason.clone()),
        );
        state.emit(
            "contracts.signer.declined",
            json!({
                "contract_id": contract.id.to_string(),
                "signer_id": signer.id.to_string(),
                "email": signer.email,
                "reason": reason,
            }),
        );
        state.notify(
            "contract.signer_declined",
            &contract.created_by,
            &format!("Signer declined: {}", contract.title),
            &format!(
                "Signer {} <{}> declined contract \"{}\": {}",
                signer.name, signer.email, contract.title, reason
            ),
            Some(format!("/api/v1/contracts/{}", contract.id)),
        );
        Ok(json!({
            "contract": contract_view(&state, &contract),
            "signer": signer_view(&signer),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Settings handlers
// ---------------------------------------------------------------------------

async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_VIEW)?;
        Ok(json!({ "alert_days": state.default_alert_days() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_MANAGE)?;
        let alert_days = validate_alert_days(&body.alert_days)?;
        *state
            .alert_days
            .write()
            .expect("alert days lock poisoned") = alert_days.clone();
        Ok(json!({ "alert_days": alert_days }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Scheduler listener
// ---------------------------------------------------------------------------

/// Handle `contracts.expiry_alert_requested`: send the due alert
/// through `notification.requested`, and mark the contract expired
/// (emitting `contracts.expired`) when its end date passed.
fn on_expiry_alert_requested(state: ContractsState, ctx: &mut HookContext) {
    let contract_id = ctx
        .data
        .get("contract_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok());
    let Some(contract_id) = contract_id else {
        ctx.add_result(json!({ "sent": false, "error": "contracts.expiry_alert_requested requires 'contract_id'" }));
        return;
    };
    let days_before = ctx
        .data
        .get("days_before")
        .and_then(|value| value.as_i64())
        .unwrap_or(0);
    let Some(mut contract) = state
        .contracts
        .get(&contract_id)
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({ "sent": false, "contract_id": contract_id.to_string(), "error": "Contract not found" }));
        return;
    };
    if materialise_expiry(&state, &mut contract) {
        state.contracts.insert(contract.id, contract.clone());
        ctx.add_result(json!({ "sent": false, "contract_id": contract_id.to_string(), "expired": true }));
        return;
    }
    if contract.status != ContractStatus::Active {
        ctx.add_result(json!({
            "sent": false,
            "contract_id": contract_id.to_string(),
            "reason": format!("Contract is {}", contract.status.as_str()),
        }));
        return;
    }
    let ends_at = contract.ends_at.map(|value| value.to_rfc3339());
    state.notify(
        "contract.expiring",
        &contract.created_by,
        &format!("Contract expires in {days_before} day(s): {}", contract.title),
        &format!(
            "Contract \"{}\" with {} ends on {}.",
            contract.title,
            contract.counterparty_name,
            ends_at.clone().unwrap_or_default()
        ),
        Some(format!("/api/v1/contracts/{}", contract.id)),
    );
    record_audit(
        &state,
        contract.id,
        None,
        "scheduler",
        "expiry_alert_sent",
        None,
        Some(format!("{days_before} day(s) before end date")),
    );
    ctx.add_result(json!({
        "sent": true,
        "contract_id": contract_id.to_string(),
        "days_before": days_before,
        "ends_at": ends_at,
    }));
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct ContractsModule {
    state: ContractsState,
}

impl ContractsModule {
    pub fn new() -> Self {
        Self {
            state: ContractsState::default(),
        }
    }
}

impl Default for ContractsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for ContractsModule {
    fn id(&self) -> &'static str {
        "contracts"
    }

    fn name(&self) -> &'static str {
        "Contracts & E-Signature"
    }

    fn description(&self) -> &'static str {
        "Contract templates with placeholders, contracts with a validated status machine, in-app token-based e-signing with signing order and a full audit trail, expiry and renewal alerts, and a billing linkage event when a contract becomes active. In-app signing only: no certified third-party e-signature provider, no qualified certificates, no eIDAS/QES claim; invoices are never created here."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "billing", "notifications", "scheduler"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "billing", "notifications", "scheduler"]
    }

    /// Contracts is sold as a product: a customer buys a module
    /// entitlement for it (see `crate::modules::licensing`).
    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            EXPIRY_ALERT_REQUESTED,
            "contracts",
            Arc::new(move |ctx: &mut HookContext| {
                on_expiry_alert_requested(state.clone(), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/contracts")
                .app_data(state)
                .route("", web::get().to(list_contracts))
                .route("", web::post().to(create_contract))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::patch().to(update_settings))
                .route("/templates", web::get().to(list_templates))
                .route("/templates", web::post().to(create_template))
                .route("/templates/{id}", web::get().to(get_template))
                .route("/templates/{id}", web::patch().to(update_template))
                .route("/templates/{id}", web::delete().to(delete_template))
                .route("/templates/{id}/render", web::post().to(render_template_handler))
                .route("/sign/{token}", web::get().to(get_signing_link))
                .route("/sign/{token}", web::post().to(sign_via_link))
                .route("/sign/{token}/decline", web::post().to(decline_via_link))
                .route("/{id}", web::get().to(get_contract))
                .route("/{id}", web::patch().to(update_contract))
                .route("/{id}", web::delete().to(delete_contract))
                .route("/{id}/send", web::post().to(send_contract))
                .route("/{id}/activate", web::post().to(activate_contract))
                .route("/{id}/void", web::post().to(void_contract))
                .route("/{id}/terminate", web::post().to(terminate_contract))
                .route("/{id}/check-expiry", web::post().to(check_expiry_contract))
                .route("/{id}/renew", web::post().to(renew_contract))
                .route("/{id}/audit", web::get().to(list_audit))
                .route("/{id}/signers", web::get().to(list_signers))
                .route("/{id}/signers", web::post().to(add_signer))
                .route("/{id}/signers/{signer_id}", web::delete().to(remove_signer))
                .route(
                    "/{id}/signers/{signer_id}/resend",
                    web::post().to(resend_signer_token),
                ),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/contracts", owner: "contracts", description: "List contracts (paginated, filter by status, template_id, counterparty_email, search)." },
            RouteInfo { method: "POST", path: "/api/v1/contracts", owner: "contracts", description: "Create a contract (draft)." },
            RouteInfo { method: "GET", path: "/api/v1/contracts/{id}", owner: "contracts", description: "Get one contract (effective status derived at read time)." },
            RouteInfo { method: "PATCH", path: "/api/v1/contracts/{id}", owner: "contracts", description: "Update a draft contract only; sealed contracts are rejected (void + renew instead)." },
            RouteInfo { method: "DELETE", path: "/api/v1/contracts/{id}", owner: "contracts", description: "Delete a draft contract only." },
            RouteInfo { method: "POST", path: "/api/v1/contracts/{id}/send", owner: "contracts", description: "Send a draft contract (draft -> sent; at least one signer required; expiry alerts armed)." },
            RouteInfo { method: "POST", path: "/api/v1/contracts/{id}/activate", owner: "contracts", description: "Activate a signed contract (signed -> active; emits billing.contract_activated, no invoice created here)." },
            RouteInfo { method: "POST", path: "/api/v1/contracts/{id}/void", owner: "contracts", description: "Void a contract with a reason (audited; terminal)." },
            RouteInfo { method: "POST", path: "/api/v1/contracts/{id}/terminate", owner: "contracts", description: "Terminate a contract early with a reason (audited)." },
            RouteInfo { method: "POST", path: "/api/v1/contracts/{id}/check-expiry", owner: "contracts", description: "Materialise expiry now when the end date passed (also driven by the scheduler listener)." },
            RouteInfo { method: "POST", path: "/api/v1/contracts/{id}/renew", owner: "contracts", description: "Create a new draft contract copying terms, parties, and value of this one." },
            RouteInfo { method: "GET", path: "/api/v1/contracts/{id}/audit", owner: "contracts", description: "Full audit trail of a contract (paginated, filter by action, actor)." },
            RouteInfo { method: "GET", path: "/api/v1/contracts/{id}/signers", owner: "contracts", description: "List signers of a contract (paginated, filter by status)." },
            RouteInfo { method: "POST", path: "/api/v1/contracts/{id}/signers", owner: "contracts", description: "Add a signer (draft / sent only; returns the raw signing token once)." },
            RouteInfo { method: "DELETE", path: "/api/v1/contracts/{id}/signers/{signer_id}", owner: "contracts", description: "Remove a pending signer (draft / sent only; a signer who signed cannot be removed)." },
            RouteInfo { method: "POST", path: "/api/v1/contracts/{id}/signers/{signer_id}/resend", owner: "contracts", description: "Issue a fresh signing token (old token stops working; audited; returns the raw token once)." },
            RouteInfo { method: "GET", path: "/api/v1/contracts/sign/{token}", owner: "contracts", description: "TOKEN-GATED: read the contract a signer was asked to sign (first view is audited; unknown/expired tokens answer 404)." },
            RouteInfo { method: "POST", path: "/api/v1/contracts/sign/{token}", owner: "contracts", description: "TOKEN-GATED: sign (requires confirm=true; sequential order enforced; last signature seals the contract)." },
            RouteInfo { method: "POST", path: "/api/v1/contracts/sign/{token}/decline", owner: "contracts", description: "TOKEN-GATED: decline with a reason (audited)." },
            RouteInfo { method: "GET", path: "/api/v1/contracts/templates", owner: "contracts", description: "List contract templates (paginated, search)." },
            RouteInfo { method: "POST", path: "/api/v1/contracts/templates", owner: "contracts", description: "Create a contract template (unknown placeholders in the body are rejected at save)." },
            RouteInfo { method: "GET", path: "/api/v1/contracts/templates/{id}", owner: "contracts", description: "Get one contract template." },
            RouteInfo { method: "PATCH", path: "/api/v1/contracts/templates/{id}", owner: "contracts", description: "Update a contract template (body and placeholders validated together)." },
            RouteInfo { method: "DELETE", path: "/api/v1/contracts/templates/{id}", owner: "contracts", description: "Delete a template not used by any contract." },
            RouteInfo { method: "POST", path: "/api/v1/contracts/templates/{id}/render", owner: "contracts", description: "Render a template with supplied values (values are escaped; missing or unknown placeholders rejected)." },
            RouteInfo { method: "GET", path: "/api/v1/contracts/settings", owner: "contracts", description: "Get expiry alert settings (alert_days offsets)." },
            RouteInfo { method: "PATCH", path: "/api/v1/contracts/settings", owner: "contracts", description: "Update expiry alert settings (alert_days, descending, 1..365)." },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ContractsModule::new()));
}
