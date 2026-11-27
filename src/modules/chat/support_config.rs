//! chat - support config.

use unified_core::error::{UnifiedResult};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{Utc};
use uuid::Uuid;
use super::{limits::*, types::*, hub::*, state::*, helpers::*, module::*, handlers_channels::*, handlers_messages::*, handlers_files::*, handlers_direct::*, handlers_presence::*, handlers_websocket::*, support_sessions::*, support_ratings::*, support_agents::*, support_canned::*, support_offline::*};



// Live support: widget config + bootstrap
pub(crate) async fn get_widget_config(
    state: web::Data<ChatState>,
    query: web::Query<WidgetConfigQuery>,
) -> HttpResponse {
    let query = query.into_inner();
    let key = query.company_id.clone().unwrap_or_else(|| "default".to_string());
    let config = state
        .widget_configs
        .get(&key)
        .map(|entry| entry.value().clone())
        .unwrap_or_else(|| default_widget_config(query.company_id));
    ok(config)
}


pub(crate) async fn save_widget_config(
    state: web::Data<ChatState>,
    req: HttpRequest,
    body: web::Json<UpsertWidgetConfigRequest>,
) -> HttpResponse {
    let result: UnifiedResult<WidgetConfig> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let _actor = actor_id(&req)?;
        let body = body.into_inner();
        let company_id = require_company_match(
            &_tenant,
            &req,
            &clean_optional_text(&body.company_id, "company_id", MAX_DEPARTMENT_LEN)?,
        )?;
        let key = company_id.clone().unwrap_or_else(|| "default".to_string());
        let now = Utc::now();
        let mut config = state
            .widget_configs
            .get(&key)
            .map(|entry| entry.value().clone())
            .unwrap_or_else(|| default_widget_config(company_id.clone()));
        if let Some(name) = &body.name {
            config.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(greeting) = &body.greeting {
            config.greeting = require_text(greeting, "greeting", MAX_GREETING_LEN)?;
        }
        if let Some(offline_message) = &body.offline_message {
            config.offline_message =
                require_text(offline_message, "offline_message", MAX_GREETING_LEN)?;
        }
        if let Some(primary_color) = &body.primary_color {
            config.primary_color =
                require_text(primary_color, "primary_color", MAX_COLOR_LEN)?;
        }
        if let Some(departments) = body.departments {
            config.departments = clean_department_ids(Some(departments))?;
        }
        if let Some(enabled) = body.enabled {
            config.enabled = enabled;
        }
        config.company_id = company_id;
        config.updated_at = now;
        state.widget_configs.insert(key, config.clone());
        Ok(config)
    })();
    match result {
        Ok(config) => ok(config),
        Err(error) => err(error),
    }
}


/// Visitor bootstrap for the embeddable website widget. No account is
/// required: a visitor id is generated when the widget does not supply
/// one. Creates the backing private channel and the session, routes it
/// to an agent when one is free, and reports the queue position.
pub(crate) async fn bootstrap_widget_session(
    state: web::Data<ChatState>,
    body: web::Json<CreateSupportSessionRequest>,
) -> HttpResponse {
    let result: UnifiedResult<WidgetBootstrap> = (|| {
        let body = body.into_inner();
        let visitor_id = match &body.visitor_id {
            Some(raw) => require_text(raw, "visitor_id", MAX_DEPARTMENT_LEN)?,
            None => format!("visitor:{}", Uuid::now_v7()),
        };
        let visitor_name =
            clean_optional_text(&body.visitor_name, "visitor_name", MAX_VISITOR_NAME_LEN)?;
        let visitor_email = match &body.visitor_email {
            Some(raw) => Some(require_email(raw)?),
            None => None,
        };
        let department_id =
            clean_optional_text(&body.department_id, "department_id", MAX_DEPARTMENT_LEN)?;
        let subject = match &body.subject {
            Some(raw) => require_text(raw, "subject", MAX_SUBJECT_LEN)?,
            None => "Website chat".to_string(),
        };
        let company_id = clean_optional_text(&body.company_id, "company_id", MAX_DEPARTMENT_LEN)?;
        let session = create_support_session_inner(
            &state,
            visitor_id,
            visitor_name,
            visitor_email,
            department_id,
            subject,
            company_id,
        )?;
        Ok(WidgetBootstrap {
            channel_id: session.channel_id,
            queue_position: session.queue_position,
            agents_available: agents_available(&state, session.department_id.as_deref()),
            session,
        })
    })();
    match result {
        Ok(bootstrap) => created(bootstrap),
        Err(error) => err(error),
    }
}


/// Visitor polling: the widget re-reads its session (status, assigned
/// agent, queue position) with the unguessable session id. No account
/// is required.
pub(crate) async fn poll_widget_session(
    state: web::Data<ChatState>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<WidgetBootstrap> = (|| {
        refresh_queue_positions(&state);
        let session = load_session(&state, &path.into_inner())?;
        Ok(WidgetBootstrap {
            channel_id: session.channel_id,
            queue_position: session.queue_position,
            agents_available: agents_available(&state, session.department_id.as_deref()),
            session,
        })
    })();
    match result {
        Ok(bootstrap) => ok(bootstrap),
        Err(error) => err(error),
    }
}



// Live support: queue
pub(crate) async fn list_queue(
    state: web::Data<ChatState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<SupportSession>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let _actor = actor_id(&req)?;
        refresh_queue_positions(&state);
        let params = query.into_inner();
        let department_filter = params.filters.get("department_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<SupportSession> = state
            .sessions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|session| session.status == SupportSessionStatus::Waiting)
            .filter(|session| in_company_scope(&session.company_id, &_scope))
            .filter(|session| {
                if let Some(department) = &department_filter {
                    if session.department_id.as_deref() != Some(department.as_str()) {
                        return false;
                    }
                }
                if !search.is_empty()
                    && !session.subject.to_lowercase().contains(&search)
                    && !session.visitor_id.to_lowercase().contains(&search)
                {
                    return false;
                }
                true
            })
            .collect();
        // Queue order: oldest waiting visitor first.
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.id.cmp(&b.id)));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}
