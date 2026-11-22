// Notifications module.
//
// In-app notifications with read tracking and per-user channel
// preferences. Other modules request a notification through the
// `notification.requested` hook; email delivery is forwarded to the Mail
// Manager through the `mail.send_requested` hook. Interconnection happens
// only through the hook bus.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext, HookHandler};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub id: String,
    pub user_id: String,
    pub title: String,
    pub body: String,
    pub kind: String,
    pub read: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateNotification {
    pub user_id: String,
    pub title: String,
    pub body: String,
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationPreferences {
    pub user_id: String,
    /// Enabled channels: `in_app`, `email`.
    pub channels: Vec<String>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdatePreferences {
    pub channels: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReadAllRequest {
    pub user_id: String,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct NotificationsState {
    hooks: RwLock<Arc<HookBus>>,
    notifications: DashMap<String, Notification>,
    preferences: DashMap<String, NotificationPreferences>,
}

impl NotificationsState {
    fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            notifications: DashMap::new(),
            preferences: DashMap::new(),
        }
    }

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    fn create_notification(&self, input: CreateNotification) -> UnifiedResult<Notification> {
        if input.user_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("user_id must not be empty"));
        }
        if input.title.trim().is_empty() {
            return Err(UnifiedError::bad_request("Title must not be empty"));
        }
        let notification = Notification {
            id: new_id(),
            user_id: input.user_id.trim().to_string(),
            title: input.title.trim().to_string(),
            body: input.body,
            kind: input.kind.unwrap_or_else(|| "general".to_string()),
            read: false,
            created_at: now(),
        };
        self.notifications
            .insert(notification.id.clone(), notification.clone());
        Ok(notification)
    }

    fn preferences_for(&self, user_id: &str) -> NotificationPreferences {
        self.preferences
            .get(user_id)
            .map(|entry| entry.value().clone())
            .unwrap_or_else(|| NotificationPreferences {
                user_id: user_id.to_string(),
                channels: vec!["in_app".to_string()],
                updated_at: now(),
            })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

fn now() -> DateTime<Utc> {
    Utc::now()
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

fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

fn respond_created<T: Serialize>(result: UnifiedResult<T>) -> HttpResponse {
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn list_notifications(
    state: web::Data<Arc<NotificationsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<Notification> = state
        .notifications
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|notification| {
            matches_search(
                &format!("{} {}", notification.title, notification.body),
                &params,
            ) && params
                .filters
                .get("user_id")
                .is_none_or(|user_id| notification.user_id == *user_id)
                && params.filters.get("kind").is_none_or(|kind| notification.kind == *kind)
                && params.filters.get("read").is_none_or(|read| {
                    notification.read == (*read == "true" || *read == "1")
                })
        })
        .collect();
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_notification(
    state: web::Data<Arc<NotificationsState>>,
    body: web::Json<CreateNotification>,
) -> HttpResponse {
    let result = state.create_notification(body.into_inner());
    respond_created(result)
}

async fn get_notification(
    state: web::Data<Arc<NotificationsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Notification> = state
        .notifications
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Notification not found"));
    json::respond(result, false)
}

async fn delete_notification(
    state: web::Data<Arc<NotificationsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        state
            .notifications
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Notification not found"))
    })();
    json::respond(result, false)
}

async fn mark_read(
    state: web::Data<Arc<NotificationsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Notification> = (|| {
        let mut notification = state
            .notifications
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Notification not found"))?;
        notification.read = true;
        Ok(notification.clone())
    })();
    json::respond(result, false)
}

async fn mark_all_read(
    state: web::Data<Arc<NotificationsState>>,
    body: web::Json<ReadAllRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let user_id = body.into_inner().user_id;
        if user_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("user_id must not be empty"));
        }
        let mut marked = 0u64;
        for mut entry in state.notifications.iter_mut() {
            if entry.value().user_id == user_id && !entry.value().read {
                entry.value_mut().read = true;
                marked += 1;
            }
        }
        Ok(json!({ "user_id": user_id, "marked_read": marked }))
    })();
    json::respond(result, false)
}

async fn get_preferences(
    state: web::Data<Arc<NotificationsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    // The user is selected with the `user_id` query parameter.
    let user_id = query
        .filters
        .get("user_id")
        .cloned()
        .unwrap_or_default();
    if user_id.trim().is_empty() {
        return json::error_response(
            &UnifiedError::bad_request("Query parameter user_id is required"),
            false,
        );
    }
    let preferences = state.preferences_for(&user_id);
    json::success_response(preferences, 200)
}

async fn update_preferences(
    state: web::Data<Arc<NotificationsState>>,
    query: web::Query<PaginationParams>,
    body: web::Json<UpdatePreferences>,
) -> HttpResponse {
    let result: UnifiedResult<NotificationPreferences> = (|| {
        let user_id = query
            .filters
            .get("user_id")
            .cloned()
            .unwrap_or_default();
        if user_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("Query parameter user_id is required"));
        }
        let channels = body.into_inner().channels;
        if channels.is_empty() {
            return Err(UnifiedError::bad_request(
                "At least one channel must be enabled",
            ));
        }
        for channel in &channels {
            if !matches!(channel.as_str(), "in_app" | "email") {
                return Err(UnifiedError::bad_request(
                    "Channels must be a subset of: in_app, email",
                ));
            }
        }
        let preferences = NotificationPreferences {
            user_id: user_id.clone(),
            channels,
            updated_at: now(),
        };
        state
            .preferences
            .insert(user_id, preferences.clone());
        Ok(preferences)
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Hook handlers
// ---------------------------------------------------------------------------

fn on_notification_requested(state: Arc<NotificationsState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let kind = data
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("general")
        .to_string();
    let input = CreateNotification {
        user_id: data
            .get("user_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        // Chat mentions carry no title/body, so synthesize readable ones
        // from the kind instead of storing an empty notification.
        title: data
            .get("title")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| match kind.as_str() {
                "chat.mention" => "You were mentioned in chat".to_string(),
                "ticket" => "Ticket update".to_string(),
                "invoice" => "Invoice update".to_string(),
                "license" => "License update".to_string(),
                other => format!("Notification: {other}"),
            }),
        body: data
            .get("body")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        kind: Some(kind),
    };
    match state.create_notification(input) {
        Ok(notification) => {
            let value = serde_json::to_value(&notification).unwrap_or(Value::Null);
            ctx.add_result(value);
            // Forward to the Mail Manager when the requester asked for email
            // and the user has the email channel enabled.
            let wants_email = data
                .get("channel")
                .and_then(|v| v.as_str())
                .is_some_and(|channel| channel == "email")
                || data
                    .get("channels")
                    .and_then(|v| v.as_array())
                    .is_some_and(|channels| {
                        channels.iter().any(|c| c.as_str() == Some("email"))
                    });
            let email = data.get("email").and_then(|v| v.as_str()).map(str::to_string);
            if wants_email && state.preferences_for(&notification.user_id).channels.iter().any(|c| c == "email") {
                if let Some(to) = email {
                    state.emit(
                        "mail.send_requested",
                        json!({
                            "to": to,
                            "subject": notification.title,
                            "body": notification.body,
                            "notification_id": notification.id,
                        }),
                    );
                }
            }
        }
        Err(error) => {
            ctx.add_result(json!({ "error": error.message }));
        }
    }
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct NotificationsModule {
    state: Arc<NotificationsState>,
}

impl NotificationsModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(NotificationsState::new()),
        }
    }
}

impl Default for NotificationsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for NotificationsModule {
    fn id(&self) -> &'static str {
        "notifications"
    }

    fn name(&self) -> &'static str {
        "Notifications"
    }

    fn description(&self) -> &'static str {
        "Notifications with read tracking and per-user channel preferences."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["mail"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_notification_requested(state.clone(), ctx));
        bus.register("notification.requested", "notifications", handler);
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/notifications")
                .route("", web::get().to(list_notifications))
                .route("", web::post().to(create_notification))
                .route("/preferences", web::get().to(get_preferences))
                .route("/preferences", web::put().to(update_preferences))
                .route("/read-all", web::post().to(mark_all_read))
                .route("/{id}", web::get().to(get_notification))
                .route("/{id}", web::delete().to(delete_notification))
                .route("/{id}/read", web::post().to(mark_read)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(NotificationsModule::new()));
}
