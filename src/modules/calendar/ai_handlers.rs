//! calendar module - AI draft handlers and AI grounding answers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::{NaiveDate, TimeZone, Utc};
use serde_json::{Value, json};
use std::sync::Arc;

use super::aggregation::aggregate_items;
use super::constants::EVENT_AI_DRAFT_REQUESTED;
use super::constants::MAX_AI_ITEMS;
use super::constants::PERM_AI_USE;
use super::constants::PERM_CALENDAR_READ;
use super::helpers::Caller;
use super::helpers::caller;
use super::helpers::err;
use super::helpers::ok;
use super::helpers::require_permission;
use super::state::CalendarState;
use super::types::AiDraftRequest;
use super::types::AiDraftResponse;

/// Shared AI draft flow for `/ai/summarize-day` and
/// `/ai/whats-next`: build context from the caller's own
/// aggregated items for one day, emit `calendar.ai.draft_requested`,
/// and return the first hook result that carries a draft string.
/// Fails closed (503) when no AI handler answers — a summary is
/// never fabricated locally. Drafts only: the AI never creates,
/// moves, or deletes a calendar item.
pub(crate) fn request_ai_draft(
    ctx: &AppContext,
    caller: &Caller,
    kind: &str,
    date: NaiveDate,
    context_text: String,
    item_ids: Vec<String>,
) -> UnifiedResult<AiDraftResponse> {
    let answer = ctx.hooks.emit(
        EVENT_AI_DRAFT_REQUESTED,
        json!({
            "tenant_id": caller.tenant_id,
            "user_id": caller.actor,
            "role": caller.role,
            "kind": kind,
            "date": date.to_string(),
            "item_ids": item_ids,
            "context_text": context_text,
            "requested_at": Utc::now().to_rfc3339(),
        }),
    );
    for result in &answer.results {
        let draft = result
            .get("draft")
            .and_then(Value::as_str)
            .or_else(|| result.get("text").and_then(Value::as_str));
        if let Some(draft) = draft {
            if draft.trim().is_empty() {
                continue;
            }
            return Ok(AiDraftResponse {
                kind: kind.to_string(),
                draft: draft.to_string(),
                ai_generated: true,
                status: "draft".to_string(),
                item_ids,
                user_id: caller.actor.clone(),
                model: result
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                provider: result
                    .get("provider")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                requested_at: Utc::now(),
            });
        }
        if let Some(error) = result.get("error").and_then(Value::as_str) {
            return Err(UnifiedError::new(
                format!("AI draft request failed: {error}"),
                502,
            ));
        }
    }
    Err(UnifiedError::new(
        "AI draft service did not answer calendar.ai.draft_requested: no handler is registered for calendar drafts",
        503,
    ))
}

/// Build the grounding text for one day: one line per item.
pub(crate) fn day_context_text(
    ctx: &AppContext,
    state: &CalendarState,
    caller: &Caller,
    date: NaiveDate,
) -> (String, Vec<String>) {
    let day_start = Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).expect("midnight is valid"));
    let day_end = day_start + chrono::Duration::days(1);
    let (items, _) = aggregate_items(ctx, state, caller, day_start, day_end, None);
    let mut lines: Vec<String> = Vec::new();
    let mut ids: Vec<String> = Vec::new();
    for item in items.iter().take(MAX_AI_ITEMS) {
        let end = item.effective_end();
        lines.push(format!(
            "- [{}] {} ({} to {}, visibility: {})",
            item.module,
            item.title,
            item.start.to_rfc3339(),
            end.to_rfc3339(),
            item.visibility,
        ));
        ids.push(format!("{}:{}", item.module, item.id));
    }
    (lines.join("\n"), ids)
}

pub(crate) fn parse_ai_date(raw: Option<&str>) -> UnifiedResult<NaiveDate> {
    match raw {
        Some(raw) if !raw.trim().is_empty() => NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d")
            .map_err(|_| {
                UnifiedError::bad_request(
                    "date must be an ISO calendar date such as \"2026-11-23\"",
                )
            }),
        _ => Ok(Utc::now().date_naive()),
    }
}

/// POST /api/v1/calendar/ai/summarize-day — AI draft summary of
/// the caller's calendar day.
pub(crate) async fn ai_summarize_day(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CalendarState>,
    req: HttpRequest,
    body: web::Json<AiDraftRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AiDraftResponse> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALENDAR_READ)?;
        require_permission(&ctx, &caller.role, PERM_AI_USE)?;
        let date = parse_ai_date(body.date.as_deref())?;
        let (context_text, item_ids) = day_context_text(&ctx, &state, &caller, date);
        if item_ids.is_empty() {
            return Err(UnifiedError::bad_request(
                "No calendar items on that date to summarize",
            ));
        }
        request_ai_draft(&ctx, &caller, "summarize_day", date, context_text, item_ids)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// POST /api/v1/calendar/ai/whats-next — AI draft briefing of
/// what comes next on the caller's calendar from the given date.
pub(crate) async fn ai_whats_next(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CalendarState>,
    req: HttpRequest,
    body: web::Json<AiDraftRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AiDraftResponse> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALENDAR_READ)?;
        require_permission(&ctx, &caller.role, PERM_AI_USE)?;
        let date = parse_ai_date(body.date.as_deref())?;
        let (context_text, item_ids) = day_context_text(&ctx, &state, &caller, date);
        if item_ids.is_empty() {
            return Err(UnifiedError::bad_request(
                "No calendar items on that date to brief on",
            ));
        }
        request_ai_draft(&ctx, &caller, "whats_next", date, context_text, item_ids)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
