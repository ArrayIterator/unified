//! ai module - AI capability registry and capability execution.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use crate::modules::privacy;
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Instant;
use uuid::Uuid;

use super::constants::DEFAULT_MAX_TOKENS;
use super::constants::EVENT_AI_CALL_COMPLETED;
use super::constants::EVENT_AI_CALL_FAILED;
use super::constants::EVENT_AI_DRAFT_CREATED;
use super::constants::MAX_CONTEXT_TEXT_LEN;
use super::constants::MAX_MAX_TOKENS;
use super::constants::MAX_PROMPT_LEN;
use super::constants::PERM_AI_USE;
use super::crypto::decrypt_api_key;
use super::crypto::encryption_secret;
use super::gating::AiAuthorization;
use super::gating::AiCallRequest;
use super::gating::authorize_ai_call;
use super::gating::release_cap_reservation;
use super::gating::resolve_caller_product;
use super::handlers_drafts::complete;
use super::helpers::Caller;
use super::helpers::caller;
use super::helpers::caller_company_uuid;
use super::helpers::caller_user_id;
use super::helpers::check_rate_limit;
use super::helpers::emit_audit;
use super::helpers::err;
use super::helpers::ok;
use super::helpers::paginate;
use super::helpers::record_usage;
use super::helpers::require_permission;
use super::helpers::require_tenant;
use super::module::register;
use super::providers::CompletionCall;
use super::providers::ai_provider_registry;
use super::state::AiState;
use super::types::AiDraft;
use super::types::AiProviderConnection;
use super::types::AiUsageRecord;
use super::types::DraftKind;
use super::types::DraftStatus;
use super::types::GroundingSource;
use super::validation::estimate_prompt_tokens;
use super::validation::resolve_capability_company;
/// the earliest-created enabled connection when none was requested.
pub(crate) fn resolve_connection(
    state: &AiState,
    provider_id: Option<Uuid>,
) -> UnifiedResult<AiProviderConnection> {
    match provider_id {
        Some(id) => state
            .providers
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!("AI provider connection not found: {id}"))
            }),
        None => {
            let mut enabled: Vec<AiProviderConnection> = state
                .providers
                .iter()
                .map(|entry| entry.value().clone())
                .filter(|connection| connection.enabled)
                .collect();
            enabled.sort_by(|a, b| a.created_at.cmp(&b.created_at));
            enabled.into_iter().next().ok_or_else(|| {
                UnifiedError::bad_request(
                    "No enabled AI provider connection exists: create one first, or pass provider_id",
                )
            })
        }
    }
}

/// Outcome of one provider call, with its usage record attached.
pub(crate) struct CompletionOutcome {
    pub(crate) text: String,
    pub(crate) usage: AiUsageRecord,
    pub(crate) provider_id: Uuid,
    pub(crate) provider_kind: String,
    pub(crate) model: String,
}

// AI Capabilities Contract.
//
// Every dynamic module exposes its AI features through this registry
// instead of hard-coding them here: a module declares its
// capabilities (a `&[AiCapability]` const in its own file) and calls
// `register_ai_capabilities` once from its `init`. The AI module
// never keeps a hard-coded match list of module capabilities; the
// registry is the single source of truth, and `run_ai_capability`
// is the single runner every module AI endpoint delegates to.
//
// Flow for one capability call (`run_ai_capability`):
//   (a) look the capability up in the registry (unknown = 404),
//   (b) validate the input object contains exactly the declared
//       `input_fields` (missing or unexpected field = 422),
//   (c) redact PII through the privacy module when the capability
//       does not allow it (`allows_pii == false`),
//   (d) emit the grounding hook `<module>.ai.<capability>_requested`
//       with `{"request_id", "module_id", "capability", "caller",
//       "input"}` and collect grounding from the results (first
//       result wins, the rest merge into `extra_context`),
//   (e) run the allowlist gate (`authorize_ai_call`, with the
//       capability's `use_case`) FIRST, before any provider traffic,
//   (e2) package-entitlement gate (after authorization, still
//       before any provider traffic): for a sellable non-core
//       module, the calling tenant's entitlement is checked
//       through the `entitlements.check_requested` hook query,
//       fail closed - only an explicit `allowed: true` passes.
//       Core modules, modules that are not sellable, and
//       control-plane calls (no company id) skip the hook; see the
//       gate in the function body.
//   (f) run the completion through the same `run_completion` path
//       the draft endpoints use (default provider connection: the
//       earliest-created enabled connection; the connection key,
//       never a personal key),
//   (g) record usage and emit `ai.call.completed` / `ai.call.failed`
//       through `run_completion`, exactly like the draft endpoints,
//   (h) return the output inline; the owning module decides whether
//       the output is stored, reviewed, or published. Capability
//       outputs are deliberately NOT stored as `AiDraft`s here: the
//       draft endpoints own the human-review draft flow, and a
//       capability result is data the owning module handles.
//
// Security notes:
//   - The gate + allowlist run on EVERY capability call and are
//     never cached; `authorize_ai_call` re-evaluates the layered
//     gate and the policy caps on each call.
//   - Undeclared input fields are never passed to the model.
//   - No prompt content is ever logged or put in an audit payload:
//     audit events carry ids and reason codes only.
//   - Capability calls carry the caller's purchased-product
//     context when they arrive over HTTP: the endpoint resolves
//     `X-Product-Id` and passes it through
//     `run_ai_capability_with_product`, so the product layer
//     checks the purchased product instead of defaulting to
//     allow. Programmatic callers using `run_ai_capability`
//     (no request to read the header from) still get the
//     default-allow product layer, exactly as any call without
//     that header does. The module-level package entitlement is
//     checked separately by the gate in (e2): the tenant's
//     entitlement for the called module is resolved through
//     the `entitlements.check_requested` hook, fail closed,
//     before any provider traffic.
//   - A capability's `use_case` is the string the gate and the
//     policy `allowed_features` lists match on. Built-in use cases
//     live in `KNOWN_USE_CASES`; a module-scoped use case (for
//     example `exams.draft_questions`) is accepted by
//     `validate_policy_features` once its capability is registered.
//
// Tenant scoping (standing rule: tenant-scoped data access).
//   - Tenant data: capability calls, usage records, and grounding
//     data are tenant-scoped. The company id on every capability
//     call comes from `resolve_capability_company` - the verified
//     `TenantContext` first (middleware-attached, resolved from
//     the Host header), then the pre-existing header / hook path
//     for control-plane requests. Usage is recorded per company
//     (`AiUsageRecord.company_id`). Grounding hooks receive the
//     calling tenant's company id in the payload and must only
//     ever return that tenant's data.
//   - Control-plane data: provider connections and the global AI
//     settings are platform data; when persisted they belong in
//     the `public` schema, never in a tenant schema. Company and
//     user access policies are per-tenant data.
//   - Current state, honestly: every store in this module (and in
//     the wired modules) is in-memory with no tenant partition
//     and no Postgres access at all, so there is no
//     `public`-schema fallback to remove - but also no
//     tenant-column filtering to enforce yet. The tenancy plumbing
//     itself DOES exist (`unified_core::tenancy`: `schema_name`,
//     `with_tenant_connection`, the `TenantContext` extractor, the
//     `ControlPlaneGuard`); the convergence points for persistence
//     are: (1) when AI state gains Postgres persistence, usage /
//     policies / capability results must be written through
//     `with_tenant_connection` in the tenant schema resolved from
//     the tenant context, and provider connections / global
//     settings through `public` (via `ControlPlaneGuard`);
//     (2) grounding handlers must filter by the payload's caller
//     company id once their stores carry one; (3) the
//     package-entitlement gate in `run_ai_capability` (step (e2))
//     asks `entitlements.check_requested` and fails closed while
//     the entitlements module is not registered and answering, so
//     sellable-module capability calls stay denied until the
//     integrator registers it in `mod.rs`.
// The AI module's own state is shared with this runner through a
// process-wide handle (`AI_STATE`), set once in `AiModule::init`
// from the same `AiState` the HTTP scope serves, so the providers,
// usage, and policy stores are the same instance on both paths.
// This follows the static-state pattern already used by other
// modules in this codebase for cross-module access.

/// One AI capability a module exposes. Capability ids, module ids,
/// and use cases are declared per-module (registry), never matched
/// on in a hard-coded list here. Field names and order are part of
/// the contract: parallel builders write against this exact shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AiCapability {
    pub id: &'static str,
    pub module_id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub input_fields: &'static [&'static str],
    pub allows_pii: bool,
    pub use_case: &'static str,
}

/// The AI module's own capabilities, registered in `AiModule::init`
/// so the registry is self-describing from startup.
pub const AI_CAPABILITIES: &[AiCapability] = &[
    AiCapability {
        id: "summarize",
        module_id: "ai",
        name: "Summarize text",
        description: "Summarize caller-supplied text into a short summary. No personal data is accepted: input is redacted before it reaches the model.",
        input_fields: &["text"],
        allows_pii: false,
        use_case: "complete",
    },
    AiCapability {
        id: "draft",
        module_id: "ai",
        name: "Draft text",
        description: "Draft text from a topic and a short brief. The output is returned inline; it is never published or sent by this module.",
        input_fields: &["topic", "brief"],
        allows_pii: false,
        use_case: "content",
    },
    AiCapability {
        id: "analyze",
        module_id: "ai",
        name: "Analyze text",
        description: "Analyze caller-supplied text and return key points, tone, and suggested follow-ups. No personal data is accepted: input is redacted before it reaches the model.",
        input_fields: &["text"],
        allows_pii: false,
        use_case: "complete",
    },
    AiCapability {
        id: "translate",
        module_id: "ai",
        name: "Translate text",
        description: "Translate caller-supplied text into the requested target language. No personal data is accepted: input is redacted before it reaches the model.",
        input_fields: &["text", "target_language"],
        allows_pii: false,
        use_case: "complete",
    },
];

/// Registry of per-module AI capabilities, keyed by module id.
/// Backed by a `DashMap` so modules can register from `init`
/// without a lock held across the call.
pub struct AiCapabilityRegistry {
    pub(crate) capabilities: DashMap<String, Vec<AiCapability>>,
}

impl AiCapabilityRegistry {
    pub(crate) fn new() -> Self {
        Self {
            capabilities: DashMap::new(),
        }
    }

    /// Register (or replace) the full capability list of one
    /// module. Call once per module, from that module's `init`,
    /// with the module's complete list.
    pub fn register(&self, module_id: &str, capabilities: &[AiCapability]) {
        let mut owned: Vec<AiCapability> = capabilities.to_vec();
        owned.sort_by(|a, b| a.id.cmp(b.id));
        owned.dedup_by(|a, b| a.id == b.id);
        self.capabilities.insert(module_id.to_string(), owned);
    }

    /// Owned copies of one module's capabilities, sorted by id.
    /// An unregistered module yields an empty list.
    pub fn capabilities_for(&self, module_id: &str) -> Vec<AiCapability> {
        self.capabilities
            .get(module_id)
            .map(|entry| entry.value().clone())
            .unwrap_or_default()
    }

    /// Owned copies of every registered capability, sorted by
    /// module id, then capability id.
    pub fn all_capabilities(&self) -> Vec<AiCapability> {
        let mut out: Vec<AiCapability> = self
            .capabilities
            .iter()
            .flat_map(|entry| entry.value().clone())
            .collect();
        out.sort_by(|a, b| a.module_id.cmp(b.module_id).then_with(|| a.id.cmp(b.id)));
        out
    }

    /// One capability by module id and capability id.
    pub fn find(&self, module_id: &str, capability: &str) -> Option<AiCapability> {
        self.capabilities.get(module_id).and_then(|entry| {
            entry
                .value()
                .iter()
                .find(|item| item.id == capability)
                .copied()
        })
    }

    /// True when any registered capability declares this use case.
    /// Used by policy validation so module-scoped use cases are
    /// allowlistable without a hard-coded list.
    pub fn has_use_case(&self, use_case: &str) -> bool {
        self.capabilities.iter().any(|entry| {
            entry
                .value()
                .iter()
                .any(|capability| capability.use_case == use_case)
        })
    }
}

pub(crate) static CAPABILITY_REGISTRY: std::sync::OnceLock<AiCapabilityRegistry> =
    std::sync::OnceLock::new();

/// The shared capability registry, created on first use.
pub fn ai_capability_registry() -> &'static AiCapabilityRegistry {
    CAPABILITY_REGISTRY.get_or_init(AiCapabilityRegistry::new)
}

/// Register one module's AI capabilities. Called from the owning
/// module's `init` with that module's complete capability list.
pub fn register_ai_capabilities(module_id: &'static str, capabilities: &[AiCapability]) {
    ai_capability_registry().register(module_id, capabilities);
}

/// Process-wide handle on the AI module's state, set once in
/// `AiModule::init` from the same `AiState` the HTTP scope serves.
pub(crate) static AI_STATE: std::sync::OnceLock<AiState> = std::sync::OnceLock::new();

/// The shared AI state, or an internal error when the AI module
/// has not initialised yet. Fail closed: no state means no
/// capability call runs.
pub(crate) fn ai_state() -> UnifiedResult<AiState> {
    AI_STATE.get().cloned().ok_or_else(|| {
        UnifiedError::internal("AI module is not initialised: capability calls are unavailable")
    })
}

/// Split grounding-hook results: the first result is the grounding,
/// every later result merges into `extra_context` (object results
/// merge by key; other results land under `result_{index}`).
pub(crate) fn split_grounding(results: &[Value]) -> (Value, Value) {
    let grounding = match results.first() {
        Some(Value::Object(_)) => results[0].clone(),
        Some(other) => json!({ "result": other }),
        None => Value::Null,
    };
    let mut extra = serde_json::Map::new();
    for (index, result) in results.iter().skip(1).enumerate() {
        match result {
            Value::Object(map) => {
                for (key, value) in map {
                    extra.insert(key.clone(), value.clone());
                }
            }
            other => {
                extra.insert(format!("result_{index}"), other.clone());
            }
        }
    }
    (grounding, Value::Object(extra))
}

/// True when a grounding value carries real content: a non-empty
/// string, a non-empty array / object (recursively), or any number
/// or boolean. Empty objects, empty arrays, empty strings, and
/// nulls carry nothing.
pub(crate) fn grounding_has_content(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(_) | Value::Number(_) => true,
        Value::String(text) => !text.trim().is_empty(),
        Value::Array(items) => items.iter().any(grounding_has_content),
        Value::Object(map) => map.values().any(grounding_has_content),
    }
}

/// Render the validated, redacted input fields as prompt text, in
/// declared field order. Non-string values render as compact JSON.
pub(crate) fn render_capability_input(capability: &AiCapability, input: &Value) -> String {
    let mut lines: Vec<String> = Vec::new();
    for field in capability.input_fields {
        match input.get(*field) {
            Some(Value::String(text)) => lines.push(format!("{field}: {text}")),
            Some(other) => lines.push(format!("{field}: {other}")),
            None => lines.push(format!("{field}: (missing)")),
        }
    }
    lines.join("\n")
}

/// Truncate text to at most `max` characters, keeping whole
/// characters.
pub(crate) fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        text.chars().take(max).collect()
    }
}

/// Run one registered AI capability end to end, without
/// purchased-product context (see `run_ai_capability_with_product`
/// for the HTTP path, which resolves the product from the
/// request). Kept so existing programmatic callers keep working
/// unchanged.
pub async fn run_ai_capability(
    ctx: &AppContext,
    caller: &Caller,
    company_id: Option<Uuid>,
    module_id: &str,
    capability: &str,
    input: Value,
) -> UnifiedResult<Value> {
    run_ai_capability_with_product(ctx, caller, company_id, None, module_id, capability, input)
        .await
}

/// Run one registered AI capability end to end, with the caller's
/// purchased-product context (`X-Product-Id`, resolved by the HTTP
/// endpoint) so the product gate layer checks the purchased
/// product instead of defaulting to allow. See the section header
/// for the step order and the security notes. Denials are 403s
/// with reason codes; unknown module / capability is a 404 with
/// reason `ai_capability_not_found` in metadata; input validation
/// failures are 422s. No prompt content is ever logged or placed
/// in an audit payload.
pub async fn run_ai_capability_with_product(
    ctx: &AppContext,
    caller: &Caller,
    company_id: Option<Uuid>,
    product_id: Option<Uuid>,
    module_id: &str,
    capability: &str,
    input: Value,
) -> UnifiedResult<Value> {
    let request_id = Uuid::now_v7();
    // (a) Look the capability up in the registry.
    let found = ai_capability_registry()
        .find(module_id, capability)
        .ok_or_else(|| {
            UnifiedError::not_found(format!("AI capability not found: {module_id}.{capability}"))
                .with_metadata(
                    "reason",
                    Value::String("ai_capability_not_found".to_string()),
                )
                .with_metadata("module_id", Value::String(module_id.to_string()))
                .with_metadata("capability", Value::String(capability.to_string()))
        })?;
    // (b) The input must be an object with exactly the declared fields.
    let object = input.as_object().ok_or_else(|| {
        UnifiedError::unprocessable("input must be a JSON object")
            .with_metadata("module_id", Value::String(module_id.to_string()))
            .with_metadata("capability", Value::String(capability.to_string()))
    })?;
    for field in found.input_fields {
        if !object.contains_key(*field) {
            return Err(UnifiedError::unprocessable(format!(
                "input is missing required field \"{field}\""
            ))
            .with_metadata("field", Value::String(field.to_string()))
            .with_metadata("module_id", Value::String(module_id.to_string()))
            .with_metadata("capability", Value::String(capability.to_string())));
        }
    }
    for key in object.keys() {
        if !found
            .input_fields
            .iter()
            .any(|field| *field == key.as_str())
        {
            return Err(UnifiedError::unprocessable(format!(
                "input contains unexpected field \"{key}\""
            ))
            .with_metadata("field", Value::String(key.clone()))
            .with_metadata("module_id", Value::String(module_id.to_string()))
            .with_metadata("capability", Value::String(capability.to_string())));
        }
    }
    if input.to_string().len() > MAX_PROMPT_LEN {
        return Err(UnifiedError::bad_request(format!(
            "input must be at most {MAX_PROMPT_LEN} characters in total"
        )));
    }
    // (c) Redact PII unless the capability explicitly allows it.
    // The privacy module owns redaction (`redact_value`); there is
    // no `privacy.redact_requested` hook, so the public helper is
    // called directly. Names are not redacted here (`false`):
    // field values that name a person stay usable for drafting,
    // while emails, phones, and ID-like numbers are masked.
    let (effective_input, pii_redacted) = if found.allows_pii {
        (input.clone(), false)
    } else {
        (privacy::redact_value(&input, false), true)
    };
    // (d) Grounding hook: `<module>.ai.<capability>_requested`.
    let event = format!("{module_id}.ai.{capability}_requested");
    let hook = ctx.hooks.emit(
        &event,
        json!({
            "request_id": request_id.to_string(),
            "module_id": module_id,
            "capability": capability,
            "caller": {
                "user_id": caller_user_id(caller).map(|id| id.to_string()),
                "company_id": company_id.map(|id| id.to_string()),
            },
            "input": effective_input,
        }),
    );
    let (grounding, extra_context) = split_grounding(&hook.results);
    let grounded = grounding_has_content(&grounding);
    let grounding_text = if grounded {
        truncate_chars(
            &serde_json::to_string_pretty(&grounding).unwrap_or_default(),
            MAX_CONTEXT_TEXT_LEN,
        )
    } else {
        String::new()
    };
    let extra_text = if grounding_has_content(&extra_context) {
        truncate_chars(
            &serde_json::to_string_pretty(&extra_context).unwrap_or_default(),
            MAX_CONTEXT_TEXT_LEN,
        )
    } else {
        String::new()
    };
    // Prompt: the validated input fields, rendered in declared
    // order. Grounding travels in the system prompt only, like the
    // draft endpoints do.
    let prompt_text = truncate_chars(
        &render_capability_input(&found, &effective_input),
        MAX_PROMPT_LEN,
    );
    let system_prompt = if grounded || !extra_text.is_empty() {
        let mut text = format!(
            "You are the \"{}\" capability of the \"{}\" module. Return only the output text, in the language of the input unless the capability asks otherwise. Do not publish, send, or act on anything; only return text.",
            found.name, found.module_id
        );
        if !grounding_text.is_empty() {
            text.push_str(&format!(
                "\n\nGrounding (use only this context; do not invent facts):\n{grounding_text}"
            ));
        }
        if !extra_text.is_empty() {
            text.push_str(&format!("\n\nAdditional context:\n{extra_text}"));
        }
        Some(text)
    } else {
        Some(format!(
            "You are the \"{}\" capability of the \"{}\" module. Return only the output text, in the language of the input unless the capability asks otherwise. Do not publish, send, or act on anything; only return text.",
            found.name, found.module_id
        ))
    };
    // Resolve the provider connection (not provider traffic), then
    // (e) run the allowlist gate FIRST, before any provider traffic.
    let state = ai_state()?;
    let connection = resolve_connection(&state, None)?;
    let auth = authorize_ai_call(
        ctx,
        &state,
        caller,
        company_id,
        &AiCallRequest {
            use_case: found.use_case,
            model: &connection.model,
            provider_id: connection.id,
            provider_kind: connection.kind.as_str(),
            estimated_tokens: estimate_prompt_tokens(&prompt_text) + DEFAULT_MAX_TOKENS,
            estimated_completion_tokens: DEFAULT_MAX_TOKENS,
            use_personal_key: false,
            product_id,
        },
    )?;
    // (e2) Package-entitlement gate, after authorization and
    // before any provider traffic. A sellable non-core module may
    // only run a capability for a tenant that holds an entitlement
    // for it, resolved through the `entitlements.check_requested`
    // hook query (answered by the entitlements module from its
    // stored rows; the event name is the literal of
    // `entitlements::EVENT_CHECK_REQUESTED` - the const cannot be
    // referenced here until that module is declared in `mod.rs`).
    // The answer is read fail-closed, like `role.check`: only an
    // explicit `allowed: true` - on the payload first, then in the
    // contributed results - passes. No answer, an explicit
    // `false`, or a malformed answer denies the call with a 403
    // and the provider is never contacted. Core modules and
    // modules that are not sellable pass from the registry alone,
    // mirroring the SaaS packages hook path. A capability
    // registered for a module the registry does not know is
    // inconsistent state and also denies. Control-plane calls
    // carry no company id, so there is no tenant to entitle; they
    // are covered by the allowlist gate in (e) above.
    if let Some(tenant_company_id) = company_id {
        let gated = match ctx.modules.get(module_id) {
            Some(module) => module.available_as_product() && !module.is_core(),
            None => true,
        };
        if gated {
            let answered = ctx.hooks.emit(
                "entitlements.check_requested",
                json!({
                    "company_id": tenant_company_id.to_string(),
                    "module_id": module_id,
                    "allowed": false,
                }),
            );
            let allowed = if answered.results.is_empty() {
                false
            } else {
                answered
                    .get("allowed")
                    .and_then(|value| value.as_bool())
                    .or_else(|| {
                        answered
                            .results
                            .iter()
                            .filter_map(|value| {
                                value.get("allowed").and_then(|allowed| allowed.as_bool())
                            })
                            .next()
                    })
                    .unwrap_or(false)
            };
            if !allowed {
                // The call was authorized (and reserved cap budget)
                // but never ran: release the reservation.
                release_cap_reservation(&state, auth.reservation_id);
                return Err(UnifiedError::forbidden(format!(
                    "Tenant is not entitled to module \"{module_id}\""
                ))
                .with_metadata("reason", Value::String("entitlement.required".to_string()))
                .with_metadata("module_id", Value::String(module_id.to_string()))
                .with_metadata("capability", Value::String(capability.to_string())));
            }
        }
    }
    // (f) Run the completion through the shared path; usage
    // recording and the `ai.call.completed` / `ai.call.failed`
    // events happen inside `run_completion` (g).
    let call = CompletionCall {
        prompt: prompt_text,
        system_prompt,
        max_tokens: DEFAULT_MAX_TOKENS,
        temperature: None,
    };
    let outcome = run_completion(
        &state,
        ctx,
        &caller.actor,
        &connection,
        &call,
        found.use_case,
        &auth,
    )
    .await?;
    // (h) Return the output inline.
    Ok(json!({
        "capability": found.id,
        "module_id": found.module_id,
        "output": outcome.text,
        "model": outcome.model,
        "grounded": grounded,
        "pii_redacted": pii_redacted,
        "usage_id": outcome.usage.id.to_string(),
    }))
}

// Capability HTTP handlers.

/// Body of `POST /capabilities/{module_id}/{capability}`.
#[derive(Debug, Deserialize)]
pub struct CapabilityCallInput {
    pub input: Value,
}

/// List every registered capability, paginated and filterable by
/// `module_id` and free-text search over id / name / description.
pub(crate) async fn list_capabilities(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company_uuid(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_USE)?;
        let params = query.into_inner();
        let module_filter = params.filters.get("module_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<AiCapability> = ai_capability_registry()
            .all_capabilities()
            .into_iter()
            .filter(|capability| {
                if let Some(module_id) = &module_filter {
                    if capability.module_id != module_id {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {} {} {}",
                        capability.module_id,
                        capability.id,
                        capability.name,
                        capability.description
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| a.module_id.cmp(b.module_id).then_with(|| a.id.cmp(b.id)));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// List one module's registered capabilities, paginated.
pub(crate) async fn list_module_capabilities(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let module_id = path.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company_uuid(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_USE)?;
        let params = query.into_inner();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<AiCapability> = ai_capability_registry()
            .capabilities_for(&module_id)
            .into_iter()
            .filter(|capability| {
                if search.is_empty() {
                    return true;
                }
                let haystack = format!(
                    "{} {} {}",
                    capability.id, capability.name, capability.description
                )
                .to_lowercase();
                haystack.contains(&search)
            })
            .collect();
        items.sort_by(|a, b| a.id.cmp(b.id));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Run one registered capability: `POST
/// /capabilities/{module_id}/{capability}` with body
/// `{"input": {...}}`. Requires `ai:use`; delegates to
/// `run_ai_capability`.
pub(crate) async fn run_capability_endpoint(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<CapabilityCallInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let (module_id, capability) = path.into_inner();
    let input = body.into_inner().input;
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, debug),
    };
    let _scope = match caller_company_uuid(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err(error, debug),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_AI_USE) {
        return err(error, debug);
    }
    let company_id = resolve_capability_company(&ctx, &req, &caller);
    match run_ai_capability_with_product(
        &ctx,
        &caller,
        company_id,
        resolve_caller_product(&req),
        &module_id,
        &capability,
        input,
    )
    .await
    {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Run one provider call end to end: resolve and rate-limit the
/// connection, decrypt the key, call the adapter, record usage, and
/// emit the audit event. A disabled connection is refused before any
/// provider traffic.
pub(crate) async fn run_completion(
    state: &AiState,
    ctx: &AppContext,
    actor: &str,
    connection: &AiProviderConnection,
    call: &CompletionCall,
    use_case: &str,
    auth: &AiAuthorization,
) -> UnifiedResult<CompletionOutcome> {
    if !connection.enabled {
        release_cap_reservation(state, auth.reservation_id);
        return Err(UnifiedError::bad_request(format!(
            "AI provider connection \"{}\" is disabled",
            connection.name
        )));
    }
    if let Err(error) = check_rate_limit(state, connection, auth.user_id) {
        release_cap_reservation(state, auth.reservation_id);
        return Err(error);
    }
    // The key comes from the authorization: the caller's personal key
    // when the call runs on it, otherwise the connection's key. The
    // personal key was decrypted once by `authorize_ai_call` and
    // lives only for this call.
    let api_key = if let Some(personal) = &auth.personal_key {
        Some(personal.decrypted_key.clone())
    } else {
        let secret = match encryption_secret(ctx) {
            Ok(secret) => secret,
            Err(error) => {
                release_cap_reservation(state, auth.reservation_id);
                return Err(error);
            }
        };
        match &connection.api_key_encrypted {
            Some(stored) => match decrypt_api_key(&secret, stored) {
                Ok(key) => Some(key),
                Err(error) => {
                    release_cap_reservation(state, auth.reservation_id);
                    return Err(error);
                }
            },
            None => None,
        }
    };
    let adapter = match ai_provider_registry().build(connection, api_key) {
        Ok(adapter) => adapter,
        Err(error) => {
            release_cap_reservation(state, auth.reservation_id);
            return Err(error);
        }
    };
    let started = Instant::now();
    let result = adapter.complete(call).await;
    let latency_ms = started.elapsed().as_millis() as u64;
    match result {
        Ok(raw) => {
            let usage = record_usage(
                state,
                connection,
                actor,
                auth.user_id,
                auth.company_id,
                use_case,
                Some(&raw),
                latency_ms,
                None,
                auth.use_personal_key,
                estimate_prompt_tokens(&call.prompt),
            );
            // Reconcile the cap reservation with the actual usage
            // just recorded: the estimate is replaced by the real
            // counts, so the reservation must go.
            release_cap_reservation(state, auth.reservation_id);
            emit_audit(
                ctx,
                EVENT_AI_CALL_COMPLETED,
                actor,
                &format!("ai-provider:{}", connection.id),
                json!({
                    "id": usage.id.to_string(),
                    "provider_id": connection.id.to_string(),
                    "provider_kind": connection.kind.as_str(),
                    "use_case": use_case,
                    "model": connection.model.clone(),
                    "prompt_tokens": raw.prompt_tokens,
                    "completion_tokens": raw.completion_tokens,
                    "usage_id": usage.id.to_string(),
                }),
            );
            Ok(CompletionOutcome {
                text: raw.text,
                usage,
                provider_id: connection.id,
                provider_kind: connection.kind.as_str().to_string(),
                model: connection.model.clone(),
            })
        }
        Err(error) => {
            let usage = record_usage(
                state,
                connection,
                actor,
                auth.user_id,
                auth.company_id,
                use_case,
                None,
                latency_ms,
                Some(error.code),
                auth.use_personal_key,
                estimate_prompt_tokens(&call.prompt),
            );
            release_cap_reservation(state, auth.reservation_id);
            emit_audit(
                ctx,
                EVENT_AI_CALL_FAILED,
                actor,
                &format!("ai-provider:{}", connection.id),
                json!({
                    "id": usage.id.to_string(),
                    "provider_id": connection.id.to_string(),
                    "provider_kind": connection.kind.as_str(),
                    "use_case": use_case,
                    "model": connection.model.clone(),
                    "error_code": error.code,
                    "usage_id": usage.id.to_string(),
                }),
            );
            Err(error)
        }
    }
}

// Helpers: drafts and hook-gathered context.

#[allow(clippy::too_many_arguments)]
pub(crate) fn store_draft(
    state: &AiState,
    ctx: &Arc<AppContext>,
    kind: DraftKind,
    title: Option<String>,
    subject: Option<String>,
    body: String,
    prompt: Option<String>,
    outcome: &CompletionOutcome,
    actor: &str,
    target_type: Option<String>,
    target_id: Option<String>,
    sources: Vec<GroundingSource>,
) -> AiDraft {
    let now = Utc::now();
    let draft = AiDraft {
        id: Uuid::now_v7(),
        kind,
        title,
        subject,
        body,
        prompt,
        provider_id: Some(outcome.provider_id),
        provider_kind: outcome.provider_kind.clone(),
        model: outcome.model.clone(),
        actor: actor.to_string(),
        status: DraftStatus::Draft,
        ai_generated: true,
        target_type,
        target_id,
        sources,
        usage_id: Some(outcome.usage.id),
        reviewed_by: None,
        reviewed_at: None,
        created_at: now,
        updated_at: now,
    };
    state.drafts.insert(draft.id, draft.clone());
    emit_audit(
        ctx,
        EVENT_AI_DRAFT_CREATED,
        actor,
        &format!("ai-draft:{}", draft.id),
        json!({
            "id": draft.id.to_string(),
            "kind": kind.as_str(),
            "status": "draft",
            "ai_generated": true,
            "provider_id": outcome.provider_id.to_string(),
            "usage_id": outcome.usage.id.to_string(),
            "target_type": draft.target_type,
            "target_id": draft.target_id,
        }),
    );
    draft
}

/// Gather a ticket or chat transcript through the hook bus. The
/// owning module may answer with `{ "transcript": "..." }`, with
/// `{ "messages": [{ "role", "content" }] }`, or with `{ "title",
/// "body" }`. Returns `None` when no handler answers, so the caller
/// falls back to its own supplied text.
pub(crate) fn hook_transcript(ctx: &AppContext, event: &str, payload: Value) -> Option<String> {
    let hook = ctx.hooks.emit(event, payload);
    let mut lines: Vec<String> = Vec::new();
    for result in &hook.results {
        if let Some(transcript) = result.get("transcript").and_then(|value| value.as_str()) {
            if !transcript.trim().is_empty() {
                lines.push(transcript.trim().to_string());
            }
        }
        if let Some(messages) = result.get("messages").and_then(|value| value.as_array()) {
            for message in messages {
                let role = message
                    .get("role")
                    .and_then(|value| value.as_str())
                    .unwrap_or("message");
                let content = message
                    .get("content")
                    .or_else(|| message.get("body"))
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                if !content.trim().is_empty() {
                    lines.push(format!("{role}: {}", content.trim()));
                }
            }
        }
        let title = result.get("title").and_then(|value| value.as_str());
        let body = result.get("body").and_then(|value| value.as_str());
        if let (Some(title), Some(body)) = (title, body) {
            lines.push(format!("{title}\n{body}"));
        }
    }
    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

/// Validate the shared completion options (token budget and
/// temperature) from any draft/completion input.
pub(crate) fn completion_options(
    max_tokens: Option<u32>,
    temperature: Option<f32>,
) -> UnifiedResult<(u32, Option<f32>)> {
    let max_tokens = max_tokens.unwrap_or(DEFAULT_MAX_TOKENS);
    if max_tokens == 0 || max_tokens > MAX_MAX_TOKENS {
        return Err(UnifiedError::bad_request(format!(
            "max_tokens must be between 1 and {MAX_MAX_TOKENS}"
        )));
    }
    let temperature = match temperature {
        Some(value) if !(0.0..=2.0).contains(&value) => {
            return Err(UnifiedError::bad_request(
                "temperature must be between 0.0 and 2.0",
            ));
        }
        value => value,
    };
    Ok((max_tokens, temperature))
}

#[cfg(test)]
mod tests {
    use super::truncate_chars;

    #[test]
    fn truncate_keeps_short_text() {
        assert_eq!(truncate_chars("hello", 10), "hello");
        assert_eq!(truncate_chars("hello", 5), "hello");
    }

    #[test]
    fn truncate_cuts_to_max_chars() {
        assert_eq!(truncate_chars("hello world", 5), "hello");
        assert_eq!(truncate_chars("", 5), "");
    }

    #[test]
    fn truncate_keeps_whole_characters() {
        assert_eq!(truncate_chars("héllo", 2), "hé");
    }
}
