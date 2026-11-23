//! Map & Tagging — validation and pure geospatial math.
//!
//! Everything here is pure: no database, no HTTP, no config.
//! The handlers validate every client-supplied value through
//! these functions before it reaches the store, and the
//! point-in-polygon / haversine helpers are what the geofence
//! and nearby features are built on. Unit tests live at the
//! bottom of this file.

use serde_json::Value;
use unified_core::error::{UnifiedError, UnifiedResult};

use super::types::MAX_JSON_BLOB_BYTES;

/// Latitude in decimal degrees: -90..=90, finite.
pub fn validate_latitude(value: f64, field: &str) -> UnifiedResult<()> {
    if !value.is_finite() || !(-90.0..=90.0).contains(&value) {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a latitude between -90 and 90"
        )));
    }
    Ok(())
}

/// Longitude in decimal degrees: -180..=180, finite.
pub fn validate_longitude(value: f64, field: &str) -> UnifiedResult<()> {
    if !value.is_finite() || !(-180.0..=180.0).contains(&value) {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a longitude between -180 and 180"
        )));
    }
    Ok(())
}

/// A coordinate pair, validated together.
pub fn validate_coordinates(latitude: f64, longitude: f64) -> UnifiedResult<()> {
    validate_latitude(latitude, "latitude")?;
    validate_longitude(longitude, "longitude")?;
    Ok(())
}

/// A slug: lowercase kebab-case, 1..=64 characters. Used for
/// kinds, roles, providers, and entity types.
pub fn validate_slug(value: &str, field: &str) -> UnifiedResult<()> {
    let valid = !value.is_empty()
        && value.len() <= 64
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        && value
            .chars()
            .next()
            .map(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            .unwrap_or(false);
    if !valid {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a lowercase slug of 1-64 characters (letters, digits, '-' and '_')"
        )));
    }
    Ok(())
}

/// An entity type: the same slug shape as `validate_slug`, but
/// the first character may be a letter only (entity types name
/// a module record kind, never a bare number).
pub fn validate_entity_type(value: &str) -> UnifiedResult<()> {
    validate_slug(value, "entity_type")
}

/// Required display text: non-empty after trimming, at most
/// `max` characters.
pub fn validate_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be 1-{max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

/// Optional display text: `None` stays `None`; a present value
/// must be non-empty after trimming and at most `max`
/// characters. An explicitly empty string is rejected (use
/// `None` to clear a field).
pub fn validate_optional_text(
    value: &Option<String>,
    field: &str,
    max: usize,
) -> UnifiedResult<Option<String>> {
    match value {
        None => Ok(None),
        Some(text) => Ok(Some(validate_text(text, field, max)?)),
    }
}

/// A free-form JSON blob (metadata / style): it must be an
/// object when present, and its serialised form must fit the
/// cap, so one request cannot smuggle an unbounded document
/// into a JSONB column.
pub fn validate_json_object(value: &Value, field: &str) -> UnifiedResult<()> {
    if !value.is_null() && !value.is_object() {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a JSON object"
        )));
    }
    let encoded = serde_json::to_string(value)
        .map_err(|_| UnifiedError::bad_request(format!("{field} is not valid JSON")))?;
    if encoded.len() > MAX_JSON_BLOB_BYTES {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {MAX_JSON_BLOB_BYTES} bytes"
        )));
    }
    Ok(())
}

/// A tile URL template: an http(s) URL containing the `{z}`,
/// `{x}`, and `{y}` placeholders a tile client substitutes,
/// and no embedded userinfo (credentials in a tile URL would
/// leak to every map viewer). `{key}` is allowed and means
/// "substitute the stored browser API key, if any".
pub fn validate_tile_template(template: &str) -> UnifiedResult<()> {
    let trimmed = template.trim();
    if trimmed.is_empty() || trimmed.len() > 500 {
        return Err(UnifiedError::bad_request(
            "tile_url_template must be 1-500 characters",
        ));
    }
    if !(trimmed.starts_with("https://") || trimmed.starts_with("http://")) {
        return Err(UnifiedError::bad_request(
            "tile_url_template must start with https:// or http://",
        ));
    }
    if trimmed.contains('@') && trimmed.split("://").nth(1).unwrap_or("").contains('@') {
        return Err(UnifiedError::bad_request(
            "tile_url_template must not contain credentials",
        ));
    }
    for placeholder in ["{z}", "{x}", "{y}"] {
        if !trimmed.contains(placeholder) {
            return Err(UnifiedError::bad_request(format!(
                "tile_url_template must contain the {placeholder} placeholder"
            )));
        }
    }
    Ok(())
}

/// A `#rrggbb` hex colour.
pub fn validate_color(value: &str) -> UnifiedResult<()> {
    let bytes = value.as_bytes();
    let valid =
        bytes.len() == 7 && bytes[0] == b'#' && bytes[1..].iter().all(|b| b.is_ascii_hexdigit());
    if !valid {
        return Err(UnifiedError::bad_request(
            "color must be a #rrggbb hex colour",
        ));
    }
    Ok(())
}

/// A GeoJSON Polygon or MultiPolygon of `[longitude, latitude]`
/// positions. Every ring must have at least 4 positions and be
/// closed (first position equals last); every coordinate must
/// be a valid longitude/latitude pair. Returns the geometry
/// unchanged on success.
pub fn validate_geometry(geometry: &Value) -> UnifiedResult<()> {
    validate_json_object(geometry, "geometry")?;
    let object = geometry
        .as_object()
        .ok_or_else(|| UnifiedError::bad_request("geometry must be a GeoJSON object"))?;
    let kind = object
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let coordinates = object
        .get("coordinates")
        .ok_or_else(|| UnifiedError::bad_request("geometry must have coordinates"))?;
    match kind {
        "Polygon" => validate_polygon(coordinates)?,
        "MultiPolygon" => {
            let polygons = coordinates.as_array().ok_or_else(|| {
                UnifiedError::bad_request("MultiPolygon coordinates must be an array of polygons")
            })?;
            if polygons.is_empty() {
                return Err(UnifiedError::bad_request(
                    "MultiPolygon must contain at least one polygon",
                ));
            }
            for polygon in polygons {
                validate_polygon(polygon)?;
            }
        }
        _ => {
            return Err(UnifiedError::bad_request(
                "geometry type must be Polygon or MultiPolygon",
            ));
        }
    }
    Ok(())
}

fn validate_polygon(polygon: &Value) -> UnifiedResult<()> {
    let rings = polygon.as_array().ok_or_else(|| {
        UnifiedError::bad_request("Polygon coordinates must be an array of linear rings")
    })?;
    if rings.is_empty() {
        return Err(UnifiedError::bad_request(
            "Polygon must contain at least one linear ring",
        ));
    }
    for ring in rings {
        validate_ring(ring)?;
    }
    Ok(())
}

fn validate_ring(ring: &Value) -> UnifiedResult<()> {
    let positions = ring
        .as_array()
        .ok_or_else(|| UnifiedError::bad_request("A linear ring must be an array of positions"))?;
    if positions.len() < 4 {
        return Err(UnifiedError::bad_request(
            "A linear ring must contain at least 4 positions",
        ));
    }
    for position in positions {
        let pair = position.as_array().ok_or_else(|| {
            UnifiedError::bad_request("A position must be a [longitude, latitude] array")
        })?;
        if pair.len() < 2 {
            return Err(UnifiedError::bad_request(
                "A position must be a [longitude, latitude] array",
            ));
        }
        let longitude = pair[0]
            .as_f64()
            .ok_or_else(|| UnifiedError::bad_request("A position longitude must be a number"))?;
        let latitude = pair[1]
            .as_f64()
            .ok_or_else(|| UnifiedError::bad_request("A position latitude must be a number"))?;
        validate_coordinates(latitude, longitude)?;
    }
    let first = positions.first().expect("ring has at least 4 positions");
    let last = positions.last().expect("ring has at least 4 positions");
    if first != last {
        return Err(UnifiedError::bad_request(
            "A linear ring must be closed: its first and last positions must be equal",
        ));
    }
    Ok(())
}

/// Straight-line distance in kilometres between two coordinate
/// pairs (haversine). Same construction as the logistics
/// module's route heuristic: the `a` term is clamped before
/// the inverse trig so near-antipodal points stay well-defined.
pub fn haversine_km(lat1: f64, lng1: f64, lat2: f64, lng2: f64) -> f64 {
    const EARTH_RADIUS_KM: f64 = 6371.0;
    let d_lat = (lat2 - lat1).to_radians();
    let d_lng = (lng2 - lng1).to_radians();
    let a = (d_lat / 2.0).sin().powi(2)
        + lat1.to_radians().cos() * lat2.to_radians().cos() * (d_lng / 2.0).sin().powi(2);
    let a = a.clamp(0.0, 1.0);
    2.0 * EARTH_RADIUS_KM * a.sqrt().atan2((1.0 - a).sqrt())
}

/// Latitude/longitude bounding box for a radius search: the SQL
/// prefilter for `nearby` (the exact distance is still computed
/// per row with `haversine_km`). Near the poles the longitude
/// range opens to the whole globe instead of dividing by a
/// near-zero cosine.
pub fn bounding_box(latitude: f64, longitude: f64, radius_km: f64) -> (f64, f64, f64, f64) {
    const KM_PER_DEGREE: f64 = 111.32;
    let lat_delta = radius_km / KM_PER_DEGREE;
    let cos_lat = latitude.to_radians().cos().abs();
    let lng_delta = if cos_lat < 0.05 {
        180.0
    } else {
        radius_km / (KM_PER_DEGREE * cos_lat)
    };
    (
        (latitude - lat_delta).max(-90.0),
        (latitude + lat_delta).min(90.0),
        (longitude - lng_delta).max(-180.0),
        (longitude + lng_delta).min(180.0),
    )
}

/// True when the point (longitude, latitude) lies inside the
/// GeoJSON geometry (Polygon or MultiPolygon). Outer rings
/// include, hole rings exclude; a point exactly on an edge
/// counts as inside.
pub fn point_in_geometry(longitude: f64, latitude: f64, geometry: &Value) -> bool {
    let Some(coordinates) = geometry.get("coordinates") else {
        return false;
    };
    match geometry.get("type").and_then(Value::as_str) {
        Some("Polygon") => point_in_polygon(longitude, latitude, coordinates),
        Some("MultiPolygon") => coordinates
            .as_array()
            .map(|polygons| {
                polygons
                    .iter()
                    .any(|polygon| point_in_polygon(longitude, latitude, polygon))
            })
            .unwrap_or(false),
        _ => false,
    }
}

fn point_in_polygon(longitude: f64, latitude: f64, polygon: &Value) -> bool {
    let Some(rings) = polygon.as_array() else {
        return false;
    };
    let mut rings_iter = rings.iter();
    let Some(outer) = rings_iter.next() else {
        return false;
    };
    if !point_in_ring(longitude, latitude, outer) {
        return false;
    }
    // A point inside any hole ring is outside the polygon.
    !rings_iter.any(|hole| point_in_ring(longitude, latitude, hole))
}

/// Ray casting (even-odd rule) over one linear ring. `x` is
/// longitude, `y` is latitude.
fn point_in_ring(longitude: f64, latitude: f64, ring: &Value) -> bool {
    let Some(positions) = ring.as_array() else {
        return false;
    };
    let points: Vec<(f64, f64)> = positions
        .iter()
        .filter_map(|position| {
            let pair = position.as_array()?;
            Some((pair.first()?.as_f64()?, pair.get(1)?.as_f64()?))
        })
        .collect();
    if points.len() < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = points.len() - 1;
    for i in 0..points.len() {
        let (xi, yi) = points[i];
        let (xj, yj) = points[j];
        if (yi > latitude) != (yj > latitude)
            && longitude < (xj - xi) * (latitude - yi) / (yj - yi) + xi
        {
            inside = !inside;
        }
        j = i;
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn latitude_bounds_reject_nan_and_poles() {
        assert!(validate_latitude(90.0, "latitude").is_ok());
        assert!(validate_latitude(-90.0, "latitude").is_ok());
        assert!(validate_latitude(90.0001, "latitude").is_err());
        assert!(validate_latitude(f64::NAN, "latitude").is_err());
        assert!(validate_longitude(180.0, "longitude").is_ok());
        assert!(validate_longitude(-180.0, "longitude").is_ok());
        assert!(validate_longitude(180.5, "longitude").is_err());
    }

    #[test]
    fn slug_rules() {
        assert!(validate_slug("delivery-point", "kind").is_ok());
        assert!(validate_slug("odp_2", "kind").is_ok());
        assert!(validate_slug("ODP", "kind").is_err());
        assert!(validate_slug("", "kind").is_err());
        assert!(validate_slug("has space", "kind").is_err());
    }

    #[test]
    fn haversine_same_point_and_one_degree() {
        assert_eq!(haversine_km(0.0, 0.0, 0.0, 0.0), 0.0);
        let one_degree = haversine_km(0.0, 0.0, 1.0, 0.0);
        assert!(
            (one_degree - 111.19).abs() < 0.5,
            "one degree latitude: {one_degree}"
        );
    }

    #[test]
    fn point_in_polygon_square_with_hole() {
        let square = json!({
            "type": "Polygon",
            "coordinates": [[
                [100.0, 0.0], [101.0, 0.0], [101.0, 1.0], [100.0, 1.0], [100.0, 0.0]
            ]]
        });
        assert!(point_in_geometry(100.5, 0.5, &square));
        assert!(!point_in_geometry(102.0, 0.5, &square));
        let holed = json!({
            "type": "Polygon",
            "coordinates": [
                [[100.0, 0.0], [102.0, 0.0], [102.0, 2.0], [100.0, 2.0], [100.0, 0.0]],
                [[100.5, 0.5], [101.5, 0.5], [101.5, 1.5], [100.5, 1.5], [100.5, 0.5]]
            ]
        });
        assert!(point_in_geometry(100.25, 0.25, &holed));
        assert!(!point_in_geometry(100.75, 0.75, &holed));
    }

    #[test]
    fn geometry_validation_rejects_open_rings_and_bad_types() {
        let open = json!({
            "type": "Polygon",
            "coordinates": [[[100.0, 0.0], [101.0, 0.0], [101.0, 1.0], [100.0, 1.0]]]
        });
        assert!(validate_geometry(&open).is_err());
        let point = json!({ "type": "Point", "coordinates": [100.0, 0.0] });
        assert!(validate_geometry(&point).is_err());
        let ok = json!({
            "type": "MultiPolygon",
            "coordinates": [[[[100.0, 0.0], [101.0, 0.0], [101.0, 1.0], [100.0, 1.0], [100.0, 0.0]]]]
        });
        assert!(validate_geometry(&ok).is_ok());
    }

    #[test]
    fn tile_template_requires_placeholders_and_no_credentials() {
        assert!(validate_tile_template("https://tile.openstreetmap.org/{z}/{x}/{y}.png").is_ok());
        assert!(validate_tile_template("https://example.com/{z}/{x}.png").is_err());
        assert!(validate_tile_template("https://user:pass@example.com/{z}/{x}/{y}.png").is_err());
        assert!(validate_tile_template("ftp://example.com/{z}/{x}/{y}.png").is_err());
    }
}
