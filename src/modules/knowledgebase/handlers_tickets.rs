//! Knowledgebase - HTTP handlers for ticket links.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::ai;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::{constants::*, types::*, state::*, helpers::*, module::*, handlers_articles::*, handlers_categories::*};

// Ticket link handlers

pub(crate) async fn link_ticket(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<KnowledgebaseState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<LinkTicketRequest>,
) -> HttpResponse {
    let result: UnifiedResult<TicketLink> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_KNOWLEDGEBASE_MANAGE)?;
        let id = parse_uuid(&path.into_inner(), "article")?;
        require_article_in_scope(&state, id, &_scope)?;
        let ticket_id = require_text(&body.ticket_id, "ticket_id", MAX_TICKET_ID_LEN)?;
        let link = TicketLink {
            article_id: id,
            ticket_id: ticket_id.clone(),
            linked_at: Utc::now(),
        };
        match state.ticket_links.get_mut(&id) {
            Some(mut links) => {
                if links.iter().any(|row| row.ticket_id == ticket_id) {
                    return Err(UnifiedError::conflict(format!(
                        "Article {id} is already linked to ticket {ticket_id}"
                    )));
                }
                links.push(link.clone());
            }
            None => {
                state.ticket_links.insert(id, vec![link.clone()]);
            }
        }
        state.emit(
            "knowledgebase.article.ticket_linked",
            json!({
                "article_id": id.to_string(),
                "ticket_id": ticket_id,
                "linked_by": caller.actor,
            }),
        );
        Ok(link)
    })();
    match result {
        Ok(link) => created(link),
        Err(error) => err(error),
    }
}

pub(crate) async fn list_article_tickets(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<KnowledgebaseState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<TicketLink>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_KNOWLEDGEBASE_VIEW)?;
        let id = parse_uuid(&path.into_inner(), "article")?;
        require_article_in_scope(&state, id, &_scope)?;
        let params = query.into_inner();
        let mut links: Vec<TicketLink> = state
            .ticket_links
            .get(&id)
            .map(|entry| entry.value().clone())
            .unwrap_or_default();
        links.sort_by(|a, b| b.linked_at.cmp(&a.linked_at));
        Ok(paginate(links, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}

pub(crate) async fn unlink_ticket(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<KnowledgebaseState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_KNOWLEDGEBASE_MANAGE)?;
        let (raw_id, ticket_id) = path.into_inner();
        let id = parse_uuid(&raw_id, "article")?;
        require_article_in_scope(&state, id, &_scope)?;
        let mut removed = false;
        if let Some(mut links) = state.ticket_links.get_mut(&id) {
            let before = links.len();
            links.retain(|row| row.ticket_id != ticket_id);
            removed = links.len() < before;
        }
        if !removed {
            return Err(UnifiedError::not_found(format!(
                "Ticket {ticket_id} is not linked to article {id}"
            )));
        }
        Ok(json!({ "deleted": true, "article_id": id.to_string(), "ticket_id": ticket_id }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error),
    }
}
