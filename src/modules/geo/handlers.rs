//! geo - handlers.

use unified_core::context::{AppContext, GeoInfo};
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::events;
use unified_core::json;
use unified_core::pagination::PaginationParams;
use unified_core::db::models::{GeoOverride, NewGeoOverride};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use diesel::prelude::*;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;
use super::{store::*, cidr::*, classification::*, lookup::*, db::*, types::*, module::*};



// ---------------------------------------------------------------------------
// Caller identity and capability checks (same shape as the exams and
// webhooks modules: `X-User-Id` + `X-Role` headers, capability asked
// through the `role.check` hook, failing closed when no handler
// answers). Every handler in this module calls `caller` +
// `require_permission` first, before touching any store or file.
// ---------------------------------------------------------------------------
pub(crate) const PERM_GEO_VIEW: &str = "geo.view";
pub(crate) const PERM_GEO_MANAGE: &str = "geo.manage";

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the exams module.
pub(crate) struct Caller {
    pub(crate) actor: String,
    pub(crate) role: String,
}

pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
pub(crate) fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub(crate) fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}



// ---------------------------------------------------------------------------
// Handlers.
// ---------------------------------------------------------------------------
pub(crate) async fn lookup_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<LookupQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GeoInfo> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GEO_VIEW)?;
        let ip = query
            .ip
            .as_deref()
            .ok_or_else(|| UnifiedError::bad_request("Query parameter \"ip\" is required"))?;
        let info = lookup_geo(&ctx, ip)?;
        ctx.set_geo(info.clone());
        ctx.hooks.emit(
            events::GEO_LOOKUP,
            serde_json::json!({
                "ip": info.ip,
                "country_code": info.country_code,
                "country_name": info.country_name,
                "city": info.city,
                "source": info.source,
            }),
        );
        Ok(info)
    })();
    json::respond(result, debug)
}


pub(crate) async fn list_overrides(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GEO_VIEW)?;
        let mut items: Vec<GeoOverride> = match db_pool(&ctx) {
            Some(pool) => db_load_overrides(&pool)?,
            None => overrides_store().iter().map(|entry| entry.value().clone()).collect(),
        };
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|row| {
                row.cidr.to_lowercase().contains(&needle)
                    || row.country_code.to_lowercase().contains(&needle)
                    || row
                        .country_name
                        .as_deref()
                        .unwrap_or_default()
                        .to_lowercase()
                        .contains(&needle)
            });
        }
        if let Some(active) = params.filters.get("is_active") {
            let wanted = active == "true" || active == "1";
            items.retain(|row| row.is_active == wanted);
        }
        let sort = params.allowed_sort(&["cidr", "country_code", "created_at"], "created_at");
        match sort.as_str() {
            "cidr" => items.sort_by(|a, b| a.cidr.cmp(&b.cidr)),
            "country_code" => items.sort_by(|a, b| a.country_code.cmp(&b.country_code)),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}


pub(crate) async fn create_override(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateOverrideRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GeoOverride> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GEO_MANAGE)?;
        let cidr = Cidr::parse(&body.cidr)?;
        let country_code = validate_country_code(&body.country_code)?;
        let asn = validate_asn(body.asn)?;
        let now = Utc::now();
        let row = GeoOverride {
            id: Uuid::now_v7(),
            cidr: cidr.to_cidr_string(),
            country_code,
            country_name: body.country_name.clone(),
            city: body.city.clone(),
            asn,
            asn_organization: body.asn_organization.clone(),
            is_active: true,
            created_at: now,
            updated_at: now,
        };
        if let Some(pool) = db_pool(&ctx) {
            use unified_core::db::schema::geo_overrides::dsl as geo_dsl;
            let mut conn = pool.get().map_err(|error| {
                UnifiedError::internal(format!("Cannot get database connection: {error}"))
            })?;
            let new_row = NewGeoOverride {
                id: row.id,
                cidr: row.cidr.clone(),
                country_code: row.country_code.clone(),
                country_name: row.country_name.clone(),
                city: row.city.clone(),
                asn: row.asn,
                asn_organization: row.asn_organization.clone(),
            };
            diesel::insert_into(geo_dsl::geo_overrides)
                .values(&new_row)
                .execute(&mut conn)
                .map_err(|error| {
                    UnifiedError::internal(format!("Cannot create Geo override: {error}"))
                })?;
        }
        overrides_store().insert(row.id, row.clone());
        Ok(row)
    })();
    match result {
        Ok(row) => json::success_response(row, 201),
        Err(error) => json::error_response(&error, debug),
    }
}


pub(crate) async fn get_override(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GeoOverride> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GEO_VIEW)?;
        let id = id.into_inner();
        if let Some(pool) = db_pool(&ctx) {
            use unified_core::db::schema::geo_overrides::dsl as geo_dsl;
            let mut conn = pool.get().map_err(|error| {
                UnifiedError::internal(format!("Cannot get database connection: {error}"))
            })?;
            let row = geo_dsl::geo_overrides
                .filter(geo_dsl::id.eq(id))
                .select(GeoOverride::as_select())
                .first(&mut conn)
                .optional()
                .map_err(|error| {
                    UnifiedError::internal(format!("Cannot load Geo override: {error}"))
                })?;
            if let Some(row) = row {
                return Ok(row);
            }
        }
        overrides_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Geo override not found"))
    })();
    json::respond(result, debug)
}


pub(crate) async fn update_override(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<UpdateOverrideRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GeoOverride> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GEO_MANAGE)?;
        let id = id.into_inner();
        let mut row = overrides_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Geo override not found"))?;
        if let Some(cidr_text) = &body.cidr {
            row.cidr = Cidr::parse(cidr_text)?.to_cidr_string();
        }
        if let Some(code) = &body.country_code {
            row.country_code = validate_country_code(code)?;
        }
        if let Some(value) = &body.country_name {
            row.country_name = value.clone();
        }
        if let Some(value) = &body.city {
            row.city = value.clone();
        }
        if let Some(value) = &body.asn {
            row.asn = validate_asn(*value)?;
        }
        if let Some(value) = &body.asn_organization {
            row.asn_organization = value.clone();
        }
        if let Some(active) = body.is_active {
            row.is_active = active;
        }
        row.updated_at = Utc::now();
        if let Some(pool) = db_pool(&ctx) {
            use unified_core::db::schema::geo_overrides::dsl as geo_dsl;
            #[derive(AsChangeset)]
            #[diesel(table_name = unified_core::db::schema::geo_overrides)]
            struct OverrideChanges {
                cidr: String,
                country_code: String,
                country_name: Option<String>,
                city: Option<String>,
                asn: Option<i32>,
                asn_organization: Option<String>,
                is_active: bool,
            }
            let mut conn = pool.get().map_err(|error| {
                UnifiedError::internal(format!("Cannot get database connection: {error}"))
            })?;
            diesel::update(geo_dsl::geo_overrides.filter(geo_dsl::id.eq(id)))
                .set(OverrideChanges {
                    cidr: row.cidr.clone(),
                    country_code: row.country_code.clone(),
                    country_name: row.country_name.clone(),
                    city: row.city.clone(),
                    asn: row.asn,
                    asn_organization: row.asn_organization.clone(),
                    is_active: row.is_active,
                })
                .execute(&mut conn)
                .map_err(|error| {
                    UnifiedError::internal(format!("Cannot update Geo override: {error}"))
                })?;
        }
        overrides_store().insert(id, row.clone());
        Ok(row)
    })();
    json::respond(result, debug)
}


pub(crate) async fn delete_override(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GEO_MANAGE)?;
        let id = id.into_inner();
        if let Some(pool) = db_pool(&ctx) {
            use unified_core::db::schema::geo_overrides::dsl as geo_dsl;
            let mut conn = pool.get().map_err(|error| {
                UnifiedError::internal(format!("Cannot get database connection: {error}"))
            })?;
            diesel::delete(geo_dsl::geo_overrides.filter(geo_dsl::id.eq(id)))
                .execute(&mut conn)
                .map_err(|error| {
                    UnifiedError::internal(format!("Cannot delete Geo override: {error}"))
                })?;
        }
        let removed = overrides_store().remove(&id).is_some();
        if removed {
            Ok(serde_json::json!({ "deleted": true, "id": id }))
        } else {
            Err(UnifiedError::not_found("Geo override not found"))
        }
    })();
    json::respond(result, debug)
}


pub(crate) async fn asn_check_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<AsnCheckRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GEO_VIEW)?;
        lookup_asn(&ctx, &body.ip)
    })();
    json::respond(result, debug)
}


pub(crate) async fn settings_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GeoSettingsView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GEO_VIEW)?;
        let config = ctx.config.read().expect("config lock poisoned");
        Ok(GeoSettingsView {
            database_path: config.geo.database_path.clone(),
            database_exists: std::path::Path::new(&config.geo.database_path).exists(),
            download_url: config.geo.download_url.clone(),
            auto_update: config.geo.auto_update,
            update_interval_hours: config.geo.update_interval_hours,
            settings_override: config.geo.settings_override,
            maxmind_key_configured: !config.licenses.maxmind.is_empty(),
            maxmind_key_valid: is_valid_maxmind_license_key(&config.licenses.maxmind),
            overrides_count: overrides_store().len(),
        })
    })();
    json::respond(result, debug)
}


pub(crate) async fn download_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: Option<web::Json<DownloadRequest>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let auth: UnifiedResult<()> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GEO_MANAGE)
    })();
    if let Err(error) = auth {
        return json::error_response(&error, debug);
    }
    let url = body.as_ref().and_then(|body| body.url.clone());
    let result = download_database(&ctx, url).await;
    json::respond(result, debug)
}


/// Host that a caller-supplied download URL must match when no custom
/// download URL is configured (the built-in MaxMind download host).
pub(crate) const MAXMIND_DOWNLOAD_HOST: &str = "download.maxmind.com";

/// Resolve the database download URL, enforcing the host allowlist on
/// caller-supplied overrides.
///
/// Resolution order: explicit override, configured `geo.download_url`,
/// built-in MaxMind URL (requires `licenses.maxmind`). An override is
/// only accepted when its host matches the host of the configured
/// download URL — or the built-in MaxMind host when nothing is
/// configured — so a caller cannot point the download at an internal
/// HTTP service. The host comparison is exact: a lookalike host such
/// as `download.maxmind.com.evil.example` is rejected.
pub(crate) fn resolve_download_url(
    url_override: Option<String>,
    configured_url: &str,
    maxmind_key: &str,
) -> UnifiedResult<String> {
    let allowed_host = url::Url::parse(configured_url.trim())
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_string))
        .unwrap_or_else(|| MAXMIND_DOWNLOAD_HOST.to_string());
    let url = match url_override {
        Some(url) if !url.trim().is_empty() => {
            let url = url.trim().to_string();
            let parsed = url::Url::parse(&url).map_err(|_| {
                UnifiedError::bad_request(format!("Invalid download URL \"{url}\""))
            })?;
            let host = parsed.host_str().unwrap_or_default().to_ascii_lowercase();
            if host != allowed_host.to_ascii_lowercase() {
                return Err(UnifiedError::bad_request(format!(
                    "Geo database download URL host \"{host}\" is not allowed; expected \"{allowed_host}\""
                )));
            }
            url
        }
        _ if !configured_url.trim().is_empty() => configured_url.trim().to_string(),
        _ if !maxmind_key.trim().is_empty() => format!(
            "https://download.maxmind.com/app/geoip_download?edition_id=GeoLite2-City&license_key={maxmind_key}&suffix=tar.gz"
        ),
        _ => {
            return Err(UnifiedError::bad_request(
                "No Geo database download URL configured and no MaxMind license key is set",
            ));
        }
    };
    let parsed = url::Url::parse(&url)
        .map_err(|_| UnifiedError::bad_request(format!("Invalid download URL \"{url}\"")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(UnifiedError::bad_request(
            "Geo database download URL must use http or https",
        ));
    }
    Ok(url)
}


/// Download the GeoLite database, extract the `.mmdb` file when the
/// download is a gzip tarball (the MaxMind format), save it to
/// `geo.database_path`, and validate it by opening it with maxminddb.
///
/// The download is written to a temporary file in the same directory
/// first and only renamed over the live database after validation
/// passes, so a failed or corrupt download never replaces the working
/// database. The temporary file is removed on any failure.
///
/// Note: an `.mmdb` file has no header magic bytes - validation is done by
/// parsing the file with the maxminddb reader and performing a test lookup.
pub(crate) async fn download_database(
    ctx: &AppContext,
    url_override: Option<String>,
) -> UnifiedResult<Value> {
    let (database_path, configured_url, maxmind_key) = {
        let config = ctx.config.read().expect("config lock poisoned");
        (
            config.geo.database_path.clone(),
            config.geo.download_url.clone(),
            config.licenses.maxmind.clone(),
        )
    };
    let url = resolve_download_url(url_override, &configured_url, &maxmind_key)?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .map_err(|error| UnifiedError::internal(format!("Cannot build HTTP client: {error}")))?;
    let response = client
        .get(url.clone())
        .send()
        .await
        .map_err(|error| UnifiedError::internal(format!("Geo database download failed: {error}")))?;
    if !response.status().is_success() {
        return Err(UnifiedError::internal(format!(
            "Geo database download failed with HTTP {}",
            response.status()
        )));
    }
    let bytes = response.bytes().await.map_err(|error| {
        UnifiedError::internal(format!("Cannot read Geo database download: {error}"))
    })?;
    if bytes.is_empty() {
        return Err(UnifiedError::internal(
            "Geo database download returned an empty file",
        ));
    }
    let path = std::path::Path::new(&database_path);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(UnifiedError::from)?;
        }
    }
    // Write to a temporary file in the same directory first: the rename
    // below is atomic on one filesystem, and the temporary file is
    // removed on any failure, so the live database is never replaced by
    // a partial or corrupt download.
    let temp_path = {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let file_name = format!(
            ".{}-tmp-{}-{}",
            path.file_name().and_then(|name| name.to_str()).unwrap_or("geolite"),
            std::process::id(),
            nanos
        );
        path.with_file_name(file_name)
    };
    let result = (|| -> UnifiedResult<()> {
        // MaxMind serves a .tar.gz; a custom URL may serve a raw .mmdb.
        let is_gzip = bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b;
        if is_gzip {
            let cursor = std::io::Cursor::new(bytes.to_vec());
            let decoder = flate2::read::GzDecoder::new(cursor);
            let mut archive = tar::Archive::new(decoder);
            let mut extracted = false;
            let entries = archive.entries().map_err(|error| {
                UnifiedError::internal(format!("Cannot read Geo database archive: {error}"))
            })?;
            for entry in entries {
                let mut entry = entry.map_err(|error| {
                    UnifiedError::internal(format!("Cannot read Geo database archive: {error}"))
                })?;
                let entry_path = entry.path().map_err(|error| {
                    UnifiedError::internal(format!("Cannot read Geo database archive: {error}"))
                })?;
                if entry_path.extension().and_then(|ext| ext.to_str()) == Some("mmdb") {
                    entry.unpack(&temp_path).map_err(|error| {
                        UnifiedError::internal(format!(
                            "Cannot extract Geo database archive: {error}"
                        ))
                    })?;
                    extracted = true;
                    break;
                }
            }
            if !extracted {
                return Err(UnifiedError::internal(
                    "Geo database archive contains no .mmdb file",
                ));
            }
        } else {
            std::fs::write(&temp_path, &bytes).map_err(UnifiedError::from)?;
        }
        // Validate the temporary file before it replaces the live one:
        // the reader must open it and answer a test lookup.
        let reader = maxminddb::Reader::open_readfile(&temp_path).map_err(|error| {
            UnifiedError::internal(format!("Downloaded Geo database is not a valid MaxMind file: {error}"))
        })?;
        let _: Value = reader
            .lookup("8.8.8.8".parse().unwrap())
            .map_err(|error| {
                UnifiedError::internal(format!(
                    "Downloaded Geo database failed a test lookup: {error}"
                ))
            })?
            .decode::<Value>()
            .map_err(|error| {
                UnifiedError::internal(format!(
                    "Downloaded Geo database failed a test lookup: {error}"
                ))
            })?
            .unwrap_or(Value::Null);
        std::fs::rename(&temp_path, path).map_err(|error| {
            UnifiedError::internal(format!("Cannot replace Geo database file: {error}"))
        })?;
        Ok(())
    })();
    if let Err(error) = result {
        let _ = std::fs::remove_file(&temp_path);
        return Err(error);
    }
    let size = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
    Ok(serde_json::json!({
        "database_path": database_path,
        "bytes": size,
        "validated": true,
    }))
}


#[cfg(test)]
mod tests {

    /// Build a request carrying the server-verified caller the auth
    /// middleware would attach for this user and role. The old
    /// self-asserted identity headers grant nothing,
    /// so tests attach the verified caller to the request
    /// extensions instead.
    fn verified_request(user_id: &str, role: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid(user_id), Vec::new())
                .with_roles(vec![role.to_string()]),
        );
        req
    }

    /// Build a request carrying a verified caller with no roles
    /// resolved: the caller resolves, but holds the anonymous role.
    /// This is the "no role" case in the new model (roles are
    /// resolved server-side, never claimed in a header).
    fn verified_request_without_roles(user_id: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::auth::Caller::verified(
                test_user_uuid(user_id),
                Vec::new(),
            ));
        req
    }

    /// Deterministic test-user identity: distinct names map to
    /// distinct UUIDs (FNV-1a folded into the low bits), so
    /// ownership tests can still tell two users apart. A value
    /// that already parses as a UUID is used as-is.
    fn test_user_uuid(name: &str) -> uuid::Uuid {
        if let Ok(id) = uuid::Uuid::parse_str(name) {
            return id;
        }
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in name.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        uuid::Uuid::from_u128(u128::from(hash) | 0x0000_0000_0000_4000_8000_0000_0000_0000)
    }

    use super::*;
    use unified_core::config::AppConfig;
    use unified_core::hooks::HookBus;
    use unified_core::modules::ModuleRegistry;
    use actix_web::test::TestRequest;

    fn test_context() -> AppContext {
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    #[test]
    fn caller_accepts_a_verified_caller() {
        let req = verified_request("user-1", "admin");
        let found = caller(&req).unwrap();
        assert_eq!(found.actor, test_user_uuid("user-1").to_string());
        assert_eq!(found.role, "admin");
    }

    #[test]
    fn caller_rejects_a_request_without_a_verified_caller() {
        let req = TestRequest::default().to_http_request();
        assert!(caller(&req).is_err());
    }

    #[test]
    fn caller_without_a_resolved_role_is_anonymous() {
        // A verified caller with no role resolved holds the anonymous
        // role: the identity resolves, the capability check is what
        // refuses it.
        let req = verified_request_without_roles("user-1");
        let found = caller(&req).expect("a verified caller resolves without a role");
        assert_eq!(found.role, "anonymous");
    }

    #[test]
    fn require_permission_fails_closed_without_a_role_check_handler() {
        // No `role.check` handler is registered on this bus, so the
        // preset `"allowed": false` must stand and the call is refused.
        let ctx = test_context();
        assert!(!check_capability(&ctx, "admin", PERM_GEO_MANAGE));
        assert!(require_permission(&ctx, "admin", PERM_GEO_MANAGE).is_err());
        assert!(require_permission(&ctx, "admin", PERM_GEO_VIEW).is_err());
    }

    #[test]
    fn require_permission_accepts_when_role_check_allows() {
        let ctx = test_context();
        ctx.hooks.register(
            "role.check",
            "test",
            Arc::new(|context: &mut unified_core::hooks::HookContext| {
                context.set("allowed", serde_json::Value::Bool(true));
            }),
        );
        assert!(require_permission(&ctx, "admin", PERM_GEO_VIEW).is_ok());
        assert!(require_permission(&ctx, "admin", PERM_GEO_MANAGE).is_ok());
    }

    #[test]
    fn resolve_download_url_rejects_override_to_unlisted_host() {
        // The classic SSRF target: an internal metadata service is not
        // the configured MaxMind host.
        let result = resolve_download_url(
            Some("http://169.254.169.254/latest/meta-data/".to_string()),
            "",
            "",
        );
        assert!(result.is_err());
    }

    #[test]
    fn resolve_download_url_rejects_lookalike_host() {
        let result = resolve_download_url(
            Some("https://download.maxmind.com.evil.example/GeoLite2-City.mmdb".to_string()),
            "",
            "",
        );
        assert!(result.is_err());
    }

    #[test]
    fn resolve_download_url_accepts_override_to_maxmind_host() {
        let url = resolve_download_url(
            Some("https://download.maxmind.com/app/geoip_download?edition_id=GeoLite2-City&suffix=tar.gz".to_string()),
            "",
            "",
        )
        .unwrap();
        assert!(url.starts_with("https://download.maxmind.com/"));
    }

    #[test]
    fn resolve_download_url_rejects_non_http_override_scheme() {
        let result = resolve_download_url(
            Some("ftp://download.maxmind.com/GeoLite2-City.mmdb".to_string()),
            "",
            "",
        );
        assert!(result.is_err());
    }

    #[test]
    fn resolve_download_url_override_must_match_configured_mirror_host() {
        let configured = "https://mirror.example.com/geo/GeoLite2-City.mmdb";
        let ok = resolve_download_url(
            Some("https://mirror.example.com/geo/other.mmdb".to_string()),
            configured,
            "",
        );
        assert!(ok.is_ok());
        let bad = resolve_download_url(
            Some("https://download.maxmind.com/GeoLite2-City.mmdb".to_string()),
            configured,
            "",
        );
        assert!(bad.is_err());
    }

    #[test]
    fn resolve_download_url_uses_configured_url_without_override() {
        let url = resolve_download_url(None, "https://mirror.example.com/geo/db.mmdb", "").unwrap();
        assert_eq!(url, "https://mirror.example.com/geo/db.mmdb");
    }

    #[test]
    fn resolve_download_url_builds_maxmind_url_from_license_key() {
        let url = resolve_download_url(None, "", "test-license-key").unwrap();
        assert!(url.starts_with("https://download.maxmind.com/app/geoip_download"));
        assert!(url.contains("license_key=test-license-key"));
    }

    #[test]
    fn resolve_download_url_errors_when_nothing_is_configured() {
        assert!(resolve_download_url(None, "", "").is_err());
        assert!(resolve_download_url(Some("   ".to_string()), "", "").is_err());
    }
}
