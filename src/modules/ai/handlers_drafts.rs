//! ai module - HTTP handlers for completions and drafts.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

use super::capabilities::completion_options;
use super::capabilities::hook_transcript;
use super::capabilities::run_completion;
use super::capabilities::store_draft;
use super::constants::EVENT_AI_DRAFT_APPROVED;
use super::constants::EVENT_AI_DRAFT_REJECTED;
use super::constants::EVENT_CHAT_CONTEXT_REQUESTED;
use super::constants::EVENT_TICKET_CONTEXT_REQUESTED;
use super::constants::MAX_CONTEXT_TEXT_LEN;
use super::constants::MAX_PROMPT_LEN;
use super::constants::PERM_AI_APPROVE;
use super::constants::PERM_AI_USE;
use super::context::assemble_shared_context;
use super::context::shared_context_prompt;
use super::gating::personal_mcp_grounding_sources;
use super::handlers_providers::authorize_call;
use super::helpers::Caller;
use super::helpers::caller;
use super::helpers::caller_company_uuid;
use super::helpers::caller_user_id;
use super::helpers::check_capability;
use super::helpers::created;
use super::helpers::emit_audit;
use super::helpers::err;
use super::helpers::ok;
use super::helpers::optional_text;
use super::helpers::paginate;
use super::helpers::parse_uuid;
use super::helpers::require_permission;
use super::helpers::require_tenant;
use super::helpers::require_text;
use super::inputs::AnswerInput;
use super::inputs::CompleteInput;
use super::inputs::ContentDraftInput;
use super::inputs::EmailDraftInput;
use super::inputs::SupportDraftInput;
use super::providers::CompletionCall;
use super::state::AiState;
use super::types::AiDraft;
use super::types::DraftKind;
use super::types::DraftStatus;
use super::validation::validate_use_case;

pub(crate) async fn complete(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    body: web::Json<CompleteInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, debug),
    };
    let _scope = match caller_company_uuid(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err(error, debug),
    };
    let input = body.into_inner();
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_AI_USE) {
        return err(error, debug);
    }
    let prompt = match require_text(&input.prompt, "prompt", MAX_PROMPT_LEN) {
        Ok(prompt) => prompt,
        Err(error) => return err(error, debug),
    };
    let system_prompt = match optional_text(&input.system_prompt, "system_prompt", MAX_PROMPT_LEN) {
        Ok(system_prompt) => system_prompt,
        Err(error) => return err(error, debug),
    };
    let (max_tokens, temperature) = match completion_options(input.max_tokens, input.temperature) {
        Ok(options) => options,
        Err(error) => return err(error, debug),
    };
    let site = input.site.clone().unwrap_or_default();
    let grounding_query = input
        .grounding_query
        .clone()
        .unwrap_or_else(|| prompt.clone());
    let mut shared = assemble_shared_context(Some(&ctx.hooks), &grounding_query, &site, true);
    shared
        .grounding_articles
        .extend(personal_mcp_grounding_sources(
            &state,
            caller_user_id(&caller),
        ));
    let context_text = shared_context_prompt(&shared);
    let full_system = match system_prompt {
        Some(system) if !context_text.is_empty() => {
            Some(format!("{system}\n\nSite context:\n{context_text}"))
        }
        Some(system) => Some(system),
        None if !context_text.is_empty() => Some(format!("Site context:\n{context_text}")),
        None => None,
    };
    let call = CompletionCall {
        prompt: prompt.clone(),
        system_prompt: full_system,
        max_tokens,
        temperature,
    };
    let use_case = match input.use_case.clone() {
        Some(use_case) => match validate_use_case(&use_case) {
            Ok(use_case) => use_case,
            Err(error) => return err(error, debug),
        },
        None => "complete".to_string(),
    };
    let (connection, auth) = match authorize_call(
        &ctx,
        &state,
        &req,
        &caller,
        input.provider_id,
        &prompt,
        max_tokens,
        &use_case,
        input.use_personal_key,
    ) {
        Ok(authorization) => authorization,
        Err(error) => return err(error, debug),
    };
    match run_completion(
        &state,
        &ctx,
        &caller.actor,
        &connection,
        &call,
        &use_case,
        &auth,
    )
    .await
    {
        Ok(outcome) => {
            let draft = store_draft(
                &state,
                &ctx,
                DraftKind::Generic,
                None,
                None,
                outcome.text.clone(),
                Some(prompt),
                &outcome,
                &caller.actor,
                None,
                None,
                shared.grounding_articles,
            );
            created(draft)
        }
        Err(error) => err(error, debug),
    }
}

/// Shared implementation for the ticket-reply and chat-reply draft
/// endpoints. Context is gathered through the hook bus first
/// (`ticket.context_requested` / `chat.context_requested`) and falls
/// back to the caller-supplied `context_text`.
pub(crate) async fn build_reply_draft(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    body: web::Json<SupportDraftInput>,
    kind: DraftKind,
    use_case: &str,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, debug),
    };
    let _scope = match caller_company_uuid(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err(error, debug),
    };
    let input = body.into_inner();
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_AI_USE) {
        return err(error, debug);
    }
    let instruction = match require_text(&input.prompt, "prompt", MAX_PROMPT_LEN) {
        Ok(instruction) => instruction,
        Err(error) => return err(error, debug),
    };
    let (max_tokens, temperature) = match completion_options(input.max_tokens, input.temperature) {
        Ok(options) => options,
        Err(error) => return err(error, debug),
    };
    let mut context_parts: Vec<String> = Vec::new();
    if let Some(ticket_id) = &input.ticket_id {
        if let Some(transcript) = hook_transcript(
            &ctx,
            EVENT_TICKET_CONTEXT_REQUESTED,
            json!({ "ticket_id": ticket_id }),
        ) {
            context_parts.push(format!("Ticket {ticket_id}:\n{transcript}"));
        }
    }
    if let Some(conversation_id) = &input.conversation_id {
        if let Some(transcript) = hook_transcript(
            &ctx,
            EVENT_CHAT_CONTEXT_REQUESTED,
            json!({ "conversation_id": conversation_id }),
        ) {
            context_parts.push(format!(
                "Chat conversation {conversation_id}:\n{transcript}"
            ));
        }
    }
    if let Some(context_text) = &input.context_text {
        if let Ok(text) = require_text(context_text, "context_text", MAX_CONTEXT_TEXT_LEN) {
            context_parts.push(text);
        }
    }
    if context_parts.is_empty() {
        return err(
            UnifiedError::bad_request(
                "Provide ticket_id, conversation_id, or context_text so the draft can be grounded in the conversation",
            ),
            debug,
        );
    }
    let site = input.site.clone().unwrap_or_default();
    let mut shared = assemble_shared_context(Some(&ctx.hooks), &instruction, &site, true);
    shared
        .grounding_articles
        .extend(personal_mcp_grounding_sources(
            &state,
            caller_user_id(&caller),
        ));
    let context_text = shared_context_prompt(&shared);
    let system_prompt = if context_text.is_empty() {
        Some(
            "You draft support replies. Use only the conversation context below; do not invent facts, prices, dates, or promises. Return only the reply text."
                .to_string(),
        )
    } else {
        Some(format!(
            "You draft support replies. Use only the conversation context below; do not invent facts, prices, dates, or promises. Return only the reply text.\n\nSite context:\n{context_text}"
        ))
    };
    let prompt = format!(
        "Instruction from staff: {instruction}\n\nConversation context:\n{}",
        context_parts.join("\n\n")
    );
    let call = CompletionCall {
        prompt: prompt.clone(),
        system_prompt,
        max_tokens,
        temperature,
    };
    let (target_type, target_id) = match (&input.ticket_id, &input.conversation_id) {
        (Some(ticket_id), _) => (Some("ticket".to_string()), Some(ticket_id.clone())),
        (None, Some(conversation_id)) => (
            Some("chat_conversation".to_string()),
            Some(conversation_id.clone()),
        ),
        _ => (None, None),
    };
    let (connection, auth) = match authorize_call(
        &ctx,
        &state,
        &req,
        &caller,
        input.provider_id,
        &prompt,
        max_tokens,
        use_case,
        input.use_personal_key,
    ) {
        Ok(authorization) => authorization,
        Err(error) => return err(error, debug),
    };
    match run_completion(
        &state,
        &ctx,
        &caller.actor,
        &connection,
        &call,
        use_case,
        &auth,
    )
    .await
    {
        Ok(outcome) => {
            let draft = store_draft(
                &state,
                &ctx,
                kind,
                None,
                None,
                outcome.text.clone(),
                Some(prompt),
                &outcome,
                &caller.actor,
                target_type,
                target_id,
                shared.grounding_articles,
            );
            created(draft)
        }
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn support_draft(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    body: web::Json<SupportDraftInput>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, ctx.is_debug()),
    };
    let _scope = match caller_company_uuid(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err(error, ctx.is_debug()),
    };
    build_reply_draft(
        ctx,
        state,
        req,
        body,
        DraftKind::SupportReply,
        "support_reply",
    )
    .await
}

pub(crate) async fn chat_draft(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    body: web::Json<SupportDraftInput>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, ctx.is_debug()),
    };
    let _scope = match caller_company_uuid(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err(error, ctx.is_debug()),
    };
    build_reply_draft(ctx, state, req, body, DraftKind::ChatReply, "chat_reply").await
}

pub(crate) async fn content_draft(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    body: web::Json<ContentDraftInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, debug),
    };
    let _scope = match caller_company_uuid(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err(error, debug),
    };
    let input = body.into_inner();
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_AI_USE) {
        return err(error, debug);
    }
    let content_type = match input.content_type.as_str() {
        "post" | "page" => input.content_type.clone(),
        other => {
            return err(
                UnifiedError::bad_request(format!(
                    "content_type must be \"post\" or \"page\", got \"{other}\""
                )),
                debug,
            );
        }
    };
    let prompt = match require_text(&input.prompt, "prompt", MAX_PROMPT_LEN) {
        Ok(prompt) => prompt,
        Err(error) => return err(error, debug),
    };
    let title = match optional_text(&input.title, "title", 200) {
        Ok(title) => title,
        Err(error) => return err(error, debug),
    };
    let (max_tokens, temperature) = match completion_options(input.max_tokens, input.temperature) {
        Ok(options) => options,
        Err(error) => return err(error, debug),
    };
    let site = input.site.clone().unwrap_or_default();
    let mut shared = assemble_shared_context(Some(&ctx.hooks), &prompt, &site, true);
    shared
        .grounding_articles
        .extend(personal_mcp_grounding_sources(
            &state,
            caller_user_id(&caller),
        ));
    let context_text = shared_context_prompt(&shared);
    let system_prompt = if context_text.is_empty() {
        Some(format!(
            "You draft {content_type} content. Return only the draft text, in Markdown. Do not publish anything."
        ))
    } else {
        Some(format!(
            "You draft {content_type} content. Return only the draft text, in Markdown. Do not publish anything.\n\nSite context:\n{context_text}"
        ))
    };
    let call = CompletionCall {
        prompt: prompt.clone(),
        system_prompt,
        max_tokens,
        temperature,
    };
    let (connection, auth) = match authorize_call(
        &ctx,
        &state,
        &req,
        &caller,
        input.provider_id,
        &prompt,
        max_tokens,
        "content",
        input.use_personal_key,
    ) {
        Ok(authorization) => authorization,
        Err(error) => return err(error, debug),
    };
    match run_completion(
        &state,
        &ctx,
        &caller.actor,
        &connection,
        &call,
        "content",
        &auth,
    )
    .await
    {
        Ok(outcome) => {
            let draft = store_draft(
                &state,
                &ctx,
                DraftKind::Content,
                title,
                None,
                outcome.text.clone(),
                Some(prompt),
                &outcome,
                &caller.actor,
                Some(content_type),
                None,
                shared.grounding_articles,
            );
            created(draft)
        }
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn email_draft(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    body: web::Json<EmailDraftInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, debug),
    };
    let _scope = match caller_company_uuid(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err(error, debug),
    };
    let input = body.into_inner();
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_AI_USE) {
        return err(error, debug);
    }
    let prompt = match require_text(&input.prompt, "prompt", MAX_PROMPT_LEN) {
        Ok(prompt) => prompt,
        Err(error) => return err(error, debug),
    };
    let audience = match optional_text(&input.audience, "audience", 500) {
        Ok(audience) => audience,
        Err(error) => return err(error, debug),
    };
    let (max_tokens, temperature) = match completion_options(input.max_tokens, input.temperature) {
        Ok(options) => options,
        Err(error) => return err(error, debug),
    };
    let site = input.site.clone().unwrap_or_default();
    let mut shared = assemble_shared_context(Some(&ctx.hooks), &prompt, &site, true);
    shared
        .grounding_articles
        .extend(personal_mcp_grounding_sources(
            &state,
            caller_user_id(&caller),
        ));
    let context_text = shared_context_prompt(&shared);
    let system_prompt = if context_text.is_empty() {
        Some(
            "You draft email copy. Start with a line \"Subject: <subject>\", then a blank line, then the email body. Return only that text. Do not send anything."
                .to_string(),
        )
    } else {
        Some(format!(
            "You draft email copy. Start with a line \"Subject: <subject>\", then a blank line, then the email body. Return only that text. Do not send anything.\n\nSite context:\n{context_text}"
        ))
    };
    let full_prompt = match &audience {
        Some(audience) => format!("Audience: {audience}\n\nBrief: {prompt}"),
        None => prompt.clone(),
    };
    let call = CompletionCall {
        prompt: full_prompt.clone(),
        system_prompt,
        max_tokens,
        temperature,
    };
    let (connection, auth) = match authorize_call(
        &ctx,
        &state,
        &req,
        &caller,
        input.provider_id,
        &full_prompt,
        max_tokens,
        "email",
        input.use_personal_key,
    ) {
        Ok(authorization) => authorization,
        Err(error) => return err(error, debug),
    };
    match run_completion(
        &state,
        &ctx,
        &caller.actor,
        &connection,
        &call,
        "email",
        &auth,
    )
    .await
    {
        Ok(outcome) => {
            // Split a leading "Subject: ..." line off the body.
            let (subject, body_text) = match outcome.text.split_once('\n') {
                Some((first, rest))
                    if first.trim_start().to_lowercase().starts_with("subject:") =>
                {
                    let subject = first.trim_start()[8..].trim().to_string();
                    (
                        if subject.is_empty() {
                            None
                        } else {
                            Some(subject)
                        },
                        rest.trim_start_matches('\n').trim().to_string(),
                    )
                }
                _ => (None, outcome.text.clone()),
            };
            let (target_type, target_id) = match &input.list_id {
                Some(list_id) => (Some("newsletter".to_string()), Some(list_id.clone())),
                None => (Some("email".to_string()), None),
            };
            let draft = store_draft(
                &state,
                &ctx,
                DraftKind::Email,
                None,
                subject,
                body_text,
                Some(prompt),
                &outcome,
                &caller.actor,
                target_type,
                target_id,
                shared.grounding_articles,
            );
            created(draft)
        }
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn answer(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    body: web::Json<AnswerInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, debug),
    };
    let _scope = match caller_company_uuid(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err(error, debug),
    };
    let input = body.into_inner();
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_AI_USE) {
        return err(error, debug);
    }
    let question = match require_text(&input.question, "question", MAX_PROMPT_LEN) {
        Ok(question) => question,
        Err(error) => return err(error, debug),
    };
    let (max_tokens, temperature) = match completion_options(input.max_tokens, input.temperature) {
        Ok(options) => options,
        Err(error) => return err(error, debug),
    };
    let site = input.site.clone().unwrap_or_default();
    // Grounding uses the knowledgebase module's real search path
    // (`knowledgebase.search`, published articles only) through the
    // shared-context assembler.
    let mut shared = assemble_shared_context(Some(&ctx.hooks), &question, &site, true);
    shared
        .grounding_articles
        .extend(personal_mcp_grounding_sources(
            &state,
            caller_user_id(&caller),
        ));
    let grounded = !shared.grounding_articles.is_empty();
    let context_text = shared_context_prompt(&shared);
    let system_prompt = if grounded {
        format!(
            "Answer the question using ONLY the knowledgebase articles below. Cite the article titles. If the articles do not cover the question, say so plainly instead of guessing.\n\n{context_text}"
        )
    } else {
        "No knowledgebase articles matched the question. Say plainly that no grounded answer is available and suggest asking a human agent. Do not invent an answer.".to_string()
    };
    let call = CompletionCall {
        prompt: question.clone(),
        system_prompt: Some(system_prompt),
        max_tokens,
        temperature,
    };
    let (connection, auth) = match authorize_call(
        &ctx,
        &state,
        &req,
        &caller,
        input.provider_id,
        &question,
        max_tokens,
        "kb_answer",
        input.use_personal_key,
    ) {
        Ok(authorization) => authorization,
        Err(error) => return err(error, debug),
    };
    match run_completion(
        &state,
        &ctx,
        &caller.actor,
        &connection,
        &call,
        "kb_answer",
        &auth,
    )
    .await
    {
        Ok(outcome) => {
            let draft = store_draft(
                &state,
                &ctx,
                DraftKind::KbAnswer,
                None,
                None,
                outcome.text.clone(),
                Some(question),
                &outcome,
                &caller.actor,
                None,
                None,
                shared.grounding_articles,
            );
            created(draft)
        }
        Err(error) => err(error, debug),
    }
}

// Draft review handlers.
//
// Approval records a human decision only. There is deliberately no
// endpoint here that sends, publishes, or delivers a draft: the
// owning module must do that through its own existing human endpoint
// after reading the approved draft.

/// Load a draft, enforcing ownership: the author may read it, and a
/// role holding `ai:approve` may read any draft for review.
pub(crate) fn load_draft_for(
    state: &AiState,
    ctx: &AppContext,
    caller: &Caller,
    id: Uuid,
) -> UnifiedResult<AiDraft> {
    let draft = state
        .drafts
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("AI draft not found: {id}")))?;
    let can_review = check_capability(ctx, &caller.role, PERM_AI_APPROVE);
    if draft.actor != caller.actor && !can_review {
        return Err(UnifiedError::forbidden("Draft belongs to a different user"));
    }
    Ok(draft)
}

pub(crate) async fn list_drafts(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company_uuid(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_USE)?;
        // Reviewers see every draft; everyone else sees only their own.
        let can_review = check_capability(&ctx, &caller.role, PERM_AI_APPROVE);
        let params = query.into_inner();
        let kind_filter = params.filters.get("kind").cloned();
        let status_filter = params.filters.get("status").cloned();
        let provider_filter = params.filters.get("provider_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<AiDraft> = state
            .drafts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|draft| {
                if !can_review && draft.actor != caller.actor {
                    return false;
                }
                if let Some(kind) = &kind_filter {
                    if draft.kind.as_str() != kind {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if draft.status.as_str() != status {
                        return false;
                    }
                }
                if let Some(provider_id) = &provider_filter {
                    if draft.provider_id.map(|id| id.to_string()) != Some(provider_id.clone()) {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {} {}",
                        draft.title.clone().unwrap_or_default(),
                        draft.subject.clone().unwrap_or_default(),
                        draft.body
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(
            &["created_at", "updated_at", "kind", "status"],
            "created_at",
        );
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "updated_at" => a.updated_at.cmp(&b.updated_at),
                "kind" => a.kind.as_str().cmp(b.kind.as_str()),
                "status" => a.status.as_str().cmp(b.status.as_str()),
                _ => a.created_at.cmp(&b.created_at),
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

pub(crate) async fn get_draft(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AiDraft> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company_uuid(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_USE)?;
        let id = parse_uuid(&path.into_inner(), "draft")?;
        load_draft_for(&state, &ctx, &caller, id)
    })();
    match result {
        Ok(draft) => ok(draft),
        Err(error) => err(error, debug),
    }
}

/// Pure draft-review transition: the status check and the
/// separation-of-duties check for one review decision, without any
/// store access, so it is unit-testable on its own. Only a draft
/// can transition; approval by the draft's own author is refused
/// (separation of duties - a second human must review); rejection
/// by the author is allowed (withdrawing one's own draft).
/// Callers must run this while holding the draft's entry lock
/// (`state.drafts.get_mut`), so a concurrent approve + reject
/// cannot both pass the status check.
pub(crate) fn transition_draft_status(
    draft: &mut AiDraft,
    reviewer_actor: &str,
    target: DraftStatus,
) -> UnifiedResult<()> {
    if draft.status != DraftStatus::Draft {
        return Err(UnifiedError::conflict(format!(
            "Only a draft can be reviewed; this draft is {}",
            draft.status.as_str()
        )));
    }
    if target == DraftStatus::Approved && draft.actor == reviewer_actor {
        return Err(UnifiedError::forbidden(
            "A draft cannot be approved by its own author: a second reviewer must approve it",
        ));
    }
    draft.status = target;
    draft.reviewed_by = Some(reviewer_actor.to_string());
    draft.reviewed_at = Some(Utc::now());
    draft.updated_at = Utc::now();
    Ok(())
}

pub(crate) async fn approve_draft(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AiDraft> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company_uuid(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_APPROVE)?;
        let id = parse_uuid(&path.into_inner(), "draft")?;
        // Transition under the draft's entry lock: `get_mut`
        // holds the shard lock for this draft while the status
        // check and the write happen, so a concurrent reject
        // cannot interleave and last-write-win.
        let draft = {
            let mut entry = state
                .drafts
                .get_mut(&id)
                .ok_or_else(|| UnifiedError::not_found(format!("AI draft not found: {id}")))?;
            // A reviewer may review any draft; the author may not
            // approve their own (checked in the transition).
            transition_draft_status(&mut entry, &caller.actor, DraftStatus::Approved)?;
            (*entry).clone()
        };
        // Approval is a review decision, not a delivery: the payload
        // says so, and no send/publish hook is emitted here.
        emit_audit(
            &ctx,
            EVENT_AI_DRAFT_APPROVED,
            &caller.actor,
            &format!("ai-draft:{}", draft.id),
            json!({
                "id": draft.id.to_string(),
                "kind": draft.kind.as_str(),
                "status": "approved",
                "ai_generated": true,
                "requires_human_delivery": true,
                "target_type": draft.target_type,
                "target_id": draft.target_id,
            }),
        );
        Ok(draft)
    })();
    match result {
        Ok(draft) => ok(draft),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn reject_draft(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AiDraft> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company_uuid(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_APPROVE)?;
        let id = parse_uuid(&path.into_inner(), "draft")?;
        // Same entry-lock transition as approval: the status
        // check and the write happen atomically for this draft.
        let draft = {
            let mut entry = state
                .drafts
                .get_mut(&id)
                .ok_or_else(|| UnifiedError::not_found(format!("AI draft not found: {id}")))?;
            transition_draft_status(&mut entry, &caller.actor, DraftStatus::Rejected)?;
            (*entry).clone()
        };
        emit_audit(
            &ctx,
            EVENT_AI_DRAFT_REJECTED,
            &caller.actor,
            &format!("ai-draft:{}", draft.id),
            json!({
                "id": draft.id.to_string(),
                "kind": draft.kind.as_str(),
                "status": "rejected",
                "ai_generated": true,
                "target_type": draft.target_type,
                "target_id": draft.target_id,
            }),
        );
        Ok(draft)
    })();
    match result {
        Ok(draft) => ok(draft),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_draft(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company_uuid(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_USE)?;
        let id = parse_uuid(&path.into_inner(), "draft")?;
        let _draft = load_draft_for(&state, &ctx, &caller, id)?;
        state.drafts.remove(&id);
        Ok(json!({ "deleted": true, "draft_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

#[cfg(test)]
mod tests {
    use super::transition_draft_status;
    use crate::modules::ai::AiDraft;
    use crate::modules::ai::DraftKind;
    use crate::modules::ai::DraftStatus;
    use chrono::Utc;
    use uuid::Uuid;

    fn test_draft(actor: &str) -> AiDraft {
        let now = Utc::now();
        AiDraft {
            id: Uuid::now_v7(),
            kind: DraftKind::Generic,
            title: None,
            subject: None,
            body: "draft body".to_string(),
            prompt: None,
            provider_id: None,
            provider_kind: "openai".to_string(),
            model: "gpt-4o-mini".to_string(),
            actor: actor.to_string(),
            status: DraftStatus::Draft,
            ai_generated: true,
            target_type: None,
            target_id: None,
            sources: Vec::new(),
            usage_id: None,
            reviewed_by: None,
            reviewed_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn author_cannot_approve_own_draft() {
        let mut draft = test_draft("author-1");
        let error = transition_draft_status(&mut draft, "author-1", DraftStatus::Approved)
            .expect_err("self-approval must be refused");
        assert_eq!(error.code, 403);
        assert_eq!(draft.status, DraftStatus::Draft);
    }

    #[test]
    fn another_reviewer_can_approve() {
        let mut draft = test_draft("author-1");
        transition_draft_status(&mut draft, "reviewer-2", DraftStatus::Approved)
            .expect("a second reviewer must be able to approve");
        assert_eq!(draft.status, DraftStatus::Approved);
        assert_eq!(draft.reviewed_by.as_deref(), Some("reviewer-2"));
        assert!(draft.reviewed_at.is_some());
    }

    #[test]
    fn author_may_reject_own_draft() {
        let mut draft = test_draft("author-1");
        transition_draft_status(&mut draft, "author-1", DraftStatus::Rejected)
            .expect("author may withdraw their own draft");
        assert_eq!(draft.status, DraftStatus::Rejected);
    }

    #[test]
    fn reviewed_draft_cannot_transition_again() {
        let mut draft = test_draft("author-1");
        transition_draft_status(&mut draft, "reviewer-2", DraftStatus::Approved).unwrap();
        let error = transition_draft_status(&mut draft, "reviewer-3", DraftStatus::Rejected)
            .expect_err("an approved draft must not transition again");
        assert_eq!(error.code, 409);
        assert_eq!(draft.status, DraftStatus::Approved);
    }
}
