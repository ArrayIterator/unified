// webinars -- Webinar handlers.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use uuid::Uuid;

use super::helpers::*;
use super::scheduler::*;
use super::state::*;
use super::types::*;

// ---------------------------------------------------------------------------
// Webinar handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_webinars(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    if let Err(error) = require_tenant(&req) {
        return err(error);
    }
    if let Err(error) = actor_id(&req) {
        return err(error);
    }
    let params = query.into_inner();
    let status_filter = params.filters.get("status").cloned();
    let host_filter = params.filters.get("host_id").cloned();
    let model_filter = params.filters.get("access_model").cloned();
    let from_filter = params.filters.get("from").cloned();
    let to_filter = params.filters.get("to").cloned();
    let search = params.search.clone().unwrap_or_default().to_lowercase();
    let mut items: Vec<Webinar> = state
        .webinars
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|webinar| {
            if let Some(status) = &status_filter {
                let expected = match status.as_str() {
                    "draft" => WebinarStatus::Draft,
                    "scheduled" => WebinarStatus::Scheduled,
                    "live" => WebinarStatus::Live,
                    "ended" => WebinarStatus::Ended,
                    "cancelled" => WebinarStatus::Cancelled,
                    _ => return false,
                };
                if webinar.status != expected {
                    return false;
                }
            }
            if let Some(host_id) = &host_filter {
                if &webinar.host_id != host_id {
                    return false;
                }
            }
            if let Some(model) = &model_filter {
                let expected = match model.as_str() {
                    "free" => WebinarAccessModel::Free,
                    "paid" => WebinarAccessModel::Paid,
                    _ => return false,
                };
                if webinar.access_model != expected {
                    return false;
                }
            }
            if let Some(from) = &from_filter {
                if let Ok(from) = from.parse::<DateTime<Utc>>() {
                    if webinar.starts_at.is_none_or(|start| start < from) {
                        return false;
                    }
                }
            }
            if let Some(to) = &to_filter {
                if let Ok(to) = to.parse::<DateTime<Utc>>() {
                    if webinar.starts_at.is_none_or(|start| start > to) {
                        return false;
                    }
                }
            }
            if !search.is_empty() {
                let haystack = format!(
                    "{} {}",
                    webinar.title,
                    webinar.description.clone().unwrap_or_default()
                )
                .to_lowercase();
                if !haystack.contains(&search) {
                    return false;
                }
            }
            true
        })
        .collect();
    let sort = params.allowed_sort(&["title", "starts_at", "created_at"], "created_at");
    let descending = params.normalized().order == "desc";
    items.sort_by(|a, b| {
        let ordering = match sort.as_str() {
            "title" => a.title.cmp(&b.title),
            "starts_at" => a.starts_at.cmp(&b.starts_at),
            _ => a.created_at.cmp(&b.created_at),
        };
        if descending {
            ordering.reverse()
        } else {
            ordering
        }
    });
    ok(paginate(items, &params))
}

pub(crate) async fn create_webinar(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    body: web::Json<CreateWebinarRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Webinar> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let input = body.into_inner();
        let config = state.config();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let description = match &input.description {
            Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
            None => None,
        };
        let co_host_ids = match &input.co_host_ids {
            Some(ids) => require_user_ids(ids, "co_host_ids")?,
            None => Vec::new(),
        };
        let panelist_ids = match &input.panelist_ids {
            Some(ids) => require_user_ids(ids, "panelist_ids")?,
            None => Vec::new(),
        };
        if co_host_ids.contains(&actor) || panelist_ids.contains(&actor) {
            return Err(UnifiedError::bad_request(
                "The host cannot also be listed as a co-host or panelist",
            ));
        }
        for id in &co_host_ids {
            if panelist_ids.contains(id) {
                return Err(UnifiedError::bad_request(
                    "A user cannot be both a co-host and a panelist",
                ));
            }
        }
        let access_model = input.access_model.unwrap_or_default();
        let product_id = match &input.product_id {
            Some(raw) => Some(require_text(raw, "product_id", MAX_PRODUCT_ID_LEN)?),
            None => None,
        };
        if access_model != WebinarAccessModel::Free && product_id.is_none() {
            return Err(UnifiedError::bad_request(
                "A paid webinar requires product_id",
            ));
        }
        if access_model == WebinarAccessModel::Free && product_id.is_some() {
            return Err(UnifiedError::bad_request(
                "A free webinar must not carry product_id; use access_model paid for a paid webinar",
            ));
        }
        let capacity = input.capacity.unwrap_or(config.default_capacity);
        validate_capacity(&config, capacity)?;
        let timezone = match &input.timezone {
            Some(raw) => require_text(raw, "timezone", MAX_TIMEZONE_LEN)?,
            None => config.default_timezone.clone(),
        };
        let recurrence_rule = match &input.recurrence_rule {
            Some(raw) => Some(require_text(raw, "recurrence_rule", MAX_RECURRENCE_LEN)?),
            None => None,
        };
        validate_calendar(
            input.starts_at,
            input.ends_at,
            &timezone,
            recurrence_rule.as_deref(),
        )?;
        let meeting_room_id = match &input.meeting_room_id {
            Some(raw) => Some(parse_uuid(raw, "meeting_room")?),
            None => None,
        };
        let chat_channel_id = match &input.chat_channel_id {
            Some(raw) => Some(require_text(raw, "chat_channel_id", MAX_CHANNEL_ID_LEN)?),
            None => None,
        };
        let survey_id = match &input.survey_id {
            Some(raw) => Some(require_text(raw, "survey_id", MAX_PRODUCT_ID_LEN)?),
            None => None,
        };
        let survey_distribution_id = match &input.survey_distribution_id {
            Some(raw) => Some(require_text(
                raw,
                "survey_distribution_id",
                MAX_PRODUCT_ID_LEN,
            )?),
            None => None,
        };
        let now = Utc::now();
        // A future start stays scheduled (draft when no start is
        // set yet); an instant webinar (no future start) is live
        // immediately, like an instant meeting room.
        let (status, started_at) = match input.starts_at {
            Some(start) if start > now => (WebinarStatus::Scheduled, None),
            Some(_) => (WebinarStatus::Live, Some(now)),
            None => (WebinarStatus::Draft, None),
        };
        let mut webinar = Webinar {
            id: Uuid::now_v7(),
            title,
            description,
            host_id: actor.clone(),
            co_host_ids,
            panelist_ids,
            status,
            access_model,
            product_id,
            capacity,
            requires_approval: input
                .requires_approval
                .unwrap_or(config.default_requires_approval),
            allow_waitlist: input
                .allow_waitlist
                .unwrap_or(config.default_allow_waitlist),
            chat_enabled: input.chat_enabled.unwrap_or(config.chat_enabled_default),
            qa_enabled: input.qa_enabled.unwrap_or(config.qa_enabled_default),
            polls_enabled: input.polls_enabled.unwrap_or(config.polls_enabled_default),
            recording_enabled: input
                .recording_enabled
                .unwrap_or(config.recording_enabled_default),
            meeting_room_id,
            meeting_link_status: if meeting_room_id.is_some() {
                "linked".to_string()
            } else {
                "unlinked".to_string()
            },
            chat_channel_id,
            survey_id,
            survey_distribution_id,
            starts_at: input.starts_at,
            ends_at: input.ends_at,
            timezone,
            recurrence_rule,
            scheduler_job_id: None,
            scheduler_status: "not_scheduled".to_string(),
            reminder_scheduler_status: "not_scheduled".to_string(),
            started_at,
            ended_at: None,
            follow_up_sent_at: None,
            follow_up_count: 0,
            created_at: now,
            updated_at: now,
        };
        if webinar.status == WebinarStatus::Scheduled {
            schedule_webinar(&state, &mut webinar);
        }
        state.webinars.insert(webinar.id, webinar.clone());
        state.emit(
            "webinar.created",
            json!({
                "webinar_id": webinar.id.to_string(),
                "host_id": actor,
                "status": status_name(status),
                "access_model": access_name(webinar.access_model),
                "product_id": webinar.product_id,
                "capacity": webinar.capacity,
                "requires_approval": webinar.requires_approval,
                "allow_waitlist": webinar.allow_waitlist,
                "meeting_room_id": webinar.meeting_room_id.map(|id| id.to_string()),
                "starts_at": webinar.starts_at,
                "ends_at": webinar.ends_at,
                "timezone": webinar.timezone,
                "recurrence_rule": webinar.recurrence_rule,
                "scheduler_job_id": webinar.scheduler_job_id,
                "scheduler_status": webinar.scheduler_status,
                "reminder_scheduler_status": webinar.reminder_scheduler_status,
            }),
        );
        Ok(webinar)
    })();
    match result {
        Ok(webinar) => created(webinar),
        Err(error) => err(error),
    }
}

pub(crate) async fn get_webinar(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    if let Err(error) = require_tenant(&req) {
        return err(error);
    }
    if let Err(error) = actor_id(&req) {
        return err(error);
    }
    match load_webinar(&state, &path.into_inner()) {
        Ok(webinar) => ok(webinar),
        Err(error) => err(error),
    }
}

pub(crate) async fn update_webinar(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateWebinarRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Webinar> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let input = body.into_inner();
        let mut webinar = load_webinar(&state, &path.into_inner())?;
        if !can_manage(&webinar, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host, a co-host, or an admin can update the webinar",
            ));
        }
        if webinar.status == WebinarStatus::Ended || webinar.status == WebinarStatus::Cancelled {
            return Err(UnifiedError::unprocessable(
                "An ended or cancelled webinar cannot be updated",
            ));
        }
        // While live, only the description and the live feature
        // flags may change; scheduling, capacity, and pricing are
        // frozen once the session has started.
        let live = webinar.status == WebinarStatus::Live;
        if let Some(title) = &input.title {
            if live {
                return Err(UnifiedError::unprocessable(
                    "The title cannot change while the webinar is live",
                ));
            }
            webinar.title = require_text(title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(description) = &input.description {
            webinar.description = match description {
                Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(ids) = &input.co_host_ids {
            if live {
                return Err(UnifiedError::unprocessable(
                    "Co-hosts cannot change while the webinar is live",
                ));
            }
            let cleaned = require_user_ids(ids, "co_host_ids")?;
            if cleaned.contains(&webinar.host_id) {
                return Err(UnifiedError::bad_request(
                    "The host cannot also be listed as a co-host",
                ));
            }
            webinar.co_host_ids = cleaned;
        }
        if let Some(ids) = &input.panelist_ids {
            if live {
                return Err(UnifiedError::unprocessable(
                    "Panelists cannot change while the webinar is live",
                ));
            }
            let cleaned = require_user_ids(ids, "panelist_ids")?;
            if cleaned.contains(&webinar.host_id)
                || webinar.co_host_ids.iter().any(|id| cleaned.contains(id))
            {
                return Err(UnifiedError::bad_request(
                    "A user cannot hold two presenter roles at once",
                ));
            }
            webinar.panelist_ids = cleaned;
        }
        if let Some(model) = input.access_model {
            if live {
                return Err(UnifiedError::unprocessable(
                    "The access model cannot change while the webinar is live",
                ));
            }
            webinar.access_model = model;
        }
        if let Some(product_id) = &input.product_id {
            if live {
                return Err(UnifiedError::unprocessable(
                    "The product cannot change while the webinar is live",
                ));
            }
            webinar.product_id = match product_id {
                Some(raw) => Some(require_text(raw, "product_id", MAX_PRODUCT_ID_LEN)?),
                None => None,
            };
        }
        if webinar.access_model != WebinarAccessModel::Free && webinar.product_id.is_none() {
            return Err(UnifiedError::bad_request(
                "A paid webinar requires product_id",
            ));
        }
        if webinar.access_model == WebinarAccessModel::Free && webinar.product_id.is_some() {
            return Err(UnifiedError::bad_request(
                "A free webinar must not carry product_id; use access_model paid for a paid webinar",
            ));
        }
        if let Some(capacity) = input.capacity {
            if live {
                return Err(UnifiedError::unprocessable(
                    "Capacity cannot change while the webinar is live",
                ));
            }
            let config = state.config();
            validate_capacity(&config, capacity)?;
            if capacity < registered_count(&state, webinar.id) {
                return Err(UnifiedError::bad_request(
                    "Capacity cannot drop below the number of already registered seats",
                ));
            }
            webinar.capacity = capacity;
        }
        if let Some(value) = input.requires_approval {
            webinar.requires_approval = value;
        }
        if let Some(value) = input.allow_waitlist {
            webinar.allow_waitlist = value;
        }
        if let Some(value) = input.chat_enabled {
            webinar.chat_enabled = value;
        }
        if let Some(value) = input.qa_enabled {
            webinar.qa_enabled = value;
        }
        if let Some(value) = input.polls_enabled {
            webinar.polls_enabled = value;
        }
        if let Some(value) = input.recording_enabled {
            webinar.recording_enabled = value;
        }
        if let Some(room) = &input.meeting_room_id {
            if live {
                return Err(UnifiedError::unprocessable(
                    "The linked meetings room cannot change while the webinar is live",
                ));
            }
            webinar.meeting_room_id = match room {
                Some(raw) => Some(parse_uuid(raw, "meeting_room")?),
                None => None,
            };
            webinar.meeting_link_status = if webinar.meeting_room_id.is_some() {
                "linked".to_string()
            } else {
                "unlinked".to_string()
            };
        }
        if let Some(channel) = &input.chat_channel_id {
            webinar.chat_channel_id = match channel {
                Some(raw) => Some(require_text(raw, "chat_channel_id", MAX_CHANNEL_ID_LEN)?),
                None => None,
            };
        }
        if let Some(survey) = &input.survey_id {
            webinar.survey_id = match survey {
                Some(raw) => Some(require_text(raw, "survey_id", MAX_PRODUCT_ID_LEN)?),
                None => None,
            };
        }
        if let Some(distribution) = &input.survey_distribution_id {
            webinar.survey_distribution_id = match distribution {
                Some(raw) => Some(require_text(
                    raw,
                    "survey_distribution_id",
                    MAX_PRODUCT_ID_LEN,
                )?),
                None => None,
            };
        }
        let mut starts_changed = false;
        if let Some(starts_at) = input.starts_at {
            if live {
                return Err(UnifiedError::unprocessable(
                    "The start time cannot change while the webinar is live",
                ));
            }
            webinar.starts_at = Some(starts_at);
            starts_changed = true;
        }
        if let Some(ends_at) = input.ends_at {
            webinar.ends_at = Some(ends_at);
        }
        if let Some(timezone) = &input.timezone {
            if live {
                return Err(UnifiedError::unprocessable(
                    "The timezone cannot change while the webinar is live",
                ));
            }
            webinar.timezone = require_text(timezone, "timezone", MAX_TIMEZONE_LEN)?;
        }
        if let Some(rule) = &input.recurrence_rule {
            if live {
                return Err(UnifiedError::unprocessable(
                    "The recurrence rule cannot change while the webinar is live",
                ));
            }
            webinar.recurrence_rule = match rule {
                Some(raw) => Some(require_text(raw, "recurrence_rule", MAX_RECURRENCE_LEN)?),
                None => None,
            };
        }
        validate_calendar(
            webinar.starts_at,
            webinar.ends_at,
            &webinar.timezone,
            webinar.recurrence_rule.as_deref(),
        )?;
        if starts_changed && webinar.status == WebinarStatus::Scheduled {
            schedule_webinar(&state, &mut webinar);
        }
        webinar.updated_at = Utc::now();
        state.webinars.insert(webinar.id, webinar.clone());
        // A capacity increase can free seats for the waitlist.
        promote_waitlist(&state, &webinar);
        state.emit(
            "webinar.updated",
            json!({
                "webinar_id": webinar.id.to_string(),
                "status": status_name(webinar.status),
                "capacity": webinar.capacity,
                "starts_at": webinar.starts_at,
                "ends_at": webinar.ends_at,
                "timezone": webinar.timezone,
                "recurrence_rule": webinar.recurrence_rule,
                "scheduler_job_id": webinar.scheduler_job_id,
                "scheduler_status": webinar.scheduler_status,
            }),
        );
        Ok(webinar)
    })();
    match result {
        Ok(webinar) => ok(webinar),
        Err(error) => err(error),
    }
}

pub(crate) async fn delete_webinar(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let webinar = load_webinar(&state, &path.into_inner())?;
        if !is_host(&webinar, &actor) && !is_admin(&roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host or an admin can delete the webinar",
            ));
        }
        if webinar.status == WebinarStatus::Live {
            return Err(UnifiedError::unprocessable(
                "A live webinar cannot be deleted; end it first",
            ));
        }
        let id = webinar.id;
        state.webinars.remove(&id);
        // Typed sweeps: remove every row that belongs to this
        // webinar, one store at a time.
        let reg_keys: Vec<Uuid> = state
            .registrations
            .iter()
            .filter(|entry| entry.value().webinar_id == id)
            .map(|entry| *entry.key())
            .collect();
        for key in reg_keys {
            state.registrations.remove(&key);
        }
        let att_keys: Vec<Uuid> = state
            .attendance
            .iter()
            .filter(|entry| entry.value().webinar_id == id)
            .map(|entry| *entry.key())
            .collect();
        for key in att_keys {
            state.attendance.remove(&key);
        }
        let poll_keys: Vec<Uuid> = state
            .polls
            .iter()
            .filter(|entry| entry.value().webinar_id == id)
            .map(|entry| *entry.key())
            .collect();
        for key in &poll_keys {
            state.polls.remove(key);
        }
        let vote_keys: Vec<String> = state
            .votes
            .iter()
            .filter(|entry| entry.value().webinar_id == id)
            .map(|entry| entry.key().clone())
            .collect();
        for key in vote_keys {
            state.votes.remove(&key);
        }
        let q_keys: Vec<Uuid> = state
            .questions
            .iter()
            .filter(|entry| entry.value().webinar_id == id)
            .map(|entry| *entry.key())
            .collect();
        for key in q_keys {
            state.questions.remove(&key);
        }
        let rec_keys: Vec<Uuid> = state
            .recordings
            .iter()
            .filter(|entry| entry.value().webinar_id == id)
            .map(|entry| *entry.key())
            .collect();
        for key in rec_keys {
            state.recordings.remove(&key);
        }
        let mute_keys: Vec<String> = state
            .mutes
            .iter()
            .filter(|entry| entry.value().webinar_id == id)
            .map(|entry| entry.key().clone())
            .collect();
        for key in mute_keys {
            state.mutes.remove(&key);
        }
        state.emit(
            "webinar.deleted",
            json!({ "webinar_id": id.to_string(), "host_id": webinar.host_id }),
        );
        Ok(json!({ "deleted": true, "webinar_id": id.to_string() }))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error),
    }
}

pub(crate) async fn publish_webinar(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Webinar> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let mut webinar = load_webinar(&state, &path.into_inner())?;
        if !can_manage(&webinar, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host, a co-host, or an admin can publish the webinar",
            ));
        }
        if webinar.status != WebinarStatus::Draft {
            return Err(UnifiedError::unprocessable(
                "Only a draft webinar can be published",
            ));
        }
        let Some(starts_at) = webinar.starts_at else {
            return Err(UnifiedError::bad_request(
                "A webinar needs starts_at before it can be published",
            ));
        };
        if starts_at <= Utc::now() {
            return Err(UnifiedError::bad_request(
                "starts_at must be in the future for a scheduled webinar",
            ));
        }
        webinar.status = WebinarStatus::Scheduled;
        webinar.updated_at = Utc::now();
        schedule_webinar(&state, &mut webinar);
        state.webinars.insert(webinar.id, webinar.clone());
        state.emit(
            "webinar.published",
            json!({
                "webinar_id": webinar.id.to_string(),
                "host_id": webinar.host_id,
                "starts_at": webinar.starts_at,
                "scheduler_job_id": webinar.scheduler_job_id,
                "scheduler_status": webinar.scheduler_status,
                "reminder_scheduler_status": webinar.reminder_scheduler_status,
            }),
        );
        Ok(webinar)
    })();
    match result {
        Ok(webinar) => ok(webinar),
        Err(error) => err(error),
    }
}

pub(crate) async fn cancel_webinar(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Webinar> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let mut webinar = load_webinar(&state, &path.into_inner())?;
        if !can_manage(&webinar, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host, a co-host, or an admin can cancel the webinar",
            ));
        }
        if webinar.status != WebinarStatus::Draft && webinar.status != WebinarStatus::Scheduled {
            return Err(UnifiedError::unprocessable(
                "Only a draft or scheduled webinar can be cancelled; end a live webinar instead",
            ));
        }
        webinar.status = WebinarStatus::Cancelled;
        webinar.updated_at = Utc::now();
        state.webinars.insert(webinar.id, webinar.clone());
        for entry in state.registrations.iter() {
            let row = entry.value();
            if row.webinar_id == webinar.id && row.status == "registered" {
                state.notify(
                    &row.user_id,
                    &format!("Cancelled: {}", webinar.title),
                    "This webinar has been cancelled.",
                    Some(format!("/api/v1/webinars/{}", webinar.id)),
                );
            }
        }
        state.emit(
            "webinar.cancelled",
            json!({
                "webinar_id": webinar.id.to_string(),
                "host_id": webinar.host_id,
            }),
        );
        Ok(webinar)
    })();
    match result {
        Ok(webinar) => ok(webinar),
        Err(error) => err(error),
    }
}

pub(crate) async fn start_webinar(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Webinar> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let mut webinar = load_webinar(&state, &path.into_inner())?;
        if !can_manage(&webinar, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host, a co-host, or an admin can start the webinar",
            ));
        }
        // Only a scheduled webinar whose start time has arrived
        // can go live; publish enforces a future starts_at, and
        // this is the matching guard on the start path.
        let now = Utc::now();
        can_start_webinar(&webinar, now)?;
        webinar.status = WebinarStatus::Live;
        webinar.started_at = Some(now);
        webinar.updated_at = now;
        if webinar.meeting_room_id.is_some() {
            webinar.meeting_link_status = "live".to_string();
            // Ask the linked meetings room to go live as well. The
            // meetings module listens to `meeting.start_requested`
            // and takes its own room live; this webinar does not
            // manage the room's state directly.
            state.emit(
                "meeting.start_requested",
                json!({
                    "room_id": webinar.meeting_room_id.map(|id| id.to_string()),
                    "meeting_id": webinar.meeting_room_id.map(|id| id.to_string()),
                    "webinar_id": webinar.id.to_string(),
                }),
            );
        }
        state.webinars.insert(webinar.id, webinar.clone());
        for entry in state.registrations.iter() {
            let row = entry.value();
            if row.webinar_id == webinar.id && row.status == "registered" {
                state.notify(
                    &row.user_id,
                    &format!("Live now: {}", webinar.title),
                    "Your webinar has started.",
                    Some(format!("/api/v1/webinars/{}", webinar.id)),
                );
            }
        }
        state.emit(
            "webinar.started",
            json!({
                "webinar_id": webinar.id.to_string(),
                "host_id": webinar.host_id,
                "started_at": now.to_rfc3339(),
                "meeting_room_id": webinar.meeting_room_id.map(|id| id.to_string()),
                "meeting_link_status": webinar.meeting_link_status,
                "source": "manual",
            }),
        );
        Ok(webinar)
    })();
    match result {
        Ok(webinar) => ok(webinar),
        Err(error) => err(error),
    }
}

pub(crate) async fn end_webinar(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Webinar> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let mut webinar = load_webinar(&state, &path.into_inner())?;
        if !can_manage(&webinar, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host, a co-host, or an admin can end the webinar",
            ));
        }
        if webinar.status != WebinarStatus::Live {
            return Err(UnifiedError::unprocessable(
                "Only a live webinar can be ended",
            ));
        }
        let now = Utc::now();
        webinar.status = WebinarStatus::Ended;
        webinar.ended_at = Some(now);
        webinar.updated_at = now;
        if webinar.meeting_room_id.is_some() {
            webinar.meeting_link_status = "ended".to_string();
        }
        state.webinars.insert(webinar.id, webinar.clone());
        close_open_sessions(&state, webinar.id, now);
        state.emit(
            "webinar.ended",
            json!({
                "webinar_id": webinar.id.to_string(),
                "host_id": webinar.host_id,
                "started_at": webinar.started_at,
                "ended_at": now.to_rfc3339(),
                "meeting_room_id": webinar.meeting_room_id.map(|id| id.to_string()),
            }),
        );
        Ok(webinar)
    })();
    match result {
        Ok(webinar) => ok(webinar),
        Err(error) => err(error),
    }
}

pub(crate) async fn link_meeting(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<LinkMeetingRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Webinar> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let input = body.into_inner();
        let mut webinar = load_webinar(&state, &path.into_inner())?;
        if !can_manage(&webinar, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host, a co-host, or an admin can link a meetings room",
            ));
        }
        if webinar.status == WebinarStatus::Live {
            return Err(UnifiedError::unprocessable(
                "The linked meetings room cannot change while the webinar is live",
            ));
        }
        if webinar.status == WebinarStatus::Ended || webinar.status == WebinarStatus::Cancelled {
            return Err(UnifiedError::unprocessable(
                "An ended or cancelled webinar cannot link a meetings room",
            ));
        }
        let room_id = parse_uuid(&input.meeting_room_id, "meeting_room")?;
        webinar.meeting_room_id = Some(room_id);
        webinar.meeting_link_status = "linked".to_string();
        webinar.updated_at = Utc::now();
        state.webinars.insert(webinar.id, webinar.clone());
        state.emit(
            "webinar.meeting.linked",
            json!({
                "webinar_id": webinar.id.to_string(),
                "meeting_room_id": room_id.to_string(),
            }),
        );
        Ok(webinar)
    })();
    match result {
        Ok(webinar) => ok(webinar),
        Err(error) => err(error),
    }
}

pub(crate) async fn mute_webinar(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<MuteRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let input = body.into_inner();
        let webinar = load_webinar(&state, &path.into_inner())?;
        if !can_manage(&webinar, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host, a co-host, or an admin can change mute state",
            ));
        }
        if webinar.status != WebinarStatus::Live {
            return Err(UnifiedError::unprocessable(
                "Mute controls are only available while the webinar is live",
            ));
        }
        let audio_muted = input.audio_muted.unwrap_or(true);
        let video_muted = input.video_muted.unwrap_or(false);
        let now = Utc::now();
        let mut targets: Vec<String> = Vec::new();
        if input.mute_all.unwrap_or(false) {
            targets = state
                .attendance
                .iter()
                .filter(|entry| {
                    entry.value().webinar_id == webinar.id && entry.value().left_at.is_none()
                })
                .map(|entry| entry.value().user_id.clone())
                .collect();
            targets.sort();
            targets.dedup();
            if targets.is_empty() {
                return Err(UnifiedError::unprocessable(
                    "Nobody is currently in the live session to mute",
                ));
            }
        } else {
            let target = match &input.user_id {
                Some(raw) => require_text(raw, "user_id", 128)?,
                None => {
                    return Err(UnifiedError::bad_request(
                        "user_id is required unless mute_all is true",
                    ));
                }
            };
            if open_session(&state, webinar.id, &target).is_none()
                && !is_staff(&webinar, &target, &[])
            {
                return Err(UnifiedError::not_found(format!(
                    "User {target} is not currently in this webinar"
                )));
            }
            targets.push(target);
        }
        for user_id in &targets {
            let key = format!("{}:{}", webinar.id, user_id);
            let record = WebinarMuteState {
                webinar_id: webinar.id,
                user_id: user_id.clone(),
                audio_muted,
                video_muted,
                updated_at: now,
            };
            state.mutes.insert(key, record);
        }
        state.emit(
            "webinar.mute_changed",
            json!({
                "webinar_id": webinar.id.to_string(),
                "changed_by": actor,
                "user_ids": targets,
                "mute_all": input.mute_all.unwrap_or(false),
                "audio_muted": audio_muted,
                "video_muted": video_muted,
            }),
        );
        Ok(json!({
            "webinar_id": webinar.id.to_string(),
            "user_ids": targets,
            "audio_muted": audio_muted,
            "video_muted": video_muted,
        }))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error),
    }
}

pub(crate) async fn get_live(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<LiveView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _actor = actor_id(&req)?;
        let webinar = load_webinar(&state, &path.into_inner())?;
        let attendee_count = state
            .attendance
            .iter()
            .filter(|entry| {
                entry.value().webinar_id == webinar.id && entry.value().left_at.is_none()
            })
            .count() as u64;
        let mute_states: Vec<WebinarMuteState> = state
            .mutes
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| row.webinar_id == webinar.id)
            .collect();
        let open_polls = state
            .polls
            .iter()
            .filter(|entry| {
                entry.value().webinar_id == webinar.id && entry.value().status == "open"
            })
            .count() as u64;
        let unanswered_questions = state
            .questions
            .iter()
            .filter(|entry| entry.value().webinar_id == webinar.id && !entry.value().answered)
            .count() as u64;
        Ok(LiveView {
            webinar_id: webinar.id,
            status: status_name(webinar.status).to_string(),
            meeting_room_id: webinar.meeting_room_id,
            meeting_link_status: webinar.meeting_link_status.clone(),
            started_at: webinar.started_at,
            attendee_count,
            registered_count: registered_count(&state, webinar.id) as u64,
            mute_states,
            open_polls,
            unanswered_questions,
        })
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error),
    }
}
