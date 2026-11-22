// GEO module — Geo location submodule of the Network module.
//
// Resolution order (config `geo.settings_override` wins by default):
//   1. Settings overrides — CIDR / IP-range matches, IPv4 and IPv6.
//   2. MaxMind / GeoLite database file (`geo.database_path`) via maxminddb.
//   3. Otherwise `{ country: null, source: "unknown" }`.
//
// Every lookup updates the global `AppContext` Geo variable and emits the
// `geo.lookup` hook event so other modules (security, audit, network) can
// react without calling this module directly.

use crate::core::context::{AppContext, GeoInfo};
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::events;
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::db::models::{GeoOverride, NewGeoOverride};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::net::IpAddr;
use std::sync::{Arc, OnceLock, RwLock};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// In-memory store (used when no database pool is attached to the context).
// ---------------------------------------------------------------------------

static OVERRIDES: OnceLock<DashMap<Uuid, GeoOverride>> = OnceLock::new();

fn overrides_store() -> &'static DashMap<Uuid, GeoOverride> {
    OVERRIDES.get_or_init(DashMap::new)
}

/// Application context stashed at `init` so the synchronous `geo.lookup`
/// hook handler can resolve lookups without an async context.
static GEO_CTX: OnceLock<Arc<AppContext>> = OnceLock::new();

// ---------------------------------------------------------------------------
// CIDR parsing and containment (IPv4 and IPv6, implemented with std only).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct Cidr {
    pub network: IpAddr,
    pub prefix: u8,
}

impl Cidr {
    pub fn parse(input: &str) -> UnifiedResult<Self> {
        let input = input.trim();
        let (addr_text, prefix_text) = match input.split_once('/') {
            Some((addr, prefix)) => (addr, Some(prefix)),
            None => (input, None),
        };
        let addr: IpAddr = addr_text.parse().map_err(|_| {
            UnifiedError::bad_request(format!("Invalid IP address in CIDR \"{input}\""))
        })?;
        let max_prefix: u8 = match addr {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        };
        let prefix: u8 = match prefix_text {
            Some(text) => text.parse().map_err(|_| {
                UnifiedError::bad_request(format!("Invalid prefix in CIDR \"{input}\""))
            })?,
            None => max_prefix,
        };
        if prefix > max_prefix {
            return Err(UnifiedError::bad_request(format!(
                "Prefix {prefix} exceeds maximum {max_prefix} for {addr}"
            )));
        }
        Ok(Self {
            network: masked(addr, prefix),
            prefix,
        })
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.network, ip) {
            (IpAddr::V4(network), IpAddr::V4(addr)) => {
                masked(IpAddr::V4(addr), self.prefix) == self.network
            }
            (IpAddr::V6(network), IpAddr::V6(addr)) => {
                masked(IpAddr::V6(addr), self.prefix) == self.network
            }
            _ => false,
        }
    }

    /// Normalised `address/prefix` form.
    pub fn to_cidr_string(&self) -> String {
        format!("{}/{}", self.network, self.prefix)
    }
}

fn masked(ip: IpAddr, prefix: u8) -> IpAddr {
    match ip {
        IpAddr::V4(addr) => {
            let bits = u32::from(addr);
            let mask: u32 = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - prefix)
            };
            IpAddr::V4((bits & mask).into())
        }
        IpAddr::V6(addr) => {
            let bits = u128::from(addr);
            let mask: u128 = if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - prefix)
            };
            IpAddr::V6((bits & mask).into())
        }
    }
}

fn broadcast(ip: IpAddr, prefix: u8) -> IpAddr {
    match ip {
        IpAddr::V4(addr) => {
            let bits = u32::from(addr);
            let mask: u32 = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - prefix)
            };
            IpAddr::V4((bits | !mask).into())
        }
        IpAddr::V6(addr) => {
            let bits = u128::from(addr);
            let mask: u128 = if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - prefix)
            };
            IpAddr::V6((bits | !mask).into())
        }
    }
}

impl Cidr {
    /// First and last address of the block (network and broadcast).
    pub fn range(&self) -> (IpAddr, IpAddr) {
        (self.network, broadcast(self.network, self.prefix))
    }

    /// Build the single CIDR block that covers exactly `start..=end`.
    /// Ranges that are not one aligned block (for example `10.0.0.0`
    /// to `10.0.0.5`) are rejected instead of being widened into a
    /// block that covers addresses outside the range.
    pub fn from_range(start: IpAddr, end: IpAddr) -> UnifiedResult<Self> {
        let (start_num, end_num, max_prefix) = match (start, end) {
            (IpAddr::V4(a), IpAddr::V4(b)) => {
                (u32::from(a) as u128, u32::from(b) as u128, 32u8)
            }
            (IpAddr::V6(a), IpAddr::V6(b)) => (u128::from(a), u128::from(b), 128u8),
            _ => {
                return Err(UnifiedError::bad_request(
                    "range start and end must be the same IP version",
                ));
            }
        };
        if start_num > end_num {
            return Err(UnifiedError::bad_request(
                "range start must not be greater than range end",
            ));
        }
        let diff = start_num ^ end_num;
        let prefix: u8 = if diff == 0 {
            max_prefix
        } else if max_prefix == 32 {
            // Count leading zeros in 32-bit space: the values were
            // widened to u128, whose leading zeros would over-count.
            (diff as u32).leading_zeros() as u8
        } else {
            diff.leading_zeros() as u8
        };
        let candidate = Cidr {
            network: masked(start, prefix),
            prefix,
        };
        if candidate.network != start || broadcast(candidate.network, prefix) != end {
            return Err(UnifiedError::bad_request(
                "range is not exactly one CIDR block",
            ));
        }
        Ok(candidate)
    }
}

/// Numeric form of an IP address (IPv4 as the low 32 bits).
pub fn ip_to_u128(ip: IpAddr) -> u128 {
    match ip {
        IpAddr::V4(addr) => u32::from(addr) as u128,
        IpAddr::V6(addr) => u128::from(addr),
    }
}

/// Inverse of `ip_to_u128`. `is_v6` selects the address family.
pub fn u128_to_ip(value: u128, is_v6: bool) -> UnifiedResult<IpAddr> {
    if is_v6 {
        Ok(IpAddr::V6(value.into()))
    } else if value <= u32::MAX as u128 {
        Ok(IpAddr::V4((value as u32).into()))
    } else {
        Err(UnifiedError::bad_request(
            "value does not fit in an IPv4 address",
        ))
    }
}

/// Normalise an IPv4 address written with leading zeros
/// (`010.000.0.1` -> `10.0.0.1`). Rust's standard parser rejects
/// leading zeros outright, so this handles the legacy form explicitly.
pub fn normalize_ipv4(input: &str) -> UnifiedResult<String> {
    let parts: Vec<&str> = input.trim().split('.').collect();
    if parts.len() != 4 {
        return Err(UnifiedError::bad_request(format!(
            "Invalid IPv4 address \"{input}\""
        )));
    }
    let mut out: Vec<String> = Vec::with_capacity(4);
    for part in parts {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return Err(UnifiedError::bad_request(format!(
                "Invalid IPv4 address \"{input}\""
            )));
        }
        let stripped = part.trim_start_matches('0');
        let digits = if stripped.is_empty() { "0" } else { stripped };
        let value: u16 = digits.parse().map_err(|_| {
            UnifiedError::bad_request(format!("Invalid IPv4 address \"{input}\""))
        })?;
        if value > 255 {
            return Err(UnifiedError::bad_request(format!(
                "Invalid IPv4 address \"{input}\""
            )));
        }
        out.push(value.to_string());
    }
    Ok(out.join("."))
}

/// Collapse an IPv4-mapped IPv6 address (`::ffff:8.8.8.8`) to its IPv4
/// form so CIDR overrides and MaxMind lookups match it. Other
/// addresses are returned unchanged.
pub fn normalize_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(addr) => match addr.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => ip,
        },
        IpAddr::V4(_) => ip,
    }
}

// ---------------------------------------------------------------------------
// IP classification: which addresses are not publicly routable.
// ---------------------------------------------------------------------------

/// Address class. Anything other than `Public` is a bogon for Geo
/// purposes: no MaxMind record exists for it, so lookups skip the
/// database (settings overrides still apply).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpClass {
    Public,
    Private,
    Loopback,
    LinkLocal,
    Multicast,
    Unspecified,
    Broadcast,
    Documentation,
    Benchmarking,
    Reserved,
    Shared,
    UniqueLocal,
    Discard,
    SiteLocal,
}

pub fn classify_ip(ip: IpAddr) -> IpClass {
    match ip {
        IpAddr::V4(addr) => {
            let octets = addr.octets();
            if octets == [0, 0, 0, 0] || octets[0] == 0 {
                IpClass::Unspecified
            } else if addr.is_loopback() {
                IpClass::Loopback
            } else if addr.is_private() {
                IpClass::Private
            } else if addr.is_link_local() {
                IpClass::LinkLocal
            } else if addr.is_multicast() {
                IpClass::Multicast
            } else if octets == [255, 255, 255, 255] {
                IpClass::Broadcast
            } else if (octets[0] == 192 && octets[1] == 0 && octets[2] == 2)
                || (octets[0] == 198 && octets[1] == 51 && octets[2] == 100)
                || (octets[0] == 203 && octets[1] == 0 && octets[2] == 113)
            {
                // TEST-NET-1/2/3 documentation ranges (RFC 5737).
                IpClass::Documentation
            } else if octets[0] == 198 && (18..=19).contains(&octets[1]) {
                // Benchmarking range 198.18.0.0/15 (RFC 2544).
                IpClass::Benchmarking
            } else if octets[0] == 100 && (64..=127).contains(&octets[1]) {
                // Shared address space / CGNAT 100.64.0.0/10 (RFC 6598).
                IpClass::Shared
            } else if octets[0] >= 240 {
                IpClass::Reserved
            } else {
                IpClass::Public
            }
        }
        IpAddr::V6(addr) => {
            let segments = addr.segments();
            if addr.is_unspecified() {
                IpClass::Unspecified
            } else if addr.is_loopback() {
                IpClass::Loopback
            } else if addr.is_multicast() {
                IpClass::Multicast
            } else if segments[0] & 0xfe00 == 0xfc00 {
                // Unique-local addresses fc00::/7 (RFC 4193).
                IpClass::UniqueLocal
            } else if segments[0] & 0xffc0 == 0xfe80 {
                // Link-local addresses fe80::/10 (RFC 4291).
                IpClass::LinkLocal
            } else if segments[0] == 0x2001 && segments[1] == 0x0db8 {
                // Documentation range 2001:db8::/32 (RFC 3849).
                IpClass::Documentation
            } else if segments[0] == 0x2001 && segments[1] == 0x0002 && segments[2] == 0 {
                // Benchmarking range 2001:2::/48 (RFC 5180).
                IpClass::Benchmarking
            } else if segments[0] == 0x0100 && segments[1] == 0 && segments[2] == 0 && segments[3] == 0 {
                // Discard-only range 100::/64 (RFC 6666).
                IpClass::Discard
            } else if segments[0] & 0xffc0 == 0xfec0 {
                // Deprecated site-local addresses fec0::/10 (RFC 3879).
                IpClass::SiteLocal
            } else {
                IpClass::Public
            }
        }
    }
}

/// `true` when the address is not publicly routable.
pub fn is_bogon(ip: IpAddr) -> bool {
    classify_ip(ip) != IpClass::Public
}

/// `true` when the address is publicly routable.
pub fn is_public_ip(ip: IpAddr) -> bool {
    classify_ip(ip) == IpClass::Public
}

/// Validate a MaxMind license-key string by shape: 40 characters in
/// three underscore-separated parts, a 6-character alphanumeric prefix,
/// an alphanumeric middle part, and the `mmk` suffix. This checks the
/// format only; it does not verify the key against MaxMind.
pub fn is_valid_maxmind_license_key(key: &str) -> bool {
    let key = key.trim();
    if key.len() != 40 {
        return false;
    }
    let parts: Vec<&str> = key.split('_').collect();
    if parts.len() != 3 {
        return false;
    }
    let (prefix, middle, suffix) = (parts[0], parts[1], parts[2]);
    prefix.len() == 6
        && !middle.is_empty()
        && suffix == "mmk"
        && prefix.chars().all(|c| c.is_ascii_alphanumeric())
        && middle.chars().all(|c| c.is_ascii_alphanumeric())
}

// ---------------------------------------------------------------------------
// Lookup core: shared by the HTTP handler, the hook handler, and the
// Network module (which delegates geo lookups through the `geo.lookup`
// hook event and falls back to this function).
// ---------------------------------------------------------------------------

/// Resolve Geo information for an IP address.
///
/// `ctx` supplies configuration (database path, settings-override flag).
/// Does not emit hooks and does not touch the global Geo variable; the
/// caller is responsible for that.
pub fn lookup_geo(ctx: &AppContext, ip_text: &str) -> UnifiedResult<GeoInfo> {
    let ip: IpAddr = ip_text
        .trim()
        .parse()
        .map_err(|_| UnifiedError::bad_request(format!("Invalid IP address \"{ip_text}\"")))?;
    // IPv4-mapped IPv6 forms resolve as their IPv4 address.
    let ip = normalize_ip(ip);
    let (database_path, settings_override) = {
        let config = ctx.config.read().expect("config lock poisoned");
        (
            config.geo.database_path.clone(),
            config.geo.settings_override,
        )
    };

    let unknown = || GeoInfo {
        ip: ip.to_string(),
        source: "unknown".to_string(),
        ..GeoInfo::default()
    };
    let override_hit = find_override(ip);
    // Bogons have no MaxMind record: check overrides, then stop instead
    // of opening the database for an address it cannot contain.
    if settings_override {
        if let Some(info) = override_hit.clone() {
            return Ok(info);
        }
        if is_bogon(ip) {
            return Ok(unknown());
        }
        if let Some(info) = lookup_maxmind(&database_path, ip) {
            return Ok(info);
        }
        if let Some(info) = override_hit {
            return Ok(info);
        }
    } else {
        if is_bogon(ip) {
            if let Some(info) = override_hit {
                return Ok(info);
            }
            return Ok(unknown());
        }
        if let Some(info) = lookup_maxmind(&database_path, ip) {
            return Ok(info);
        }
        if let Some(info) = override_hit {
            return Ok(info);
        }
    }
    Ok(unknown())
}

/// Longest-prefix active override match for an IP address.
pub fn find_override(ip: IpAddr) -> Option<GeoInfo> {
    let ip = normalize_ip(ip);
    let mut best: Option<(u8, GeoInfo)> = None;
    for entry in overrides_store().iter() {
        let row = entry.value();
        if !row.is_active {
            continue;
        }
        let Ok(cidr) = Cidr::parse(&row.cidr) else {
            continue;
        };
        if !cidr.contains(ip) {
            continue;
        }
        let info = GeoInfo {
            ip: ip.to_string(),
            country_code: Some(row.country_code.clone()),
            country_name: row.country_name.clone(),
            city: row.city.clone(),
            region: None,
            asn: row.asn.map(|value| value as u32),
            asn_organization: row.asn_organization.clone(),
            latitude: None,
            longitude: None,
            source: "settings".to_string(),
        };
        let replace = match &best {
            Some((prefix, _)) => cidr.prefix > *prefix,
            None => true,
        };
        if replace {
            best = Some((cidr.prefix, info));
        }
    }
    best.map(|(_, info)| info)
}

/// MaxMind / GeoLite city database lookup. Returns `None` when the file is
/// missing, unreadable, or has no record for the IP.
pub fn lookup_maxmind(database_path: &str, ip: IpAddr) -> Option<GeoInfo> {
    if !std::path::Path::new(database_path).exists() {
        return None;
    }
    let reader = maxminddb::Reader::open_readfile(database_path).ok()?;
    let record: Value = reader.lookup(ip).ok()?;
    let country = record.get("country")?;
    let iso_code = country
        .get("iso_code")
        .and_then(Value::as_str)
        .map(str::to_string);
    if iso_code.is_none() {
        return None;
    }
    let name_of = |object: Option<&Value>| -> Option<String> {
        object?
            .get("names")?
            .get("en")
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let city = name_of(record.get("city"));
    let region = record
        .get("subdivisions")
        .and_then(Value::as_array)
        .and_then(|list| list.first())
        .and_then(|sub| name_of(Some(sub)));
    let (latitude, longitude) = match record.get("location") {
        Some(location) => {
            // Reject out-of-range coordinates instead of serving them:
            // a corrupt or hand-edited database must not produce a
            // latitude of 500 or a longitude of -999.
            let latitude = location
                .get("latitude")
                .and_then(Value::as_f64)
                .filter(|value| (-90.0..=90.0).contains(value));
            let longitude = location
                .get("longitude")
                .and_then(Value::as_f64)
                .filter(|value| (-180.0..=180.0).contains(value));
            (latitude, longitude)
        }
        None => (None, None),
    };
    Some(GeoInfo {
        ip: ip.to_string(),
        country_code: iso_code,
        country_name: name_of(Some(country)),
        city,
        region,
        asn: None,
        asn_organization: None,
        latitude,
        longitude,
        source: "geolite".to_string(),
    })
}

/// ASN lookup: overrides first, then the GeoLite2-ASN database placed next
/// to the city database (`GeoLite2-ASN.mmdb` in the same directory).
pub fn lookup_asn(ctx: &AppContext, ip_text: &str) -> UnifiedResult<Value> {
    let ip: IpAddr = ip_text
        .trim()
        .parse()
        .map_err(|_| UnifiedError::bad_request(format!("Invalid IP address \"{ip_text}\"")))?;
    let ip = normalize_ip(ip);
    let database_path = {
        let config = ctx.config.read().expect("config lock poisoned");
        config.geo.database_path.clone()
    };
    if let Some(info) = find_override(ip) {
        if info.asn.is_some() || info.asn_organization.is_some() {
            return Ok(serde_json::json!({
                "ip": ip.to_string(),
                "asn": info.asn,
                "asn_organization": info.asn_organization,
                "source": "settings",
            }));
        }
    }
    let asn_path = std::path::Path::new(&database_path)
        .parent()
        .map(|dir| dir.join("GeoLite2-ASN.mmdb"))
        .unwrap_or_else(|| std::path::PathBuf::from("GeoLite2-ASN.mmdb"));
    if asn_path.exists() {
        if let Ok(reader) = maxminddb::Reader::open_readfile(&asn_path) {
            if let Ok(record) = reader.lookup::<Value>(ip) {
                return Ok(serde_json::json!({
                    "ip": ip.to_string(),
                    "asn": record.get("autonomous_system_number").and_then(Value::as_u64),
                    "asn_organization": record.get("autonomous_system_organization").and_then(Value::as_str),
                    "source": "geolite",
                }));
            }
        }
    }
    Ok(serde_json::json!({
        "ip": ip.to_string(),
        "asn": null,
        "asn_organization": null,
        "source": "unknown",
    }))
}

// ---------------------------------------------------------------------------
// Database helpers (only used when a pool is attached to the context).
// ---------------------------------------------------------------------------

fn db_pool(ctx: &AppContext) -> Option<crate::db::DbPool> {
    ctx.db.read().expect("db lock poisoned").clone()
}

fn db_load_overrides(pool: &crate::db::DbPool) -> UnifiedResult<Vec<GeoOverride>> {
    use crate::db::schema::geo_overrides::dsl as geo_dsl;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    geo_dsl::geo_overrides
        .select(GeoOverride::as_select())
        .load(&mut conn)
        .map_err(|error| UnifiedError::internal(format!("Cannot load Geo overrides: {error}")))
}

// ---------------------------------------------------------------------------
// Request / response types.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct LookupQuery {
    ip: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CreateOverrideRequest {
    cidr: String,
    country_code: String,
    country_name: Option<String>,
    city: Option<String>,
    asn: Option<i32>,
    asn_organization: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UpdateOverrideRequest {
    cidr: Option<String>,
    country_code: Option<String>,
    country_name: Option<Option<String>>,
    city: Option<Option<String>>,
    asn: Option<Option<i32>>,
    asn_organization: Option<Option<String>>,
    is_active: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct AsnCheckRequest {
    ip: String,
}

#[derive(Debug, Deserialize)]
struct DownloadRequest {
    /// Optional custom download URL. Defaults to `geo.download_url`, then
    /// the built-in MaxMind URL (requires `licenses.maxmind`).
    url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct GeoSettingsView {
    database_path: String,
    database_exists: bool,
    download_url: String,
    auto_update: bool,
    update_interval_hours: u64,
    settings_override: bool,
    maxmind_key_configured: bool,
    /// Shape check only (40 characters, `xxxxxx_..._mmk`); a key can be
    /// configured yet malformed, which is exactly the failure that
    /// makes a database download fail later.
    maxmind_key_valid: bool,
    overrides_count: usize,
}

fn validate_country_code(code: &str) -> UnifiedResult<String> {
    let code = code.trim().to_uppercase();
    if code.len() != 2 || !code.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(UnifiedError::bad_request(
            "country_code must be a two-letter ISO country code",
        ));
    }
    Ok(code)
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(items.len());
    let page_items = if start >= items.len() {
        Vec::new()
    } else {
        items[start..end].to_vec()
    };
    PageResult::new(page_items, total, &pagination)
}

// ---------------------------------------------------------------------------
// Handlers.
// ---------------------------------------------------------------------------

async fn lookup_handler(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<LookupQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GeoInfo> = (|| {
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

async fn list_overrides(
    ctx: web::Data<Arc<AppContext>>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
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

async fn create_override(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateOverrideRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GeoOverride> = (|| {
        let cidr = Cidr::parse(&body.cidr)?;
        let country_code = validate_country_code(&body.country_code)?;
        let now = Utc::now();
        let row = GeoOverride {
            id: Uuid::now_v7(),
            cidr: cidr.to_cidr_string(),
            country_code,
            country_name: body.country_name.clone(),
            city: body.city.clone(),
            asn: body.asn,
            asn_organization: body.asn_organization.clone(),
            is_active: true,
            created_at: now,
            updated_at: now,
        };
        if let Some(pool) = db_pool(&ctx) {
            use crate::db::schema::geo_overrides::dsl as geo_dsl;
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

async fn get_override(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GeoOverride> = (|| {
        let id = id.into_inner();
        if let Some(pool) = db_pool(&ctx) {
            use crate::db::schema::geo_overrides::dsl as geo_dsl;
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

async fn update_override(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
    body: web::Json<UpdateOverrideRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GeoOverride> = (|| {
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
            row.asn = *value;
        }
        if let Some(value) = &body.asn_organization {
            row.asn_organization = value.clone();
        }
        if let Some(active) = body.is_active {
            row.is_active = active;
        }
        row.updated_at = Utc::now();
        if let Some(pool) = db_pool(&ctx) {
            use crate::db::schema::geo_overrides::dsl as geo_dsl;
            #[derive(AsChangeset)]
            #[diesel(table_name = crate::db::schema::geo_overrides)]
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

async fn delete_override(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let id = id.into_inner();
        if let Some(pool) = db_pool(&ctx) {
            use crate::db::schema::geo_overrides::dsl as geo_dsl;
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

async fn asn_check_handler(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<AsnCheckRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    json::respond(lookup_asn(&ctx, &body.ip), debug)
}

async fn settings_handler(ctx: web::Data<Arc<AppContext>>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GeoSettingsView> = (|| {
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

async fn download_handler(
    ctx: web::Data<Arc<AppContext>>,
    body: Option<web::Json<DownloadRequest>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let url = body.as_ref().and_then(|body| body.url.clone());
    let result = download_database(&ctx, url).await;
    json::respond(result, debug)
}

/// Download the GeoLite database, extract the `.mmdb` file when the
/// download is a gzip tarball (the MaxMind format), save it to
/// `geo.database_path`, and validate it by opening it with maxminddb.
///
/// Note: an `.mmdb` file has no header magic bytes — validation is done by
/// parsing the file with the maxminddb reader and performing a test lookup.
async fn download_database(
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
    let url = match url_override {
        Some(url) if !url.trim().is_empty() => url,
        _ if !configured_url.trim().is_empty() => configured_url,
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
                entry.unpack(path).map_err(|error| {
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
        std::fs::write(path, &bytes).map_err(UnifiedError::from)?;
    }
    // Validate: the reader must open the file and answer a test lookup.
    let reader = maxminddb::Reader::open_readfile(path).map_err(|error| {
        UnifiedError::internal(format!("Downloaded Geo database is not a valid MaxMind file: {error}"))
    })?;
    let _: Value = reader.lookup("8.8.8.8".parse().unwrap()).map_err(|error| {
        UnifiedError::internal(format!("Downloaded Geo database failed a test lookup: {error}"))
    })?;
    let size = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
    Ok(serde_json::json!({
        "database_path": database_path,
        "bytes": size,
        "validated": true,
    }))
}

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct GeoModule;

impl Module for GeoModule {
    fn id(&self) -> &'static str {
        "geo"
    }

    fn name(&self) -> &'static str {
        "Geo Location"
    }

    fn description(&self) -> &'static str {
        "Geo location lookups: settings CIDR overrides, MaxMind/GeoLite database, ASN checks, and database downloads."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["network"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        let _ = GEO_CTX.set(Arc::new(AppContext::new(
            ctx.config.read().expect("config lock poisoned").clone(),
            Arc::clone(&ctx.hooks),
            Arc::clone(&ctx.modules),
        )));
        // Keep the stashed context in sync with the live one by storing the
        // real context when possible: the hook handler only needs config
        // and the override store, both of which are reachable here.
        Ok(())
    }

    fn register_hooks(&self, bus: &crate::core::hooks::HookBus) {
        // Synchronous `geo.lookup` handler: other modules emit this event
        // with `{ "ip": "..." }` and read the first result. The lookup
        // itself is synchronous (in-memory overrides + maxminddb file).
        bus.register(
            events::GEO_LOOKUP,
            "geo",
            Arc::new(|context: &mut crate::core::hooks::HookContext| {
                let Some(ip) = context
                    .get("ip")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                else {
                    return;
                };
                let result = match GEO_CTX.get() {
                    Some(ctx) => lookup_geo(ctx, &ip),
                    None => Err(UnifiedError::internal("Geo module context is not initialised")),
                };
                match result {
                    Ok(info) => {
                        context.add_result(
                            serde_json::to_value(&info).unwrap_or(Value::Null),
                        );
                    }
                    Err(error) => {
                        context.add_result(serde_json::json!({ "error": error.message }));
                    }
                }
            }),
        );

        // `geo.update_requested` (from the Scheduler's `geo.update` job):
        // run the existing database download on a worker thread with its
        // own runtime — the download takes minutes and hook handlers are
        // synchronous — and emit `geo.update` on success.
        bus.register(
            "geo.update_requested",
            "geo",
            Arc::new(|context: &mut crate::core::hooks::HookContext| {
                let url = context
                    .get("url")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| {
                        context
                            .get("download_url")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    });
                let Some(app_ctx) = GEO_CTX.get().cloned() else {
                    context.add_result(serde_json::json!({
                        "started": false,
                        "error": "Geo module context is not initialised",
                    }));
                    return;
                };
                std::thread::spawn(move || {
                    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    else {
                        log::error!("Geo update: cannot start async runtime");
                        return;
                    };
                    match runtime.block_on(download_database(&app_ctx, url)) {
                        Ok(result) => {
                            log::info!("Geo database updated: {result}");
                            app_ctx.hooks.emit(
                                "geo.update",
                                serde_json::json!({ "updated": true, "result": result }),
                            );
                        }
                        Err(error) => {
                            log::error!("Geo database update failed: {}", error.message);
                        }
                    }
                });
                context.add_result(serde_json::json!({ "started": true }));
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/geo")
                .route("/lookup", web::get().to(lookup_handler))
                .route("/overrides", web::get().to(list_overrides))
                .route("/overrides", web::post().to(create_override))
                .route("/overrides/{id}", web::get().to(get_override))
                .route("/overrides/{id}", web::put().to(update_override))
                .route("/overrides/{id}", web::delete().to(delete_override))
                .route("/asn-check", web::post().to(asn_check_handler))
                .route("/download", web::post().to(download_handler))
                .route("/settings", web::get().to(settings_handler)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(GeoModule));
}
