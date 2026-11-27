// Dashboards - User dashboard pages and API handlers.

use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::json::{error_response, success_response};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::db::models::{License, StoreOrder};
use actix_web::{web, HttpRequest, HttpResponse};
use diesel::prelude::*;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use super::{CAP_APIKEYS_READ, CAP_LICENSES_READ, CAP_ORDERS_READ, CAP_RESELLER_ACCESS,
    CAP_SCHOOL_FEES_VIEW, CAP_RESELLER_CLIENTS_VIEW, CAP_RESELLER_PAYOUTS_VIEW, CAP_SERVICES_READ,
    CAP_TICKETS_READ, CAP_USER_ACCESS, DashboardsState, DownloadRow, OrderRow, db_pool,
    hook_section_entry, page_data, render_page_response, require_capability,
    reseller_overview_data, run_blocking, section_flags, user_overview_data};

pub(crate) async fn get_user_overview(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match require_capability(&req, &ctx, &state, CAP_USER_ACCESS).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    match user_overview_data(&ctx, &caller).await {
        Ok(data) => success_response(data, 200),
        Err(error) => error_response(&error, debug),
    }
}

pub(crate) async fn get_user_licenses(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match require_capability(&req, &ctx, &state, CAP_LICENSES_READ).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let params = query.into_inner();
    let pagination = params.normalized();
    let sort = params.allowed_sort(&["product", "status", "created_at"], "created_at");
    let descending = pagination.order == "desc";
    let status_filter = pagination.filters.get("status").cloned();
    let pool = match db_pool(&ctx) {
        Ok(pool) => pool,
        Err(error) => return error_response(&error, debug),
    };
    let user_id = caller.identity.user_id;
    let result = run_blocking(pool, move |connection| {
        use unified_core::db::schema::licenses::dsl as licenses_dsl;
        let mut count_query = licenses_dsl::licenses
            .filter(licenses_dsl::user_id.eq(user_id))
            .into_boxed();
        if let Some(status) = &status_filter {
            count_query = count_query.filter(licenses_dsl::status.eq(status.clone()));
        }
        let total = count_query
            .count()
            .get_result::<i64>(connection)
            .map_err(UnifiedError::from)?;
        let mut query = licenses_dsl::licenses
            .filter(licenses_dsl::user_id.eq(user_id))
            .into_boxed();
        if let Some(status) = &status_filter {
            query = query.filter(licenses_dsl::status.eq(status.clone()));
        }
        let items = match (sort.as_str(), descending) {
            ("product", false) => query
                .order(licenses_dsl::product.asc())
                .limit(pagination.per_page as i64)
                .offset(((pagination.page - 1) * pagination.per_page) as i64)
                .select(License::as_select())
                .load::<License>(connection),
            ("product", true) => query
                .order(licenses_dsl::product.desc())
                .limit(pagination.per_page as i64)
                .offset(((pagination.page - 1) * pagination.per_page) as i64)
                .select(License::as_select())
                .load::<License>(connection),
            ("status", false) => query
                .order(licenses_dsl::status.asc())
                .limit(pagination.per_page as i64)
                .offset(((pagination.page - 1) * pagination.per_page) as i64)
                .select(License::as_select())
                .load::<License>(connection),
            ("status", true) => query
                .order(licenses_dsl::status.desc())
                .limit(pagination.per_page as i64)
                .offset(((pagination.page - 1) * pagination.per_page) as i64)
                .select(License::as_select())
                .load::<License>(connection),
            (_, true) => query
                .order(licenses_dsl::created_at.asc())
                .limit(pagination.per_page as i64)
                .offset(((pagination.page - 1) * pagination.per_page) as i64)
                .select(License::as_select())
                .load::<License>(connection),
            (_, false) => query
                .order(licenses_dsl::created_at.desc())
                .limit(pagination.per_page as i64)
                .offset(((pagination.page - 1) * pagination.per_page) as i64)
                .select(License::as_select())
                .load::<License>(connection),
        }
        .map_err(UnifiedError::from)?;
        Ok((items, total as u64))
    })
    .await;
    match result {
        Ok((items, total)) => {
            success_response(PageResult::new(items, total, &pagination).to_json(), 200)
        }
        Err(error) => error_response(&error, debug),
    }
}

pub(crate) async fn get_user_orders(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match require_capability(&req, &ctx, &state, CAP_ORDERS_READ).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let params = query.into_inner();
    let pagination = params.normalized();
    let sort = params.allowed_sort(&["total", "status", "created_at"], "created_at");
    let descending = pagination.order == "desc";
    let status_filter = pagination.filters.get("status").cloned();
    let pool = match db_pool(&ctx) {
        Ok(pool) => pool,
        Err(error) => return error_response(&error, debug),
    };
    let user_id = caller.identity.user_id.to_string();
    let result = run_blocking(pool, move |connection| {
        use unified_core::db::schema::store_orders::dsl as orders_dsl;
        let mut count_query = orders_dsl::store_orders
            .filter(orders_dsl::user_id.eq(&user_id))
            .into_boxed();
        if let Some(status) = &status_filter {
            count_query = count_query.filter(orders_dsl::status.eq(status.clone()));
        }
        let total = count_query
            .count()
            .get_result::<i64>(connection)
            .map_err(UnifiedError::from)?;
        let mut query = orders_dsl::store_orders
            .filter(orders_dsl::user_id.eq(&user_id))
            .into_boxed();
        if let Some(status) = &status_filter {
            query = query.filter(orders_dsl::status.eq(status.clone()));
        }
        let items = match (sort.as_str(), descending) {
            ("total", false) => query
                .order(orders_dsl::total.asc())
                .limit(pagination.per_page as i64)
                .offset(((pagination.page - 1) * pagination.per_page) as i64)
                .select(StoreOrder::as_select())
                .load::<StoreOrder>(connection)
                .map(|rows| rows.into_iter().map(OrderRow::from).collect::<Vec<_>>()),
            ("total", true) => query
                .order(orders_dsl::total.desc())
                .limit(pagination.per_page as i64)
                .offset(((pagination.page - 1) * pagination.per_page) as i64)
                .select(StoreOrder::as_select())
                .load::<StoreOrder>(connection)
                .map(|rows| rows.into_iter().map(OrderRow::from).collect::<Vec<_>>()),
            ("status", false) => query
                .order(orders_dsl::status.asc())
                .limit(pagination.per_page as i64)
                .offset(((pagination.page - 1) * pagination.per_page) as i64)
                .select(StoreOrder::as_select())
                .load::<StoreOrder>(connection)
                .map(|rows| rows.into_iter().map(OrderRow::from).collect::<Vec<_>>()),
            ("status", true) => query
                .order(orders_dsl::status.desc())
                .limit(pagination.per_page as i64)
                .offset(((pagination.page - 1) * pagination.per_page) as i64)
                .select(StoreOrder::as_select())
                .load::<StoreOrder>(connection)
                .map(|rows| rows.into_iter().map(OrderRow::from).collect::<Vec<_>>()),
            (_, true) => query
                .order(orders_dsl::created_at.asc())
                .limit(pagination.per_page as i64)
                .offset(((pagination.page - 1) * pagination.per_page) as i64)
                .select(StoreOrder::as_select())
                .load::<StoreOrder>(connection)
                .map(|rows| rows.into_iter().map(OrderRow::from).collect::<Vec<_>>()),
            (_, false) => query
                .order(orders_dsl::created_at.desc())
                .limit(pagination.per_page as i64)
                .offset(((pagination.page - 1) * pagination.per_page) as i64)
                .select(StoreOrder::as_select())
                .load::<StoreOrder>(connection)
                .map(|rows| rows.into_iter().map(OrderRow::from).collect::<Vec<_>>()),
        }
        .map_err(UnifiedError::from)?;
        Ok((items, total as u64))
    })
    .await;
    match result {
        Ok((items, total)) => {
            success_response(PageResult::new(items, total, &pagination).to_json(), 200)
        }
        Err(error) => error_response(&error, debug),
    }
}

pub(crate) async fn get_user_downloads(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match require_capability(&req, &ctx, &state, CAP_LICENSES_READ).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let params = query.into_inner();
    let pagination = params.normalized();
    let pool = match db_pool(&ctx) {
        Ok(pool) => pool,
        Err(error) => return error_response(&error, debug),
    };
    let user_id = caller.identity.user_id;
    let result = run_blocking(pool, move |connection| {
        use unified_core::db::schema::licenses::dsl as licenses_dsl;
        use unified_core::db::schema::store_downloads::dsl as downloads_dsl;
        let license_ids: Vec<Uuid> = licenses_dsl::licenses
            .filter(licenses_dsl::user_id.eq(user_id))
            .select(licenses_dsl::id)
            .load::<Uuid>(connection)
            .map_err(UnifiedError::from)?;
        if license_ids.is_empty() {
            return Ok((Vec::new(), 0u64));
        }
        let total = downloads_dsl::store_downloads
            .filter(downloads_dsl::license_id.eq_any(&license_ids))
            .count()
            .get_result::<i64>(connection)
            .map_err(UnifiedError::from)?;
        let items = downloads_dsl::store_downloads
            .filter(downloads_dsl::license_id.eq_any(&license_ids))
            .order(downloads_dsl::downloaded_at.desc())
            .limit(pagination.per_page as i64)
            .offset(((pagination.page - 1) * pagination.per_page) as i64)
            .select(DownloadRow::as_select())
            .load::<DownloadRow>(connection)
            .map_err(UnifiedError::from)?;
        Ok((items, total as u64))
    })
    .await;
    match result {
        Ok((items, total)) => {
            success_response(PageResult::new(items, total, &pagination).to_json(), 200)
        }
        Err(error) => error_response(&error, debug),
    }
}

pub(crate) async fn user_page(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let raw = path.into_inner();
    user_page_inner(req, ctx, state, &raw).await
}

pub(crate) async fn user_page_inner(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
    raw: &str,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let segments: Vec<&str> = raw.split('/').filter(|part| !part.is_empty()).collect();
    let (section, subsection) = match segments.as_slice() {
        [] => ("overview", None),
        [first] => (*first, None),
        ["reseller", second] => ("reseller", Some(*second)),
        [first, ..] => (*first, None),
    };
    let allowed = ["overview", "licenses", "downloads", "orders", "school-fees", "tickets", "services", "apikeys", "profile", "reseller"];
    if !allowed.contains(&section) {
        return error_response(
            &UnifiedError::not_found(format!("Dashboard section \"{section}\" does not exist")),
            debug,
        );
    }
    if section == "reseller" && !matches!(subsection, Some("clients") | Some("payouts")) {
        return error_response(
            &UnifiedError::not_found("Reseller section must be clients or payouts".to_string()),
            debug,
        );
    }
    let capability = match (section, subsection) {
        ("licenses", _) | ("downloads", _) => CAP_LICENSES_READ,
        ("orders", _) => CAP_ORDERS_READ,
        ("school-fees", _) => CAP_SCHOOL_FEES_VIEW,
        ("tickets", _) => CAP_TICKETS_READ,
        ("services", _) => CAP_SERVICES_READ,
        ("apikeys", _) => CAP_APIKEYS_READ,
        ("reseller", Some("clients")) => CAP_RESELLER_CLIENTS_VIEW,
        ("reseller", Some("payouts")) => CAP_RESELLER_PAYOUTS_VIEW,
        ("reseller", _) => CAP_RESELLER_ACCESS,
        _ => CAP_USER_ACCESS,
    };
    let caller = match require_capability(&req, &ctx, &state, capability).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let current_path = format!("/{}/{}", state.user_prefix(), raw.trim_matches('/'));
    let current_path = current_path.trim_end_matches('/').to_string();
    let current_path = if current_path.is_empty() {
        format!("/{}", state.user_prefix())
    } else {
        current_path
    };
    let scope = if section == "reseller" { "reseller" } else { "user" };
    let mut data = page_data(&ctx, &state, &caller, scope, &format!("Dashboard - {section}"), &current_path);
    let overview = match user_overview_data(&ctx, &caller).await {
        Ok(data) => data,
        Err(error) => return error_response(&error, debug),
    };
    let mut extra = json!({});
    if section == "reseller" {
        match reseller_overview_data(&ctx, &state, &caller).await {
            Ok(data) => extra = data,
            Err(error) => return error_response(&error, debug),
        }
    }
    if let Some(object) = data.as_object_mut() {
        object.insert("section".to_string(), Value::String(section.to_string()));
        object.insert("subsection".to_string(), subsection.map(|value| Value::String(value.to_string())).unwrap_or(Value::Null));
        object.insert("overview".to_string(), overview.clone());
        object.insert("reseller".to_string(), extra.clone());
        // Section flags (`section_licenses`, `section_profile`, ...)
        // let the template show the current section's panel instead
        // of repeating the overview on every section page. Reseller
        // subsections flag their own name (`reseller-clients`).
        let flag_section = match (section, subsection) {
            ("reseller", Some(sub)) => format!("reseller-{sub}"),
            _ => section.to_string(),
        };
        if let Value::Object(flags) = section_flags(&flag_section) {
            for (key, value) in flags {
                object.insert(key, value);
            }
        }
        // Hook-provided data for this section, when its owning module
        // answers; `null` renders the honest "not connected" state.
        // Reseller payouts read the commissions entry from the
        // reseller overview instead of the user overview.
        let section_data = if section == "reseller" && subsection == Some("payouts") {
            hook_section_entry(&extra, "commissions")
        } else {
            hook_section_entry(&overview, section)
        };
        object.insert("section_data".to_string(), section_data);
    }
    render_page_response(&req, &ctx, &state, "user-dashboard", "user-layout", data)
}

pub(crate) async fn user_page_root(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
) -> HttpResponse {
    user_page_inner(req, ctx, state, "").await
}
