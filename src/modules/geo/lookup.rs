//! geo - lookup.

use unified_core::context::{AppContext, GeoInfo};
use unified_core::error::{UnifiedError, UnifiedResult};
use serde_json::Value;
use std::net::IpAddr;
use super::{store::*, cidr::*, classification::*, db::*, types::*, handlers::*, module::*};



// Lookup core: shared by the HTTP handler, the hook handler, and the
// Network module (which delegates geo lookups through the `geo.lookup`
// hook event and falls back to this function).
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
            // Rows stored before ASN validation may hold a negative or
            // zero value; only a positive ASN converts safely to `u32`
            // (a negative `i32` cast with `as u32` would wrap to a huge
            // number in every lookup answer).
            asn: row.asn.filter(|value| *value > 0).map(|value| value as u32),
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
    let record: Value = reader.lookup(ip).ok()?.decode().ok().flatten()?;
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
            if let Some(record) = reader.lookup(ip).ok().and_then(|result| result.decode::<Value>().ok()).flatten() {
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


#[cfg(test)]
mod tests {
    use super::*;
    use unified_core::db::models::GeoOverride;
    use chrono::Utc;
    use uuid::Uuid;

    fn test_override(cidr: &str, asn: Option<i32>) -> GeoOverride {
        let now = Utc::now();
        GeoOverride {
            id: Uuid::now_v7(),
            cidr: cidr.to_string(),
            country_code: "US".to_string(),
            country_name: None,
            city: None,
            asn,
            asn_organization: None,
            is_active: true,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn find_override_drops_non_positive_asn_instead_of_wrapping() {
        // A negative ASN stored before validation must not wrap to
        // 4294967295, and zero must not surface as ASN 0.
        let negative = test_override("203.0.113.0/24", Some(-1));
        let zero = test_override("198.51.100.0/24", Some(0));
        let positive = test_override("192.0.2.0/24", Some(15169));
        overrides_store().insert(negative.id, negative.clone());
        overrides_store().insert(zero.id, zero.clone());
        overrides_store().insert(positive.id, positive.clone());
        let negative_hit = find_override("203.0.113.5".parse().unwrap());
        let zero_hit = find_override("198.51.100.5".parse().unwrap());
        let positive_hit = find_override("192.0.2.5".parse().unwrap());
        overrides_store().remove(&negative.id);
        overrides_store().remove(&zero.id);
        overrides_store().remove(&positive.id);
        assert_eq!(negative_hit.unwrap().asn, None);
        assert_eq!(zero_hit.unwrap().asn, None);
        assert_eq!(positive_hit.unwrap().asn, Some(15169));
    }
}
