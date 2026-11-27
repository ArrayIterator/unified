//! Hook handlers for user deletion, the AI verification
//! grounding hook, and the MCP context source.
//!
//! The AI grounding hook is the point where the declared
//! default context and extra instructions reach the AI
//! module's gated runner: when `run_ai_capability` runs the
//! `verify_identity_documents` capability it emits
//! `kyc.ai.verify_identity_documents_requested`, and this
//! handler answers with the resolved default context, the
//! active extra instructions, and the case's document metadata
//! (references and hashes only - never file bytes). The AI
//! module merges that grounding into the system prompt, so
//! the behaviour the AI follows is the stored configuration,
//! not text hard-coded in this module.

use super::state::KycState;
use super::types::{
    resolve_verification_context, DELETED_USER_ID, EVENT_AI_VERIFY_REQUESTED, EVENT_MCP_CONTEXT_KYC,
};
use chrono::Utc;
use serde_json::json;
use unified_core::hooks::HookContext;
use uuid::Uuid;

/// Marker for anonymized rows is `deleted-user` (see types.rs).

// User deletion (users module owns the event)

/// `user.deleted` carries `user_id` (falling back to `id`, the
/// same shape the forums module reads). The user's cases are
/// anonymized to `deleted-user` so the verification history
/// stays auditable without the identity; linked documents they
/// uploaded and audit entries they authored are anonymized the
/// same way. Cases are never deleted: a verification decision
/// must stay on record.
pub(crate) fn on_user_deleted(state: KycState, ctx: &mut HookContext) {
    let user_id = ctx
        .get("user_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let Some(user_id) = user_id else {
        ctx.add_result(json!({ "kyc": "skipped", "reason": "user.deleted carries no user_id" }));
        return;
    };
    let now = Utc::now();
    let mut anonymized_cases = 0i64;
    for mut entry in state.cases.iter_mut() {
        let mut changed = false;
        if entry.value().subject_id == user_id {
            entry.value_mut().subject_id = DELETED_USER_ID.to_string();
            changed = true;
        }
        if entry.value().submitted_by == user_id {
            entry.value_mut().submitted_by = DELETED_USER_ID.to_string();
            changed = true;
        }
        if entry.value().reviewed_by.as_deref() == Some(user_id.as_str()) {
            entry.value_mut().reviewed_by = Some(DELETED_USER_ID.to_string());
            changed = true;
        }
        if changed {
            entry.value_mut().updated_at = now;
            anonymized_cases += 1;
        }
    }
    let mut anonymized_documents = 0i64;
    for mut entry in state.documents.iter_mut() {
        if entry.value().uploaded_by == user_id {
            entry.value_mut().uploaded_by = DELETED_USER_ID.to_string();
            anonymized_documents += 1;
        }
    }
    let mut anonymized_audit = 0i64;
    for mut entry in state.audit_entries.iter_mut() {
        if entry.value().actor_id == user_id {
            entry.value_mut().actor_id = DELETED_USER_ID.to_string();
            anonymized_audit += 1;
        }
    }
    ctx.add_result(json!({
        "kyc": "user_deleted",
        "user_id": user_id,
        "anonymized_cases": anonymized_cases,
        "anonymized_documents": anonymized_documents,
        "anonymized_audit_entries": anonymized_audit,
    }));
}

// AI verification grounding (ai module owns the event)

/// Answer `kyc.ai.verify_identity_documents_requested` with
/// the grounding the AI runner merges into the system prompt:
/// the resolved default verification context (which fields the
/// AI may see, which checks to run, which version), the active
/// extra instructions in merge order, and the case's document
/// metadata (type codes, document references, content hashes -
/// never file bytes). The payload carries `case_id` and
/// `tenant_id` under the input the verify handler passed to
/// `run_ai_capability`. A payload naming no case, or a case
/// this module cannot find, is skipped, not failed: the runner
/// then proceeds with the caller's input alone.
pub(crate) fn on_ai_verification_requested(state: KycState, ctx: &mut HookContext) {
    let input = ctx.get("input").cloned().unwrap_or(json!({}));
    let case_id = input
        .get("case_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok());
    let Some(case_id) = case_id else {
        ctx.add_result(
            json!({ "kyc": "skipped", "reason": "verification grounding carries no case_id" }),
        );
        return;
    };
    let Some(case) = state.cases.get(&case_id).map(|entry| entry.value().clone()) else {
        ctx.add_result(json!({ "kyc": "skipped", "case_id": case_id.to_string(), "reason": "no KYC case with that id" }));
        return;
    };
    let documents: Vec<serde_json::Value> = state
        .case_documents(case_id)
        .iter()
        .map(|document| {
            json!({
                "document_type_code": document.document_type_code,
                "document_id": document.document_id,
                "content_hash": document.content_hash,
                "file_name": document.file_name,
                "mime_type": document.mime_type,
            })
        })
        .collect();
    let mut grounding = json!({
        "case_id": case.id.to_string(),
        "subject_type": case.subject_type,
        "status": case.status.as_str(),
        "documents": documents,
    });
    // The resolved default context + extra instructions travel
    // as grounding, so the model follows the stored
    // configuration. When the tenant has no active context the
    // grounding carries only the case data; the verify handler
    // itself refuses to run without an active context, so this
    // branch is only a safety net.
    if let Some(context) = state.active_context(&case.tenant_id) {
        let instructions = state.applicable_instructions(&case.tenant_id, context.id);
        let resolved = resolve_verification_context(&context, &instructions);
        if let Some(map) = grounding.as_object_mut() {
            map.insert(
                "verification_context".to_string(),
                json!({
                    "name": resolved.context_name,
                    "version": resolved.context_version,
                    "visible_fields": resolved.visible_fields,
                    "checks": resolved.checks,
                }),
            );
            map.insert(
                "additional_instructions".to_string(),
                json!(resolved.instruction_texts),
            );
            map.insert(
                "instruction_versions".to_string(),
                json!(resolved.instruction_versions),
            );
        }
    }
    ctx.add_result(grounding);
}

/// The grounding event name, for registration in `module.rs`.
pub(crate) fn ai_verification_event() -> &'static str {
    EVENT_AI_VERIFY_REQUESTED
}

// MCP context source (mcp module owns the `mcp.context.*` contract)

/// Answer `mcp.context.kyc` for MCP servers: per-status case
/// counts and recent case summaries for one tenant. The MCP
/// module's fetch passes `server_id`, `company_id`, `site_id`,
/// `page`, `per_page`, `search`, and `filters`; this source
/// additionally requires `tenant_id` in the payload, because
/// KYC rows are tenant-scoped and a company id alone cannot
/// prove which tenant the caller may see. Without a tenant id
/// the handler contributes nothing, so the MCP module reports
/// the source honestly as a gap instead of inventing context.
/// Summaries carry no subject identity beyond the subject type
/// and no document references.
pub(crate) fn on_mcp_context_kyc(state: KycState, ctx: &mut HookContext) {
    let Some(tenant_id) = ctx
        .get("tenant_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
    else {
        return;
    };
    let page = ctx
        .get("page")
        .and_then(|value| value.as_u64())
        .unwrap_or(1)
        .max(1);
    let per_page = ctx
        .get("per_page")
        .and_then(|value| value.as_u64())
        .unwrap_or(20)
        .clamp(1, 100);
    let search = ctx
        .get("search")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_lowercase();
    let status_filter = ctx
        .get("filters")
        .and_then(|value| value.get("status"))
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let mut cases: Vec<_> = state
        .cases
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|case| {
            if case.tenant_id != tenant_id {
                return false;
            }
            if let Some(status) = &status_filter {
                if case.status.as_str() != status {
                    return false;
                }
            }
            if !search.is_empty() && !case.subject_type.to_lowercase().contains(&search) {
                return false;
            }
            true
        })
        .collect();
    cases.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    let total = cases.len() as u64;
    let start = ((page - 1) * per_page) as usize;
    let items: Vec<serde_json::Value> = cases
        .iter()
        .skip(start)
        .take(per_page as usize)
        .map(|case| {
            json!({
                "case_id": case.id.to_string(),
                "subject_type": case.subject_type,
                "status": case.status.as_str(),
                "risk_score": case.risk_score,
                "risk_level_code": case.risk_level_code,
                "escalated": case.escalated,
                "updated_at": case.updated_at.to_rfc3339(),
            })
        })
        .collect();
    let mut by_status = serde_json::Map::new();
    for case in &cases {
        let count = by_status
            .get(case.status.as_str())
            .and_then(|value| value.as_u64())
            .unwrap_or(0);
        by_status.insert(case.status.as_str().to_string(), json!(count + 1));
    }
    ctx.add_result(json!({
        "items": items,
        "total": total,
        "summary": { "by_status": by_status },
    }));
}

/// The MCP context event name, for registration in `module.rs`.
pub(crate) fn mcp_context_event() -> &'static str {
    EVENT_MCP_CONTEXT_KYC
}
