// Gamification — Season handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Season handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_seasons(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        let mut items: Vec<GamificationSeason> = state
            .seasons
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|season| in_company_scope(&season.company_id, &_scope))
            .filter(|season| {
                if let Some(status) = &status_filter {
                    if &season.status != status {
                        return false;
                    }
                }
                if let Some(company) = &company_filter {
                    if season.company_id.as_deref() != Some(company.as_str()) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|season| std::cmp::Reverse(season.starts_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_season(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    body: web::Json<CreateSeasonRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GamificationSeason> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        if input.ends_at <= input.starts_at {
            return Err(UnifiedError::bad_request("ends_at must be after starts_at"));
        }
        let company_id = require_company_match(
            &_tenant,
            &req,
            &match &input.company_id {
                Some(raw) => Some(require_text(raw, "company_id", MAX_NAME_LEN)?),
                None => None,
            },
        )?;
        let now = Utc::now();
        let season = GamificationSeason {
            id: Uuid::now_v7(),
            name,
            company_id,
            starts_at: input.starts_at,
            ends_at: input.ends_at,
            status: "upcoming".to_string(),
            created_at: now,
            updated_at: now,
        };
        state.seasons.insert(season.id, season.clone());
        state.emit(
            "gamification.season.created",
            json!({
                "season_id": season.id.to_string(),
                "name": season.name,
                "starts_at": season.starts_at.to_rfc3339(),
                "ends_at": season.ends_at.to_rfc3339(),
            }),
        );
        Ok(season)
    })();
    match result {
        Ok(season) => created(season),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_season(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GamificationSeason> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_VIEW)?;
        let season = load_season(&state, &path.into_inner())?;
        require_record_in_company_scope(&season.company_id, &_scope)?;
        require_record_in_company_scope(&season.company_id, &_scope)?;
        Ok(season)
    })();
    match result {
        Ok(season) => ok(season),
        Err(error) => err(error, debug),
    }
}

/// An active season's company is frozen: moving it would
/// re-scope points already earned under it and the season already
/// running for that company. Only an upcoming (or ended, via
/// re-creation) season may change company.
pub(crate) fn check_season_company_change(
    season: &GamificationSeason,
    new_company_id: &Option<Option<String>>,
) -> UnifiedResult<()> {
    if season.status == "active" {
        if let Some(new) = new_company_id {
            if *new != season.company_id {
                return Err(UnifiedError::conflict(
                    "An active season cannot change company_id",
                ));
            }
        }
    }
    Ok(())
}

/// A season can only start while now is inside its window
/// [starts_at, ends_at]. Starting early or late would create an
/// active season whose window already disagrees with the clock.
pub(crate) fn check_season_startable(
    season: &GamificationSeason,
    now: DateTime<Utc>,
) -> UnifiedResult<()> {
    if now < season.starts_at || now > season.ends_at {
        return Err(UnifiedError::conflict(format!(
            "Season cannot be started now: the current time is outside its window ({} to {})",
            season.starts_at.to_rfc3339(),
            season.ends_at.to_rfc3339(),
        )));
    }
    Ok(())
}

pub(crate) async fn update_season(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateSeasonRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GamificationSeason> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_MANAGE)?;
        let mut season = load_season(&state, &path.into_inner())?;
        require_record_in_company_scope(&season.company_id, &_scope)?;
        if season.status == "ended" {
            return Err(UnifiedError::conflict("An ended season cannot be updated"));
        }
        let input = body.into_inner();
        check_season_company_change(&season, &input.company_id)?;
        if let Some(name) = &input.name {
            season.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(company_id) = &input.company_id {
            season.company_id = match company_id {
                Some(raw) => Some(require_text(raw, "company_id", MAX_NAME_LEN)?),
                None => None,
            };
        }
        if let Some(starts_at) = input.starts_at {
            season.starts_at = starts_at;
        }
        if let Some(ends_at) = input.ends_at {
            season.ends_at = ends_at;
        }
        if season.ends_at <= season.starts_at {
            return Err(UnifiedError::bad_request("ends_at must be after starts_at"));
        }
        season.updated_at = Utc::now();
        state.seasons.insert(season.id, season.clone());
        Ok(season)
    })();
    match result {
        Ok(season) => ok(season),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn start_season(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GamificationSeason> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_MANAGE)?;
        let mut season = load_season(&state, &path.into_inner())?;
        require_record_in_company_scope(&season.company_id, &_scope)?;
        if season.status != "upcoming" {
            return Err(UnifiedError::conflict(format!(
                "Only an upcoming season can be started (status is \"{}\")",
                season.status
            )));
        }
        let now = Utc::now();
        check_season_startable(&season, now)?;
        // One active season at a time per company: end any other
        // active season of the same company first.
        let others: Vec<GamificationSeason> = state
            .seasons
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|other| {
                other.id != season.id
                    && other.status == "active"
                    && other.company_id == season.company_id
            })
            .collect();
        for mut other in others {
            other.status = "ended".to_string();
            other.updated_at = now;
            state.seasons.insert(other.id, other.clone());
            state.emit(
                "gamification.season.ended",
                json!({
                    "season_id": other.id.to_string(),
                    "name": other.name,
                    "reason": "superseded",
                }),
            );
        }
        season.status = "active".to_string();
        season.updated_at = now;
        state.seasons.insert(season.id, season.clone());
        state.emit(
            "gamification.season.started",
            json!({
                "season_id": season.id.to_string(),
                "name": season.name,
                "starts_at": season.starts_at.to_rfc3339(),
                "ends_at": season.ends_at.to_rfc3339(),
            }),
        );
        Ok(season)
    })();
    match result {
        Ok(season) => ok(season),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn end_season(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GamificationSeason> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_MANAGE)?;
        let mut season = load_season(&state, &path.into_inner())?;
        require_record_in_company_scope(&season.company_id, &_scope)?;
        if season.status != "active" {
            return Err(UnifiedError::conflict(format!(
                "Only an active season can be ended (status is \"{}\")",
                season.status
            )));
        }
        season.status = "ended".to_string();
        season.updated_at = Utc::now();
        state.seasons.insert(season.id, season.clone());
        state.emit(
            "gamification.season.ended",
            json!({
                "season_id": season.id.to_string(),
                "name": season.name,
            }),
        );
        Ok(season)
    })();
    match result {
        Ok(season) => ok(season),
        Err(error) => err(error, debug),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn season(status: &str, company_id: Option<&str>, starts_in_days: i64, ends_in_days: i64) -> GamificationSeason {
        let now = Utc::now();
        GamificationSeason {
            id: Uuid::now_v7(),
            name: "Season".to_string(),
            company_id: company_id.map(str::to_string),
            starts_at: now + chrono::Duration::days(starts_in_days),
            ends_at: now + chrono::Duration::days(ends_in_days),
            status: status.to_string(),
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn active_season_cannot_change_company() {
        let season = season("active", Some("c1"), -1, 1);
        let error = check_season_company_change(&season, &Some(Some("c2".to_string())))
            .expect_err("active season company change must be rejected");
        assert_eq!(error.code, 409);
        // Clearing the company is a change too.
        assert!(check_season_company_change(&season, &Some(None)).is_err());
    }

    #[test]
    fn active_season_keeping_its_company_is_fine() {
        let season = season("active", Some("c1"), -1, 1);
        assert!(check_season_company_change(&season, &Some(Some("c1".to_string()))).is_ok());
        // Not providing company_id is not a change.
        assert!(check_season_company_change(&season, &None).is_ok());
    }

    #[test]
    fn upcoming_season_may_change_company() {
        let season = season("upcoming", Some("c1"), 1, 2);
        assert!(check_season_company_change(&season, &Some(Some("c2".to_string()))).is_ok());
    }

    #[test]
    fn season_cannot_start_before_its_window() {
        let season = season("upcoming", None, 1, 2);
        let error = check_season_startable(&season, Utc::now())
            .expect_err("starting before starts_at must be rejected");
        assert_eq!(error.code, 409);
    }

    #[test]
    fn season_cannot_start_after_its_window() {
        let season = season("upcoming", None, -2, -1);
        assert!(check_season_startable(&season, Utc::now()).is_err());
    }

    #[test]
    fn season_can_start_inside_its_window_inclusive() {
        let season = season("upcoming", None, -1, 1);
        assert!(check_season_startable(&season, Utc::now()).is_ok());
        assert!(check_season_startable(&season, season.starts_at).is_ok());
        assert!(check_season_startable(&season, season.ends_at).is_ok());
    }
}
