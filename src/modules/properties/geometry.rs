// Properties — GeoJSON validation, measurement, and hashing.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// GeoJSON validation, measurement, and hashing
// ---------------------------------------------------------------------------

/// What `validate_geometry` returns for a geometry that passed
/// every check: its kind, the server-computed measurements, and
/// the centroid (projected back to longitude / latitude) used for
/// the SVG label position.
#[derive(Debug, Clone)]
pub struct ValidatedGeometry {
    pub kind: String,
    pub area_sqm: Option<f64>,
    pub length_m: Option<f64>,
    pub centroid: Option<(f64, f64)>,
}

/// Validate a GeoJSON geometry object. Accepted types are
/// `Polygon`, `MultiPolygon`, and `LineString` only — no `Point`,
/// no `GeometryCollection`, no `Feature` wrapper (the geometry
/// object itself is what is stored). Every position must be
/// exactly `[longitude, latitude]` with finite numbers in range.
/// Polygons need at least one ring, each ring at least four
/// positions, and every ring must be closed (first position
/// exactly equal to the last). Lines need at least two positions.
/// The total position count is bounded so one geometry can never
/// allocate unbounded work.
pub fn validate_geometry(value: &Value) -> UnifiedResult<ValidatedGeometry> {
    let object = value.as_object().ok_or_else(|| {
        UnifiedError::bad_request("geometry must be a GeoJSON geometry object")
    })?;
    let kind = object
        .get("type")
        .and_then(|value| value.as_str())
        .ok_or_else(|| UnifiedError::bad_request("geometry.type is required"))?;
    let coordinates = object.get("coordinates").ok_or_else(|| {
        UnifiedError::bad_request("geometry.coordinates is required")
    })?;
    match kind {
        "Polygon" => validate_polygon(coordinates),
        "MultiPolygon" => validate_multi_polygon(coordinates),
        "LineString" => validate_line_string(coordinates),
        other => Err(UnifiedError::bad_request(format!(
            "unsupported geometry type \"{other}\": only Polygon, MultiPolygon, and LineString are accepted"
        ))),
    }
}

/// Read one position: exactly two finite numbers
/// `[longitude, latitude]` in range.
pub(crate) fn read_position(value: &Value) -> UnifiedResult<(f64, f64)> {
    let array = value.as_array().ok_or_else(|| {
        UnifiedError::bad_request("geometry positions must be arrays [longitude, latitude]")
    })?;
    if array.len() != 2 {
        return Err(UnifiedError::bad_request(
            "geometry positions must have exactly two numbers [longitude, latitude]",
        ));
    }
    let longitude = array[0].as_f64().ok_or_else(|| {
        UnifiedError::bad_request("geometry longitude must be a number")
    })?;
    let latitude = array[1].as_f64().ok_or_else(|| {
        UnifiedError::bad_request("geometry latitude must be a number")
    })?;
    if !longitude.is_finite() || !(-180.0..=180.0).contains(&longitude) {
        return Err(UnifiedError::bad_request(
            "geometry longitude must be between -180 and 180",
        ));
    }
    if !latitude.is_finite() || !(-90.0..=90.0).contains(&latitude) {
        return Err(UnifiedError::bad_request(
            "geometry latitude must be between -90 and 90",
        ));
    }
    Ok((longitude, latitude))
}

/// Read one ring: an array of positions, at least four, closed
/// (first position exactly equal to the last).
pub(crate) fn read_ring(value: &Value, position_count: &mut usize) -> UnifiedResult<Vec<(f64, f64)>> {
    let array = value.as_array().ok_or_else(|| {
        UnifiedError::bad_request("polygon rings must be arrays of positions")
    })?;
    if array.len() < 4 {
        return Err(UnifiedError::bad_request(
            "polygon rings must have at least 4 positions (closed ring)",
        ));
    }
    let mut positions = Vec::with_capacity(array.len());
    for raw in array {
        *position_count += 1;
        if *position_count > MAX_GEOMETRY_POSITIONS {
            return Err(UnifiedError::bad_request(format!(
                "geometry has more than {MAX_GEOMETRY_POSITIONS} positions"
            )));
        }
        positions.push(read_position(raw)?);
    }
    if positions.first() != positions.last() {
        return Err(UnifiedError::bad_request(
            "polygon rings must be closed (first position must equal last position)",
        ));
    }
    Ok(positions)
}

pub(crate) fn validate_polygon(coordinates: &Value) -> UnifiedResult<ValidatedGeometry> {
    let rings = coordinates.as_array().ok_or_else(|| {
        UnifiedError::bad_request("Polygon coordinates must be an array of rings")
    })?;
    if rings.is_empty() {
        return Err(UnifiedError::bad_request(
            "Polygon must have at least one ring",
        ));
    }
    if rings.len() > MAX_GEOMETRY_RINGS {
        return Err(UnifiedError::bad_request(format!(
            "Polygon has more than {MAX_GEOMETRY_RINGS} rings"
        )));
    }
    let mut count = 0usize;
    let mut parsed: Vec<Vec<(f64, f64)>> = Vec::with_capacity(rings.len());
    for ring in rings {
        parsed.push(read_ring(ring, &mut count)?);
    }
    let (area_sqm, centroid) = polygon_measurements(&parsed);
    Ok(ValidatedGeometry {
        kind: "Polygon".to_string(),
        area_sqm: Some(area_sqm),
        length_m: None,
        centroid,
    })
}

pub(crate) fn validate_multi_polygon(coordinates: &Value) -> UnifiedResult<ValidatedGeometry> {
    let polygons = coordinates.as_array().ok_or_else(|| {
        UnifiedError::bad_request("MultiPolygon coordinates must be an array of polygons")
    })?;
    if polygons.is_empty() {
        return Err(UnifiedError::bad_request(
            "MultiPolygon must have at least one polygon",
        ));
    }
    let mut count = 0usize;
    let mut total_area = 0.0f64;
    let mut centroid: Option<(f64, f64)> = None;
    let mut largest_area = 0.0f64;
    for polygon in polygons {
        let rings = polygon.as_array().ok_or_else(|| {
            UnifiedError::bad_request("MultiPolygon polygons must be arrays of rings")
        })?;
        if rings.is_empty() {
            return Err(UnifiedError::bad_request(
                "MultiPolygon polygons must have at least one ring",
            ));
        }
        if rings.len() > MAX_GEOMETRY_RINGS {
            return Err(UnifiedError::bad_request(format!(
                "MultiPolygon polygon has more than {MAX_GEOMETRY_RINGS} rings"
            )));
        }
        let mut parsed: Vec<Vec<(f64, f64)>> = Vec::with_capacity(rings.len());
        for ring in rings {
            parsed.push(read_ring(ring, &mut count)?);
        }
        let (area, ring_centroid) = polygon_measurements(&parsed);
        total_area += area;
        if area > largest_area {
            largest_area = area;
            centroid = ring_centroid;
        }
    }
    Ok(ValidatedGeometry {
        kind: "MultiPolygon".to_string(),
        area_sqm: Some(total_area),
        length_m: None,
        centroid,
    })
}

pub(crate) fn validate_line_string(coordinates: &Value) -> UnifiedResult<ValidatedGeometry> {
    let array = coordinates.as_array().ok_or_else(|| {
        UnifiedError::bad_request("LineString coordinates must be an array of positions")
    })?;
    if array.len() < 2 {
        return Err(UnifiedError::bad_request(
            "LineString must have at least 2 positions",
        ));
    }
    if array.len() > MAX_GEOMETRY_POSITIONS {
        return Err(UnifiedError::bad_request(format!(
            "geometry has more than {MAX_GEOMETRY_POSITIONS} positions"
        )));
    }
    let mut positions = Vec::with_capacity(array.len());
    for raw in array {
        positions.push(read_position(raw)?);
    }
    let length_m = line_length_m(&positions);
    let midpoint = positions.get(positions.len() / 2).copied();
    Ok(ValidatedGeometry {
        kind: "LineString".to_string(),
        area_sqm: None,
        length_m: Some(length_m),
        centroid: midpoint,
    })
}

/// Project longitude / latitude to local metres with an
/// equirectangular projection around `reference_latitude`.
/// This is an approximation suited to site plans (a few hundred
/// metres across); it is not a survey-grade geodesic measurement.
pub(crate) fn project(longitude: f64, latitude: f64, reference_latitude: f64) -> (f64, f64) {
    let x = longitude * METRES_PER_DEGREE_LNG_AT_EQUATOR * reference_latitude.to_radians().cos();
    let y = latitude * METRES_PER_DEGREE_LAT;
    (x, y)
}

pub(crate) fn mean_latitude(rings: &[Vec<(f64, f64)>]) -> f64 {
    let mut sum = 0.0f64;
    let mut count = 0usize;
    for ring in rings {
        for (_, latitude) in ring {
            sum += latitude;
            count += 1;
        }
    }
    if count == 0 {
        0.0
    } else {
        sum / count as f64
    }
}

/// Shoelace area and centroid of one ring, in projected metres.
/// Returns (signed area, centroid in projected metres).
pub(crate) fn ring_area_centroid(ring: &[(f64, f64)], reference_latitude: f64) -> (f64, (f64, f64)) {
    let mut cross_sum = 0.0f64;
    let mut cx = 0.0f64;
    let mut cy = 0.0f64;
    for window in ring.windows(2) {
        let (x1, y1) = project(window[0].0, window[0].1, reference_latitude);
        let (x2, y2) = project(window[1].0, window[1].1, reference_latitude);
        let cross = x1 * y2 - x2 * y1;
        cross_sum += cross;
        cx += (x1 + x2) * cross;
        cy += (y1 + y2) * cross;
    }
    let signed_area = cross_sum / 2.0;
    if signed_area.abs() < f64::EPSILON {
        // Degenerate ring (zero area): fall back to the first vertex.
        let (x, y) = project(ring[0].0, ring[0].1, reference_latitude);
        return (0.0, (x, y));
    }
    (signed_area, (cx / (6.0 * signed_area), cy / (6.0 * signed_area)))
}

/// Area of a polygon (outer ring minus holes) and the centroid of
/// its outer ring, converted back to longitude / latitude.
pub(crate) fn polygon_measurements(rings: &[Vec<(f64, f64)>]) -> (f64, Option<(f64, f64)>) {
    let reference_latitude = mean_latitude(rings);
    let (outer_area, outer_centroid) = ring_area_centroid(&rings[0], reference_latitude);
    let mut area = outer_area.abs();
    for hole in &rings[1..] {
        let (hole_area, _) = ring_area_centroid(hole, reference_latitude);
        area -= hole_area.abs();
    }
    let scale_x = METRES_PER_DEGREE_LNG_AT_EQUATOR * reference_latitude.to_radians().cos();
    let centroid = if scale_x.abs() < f64::EPSILON {
        None
    } else {
        Some((
            outer_centroid.0 / scale_x,
            outer_centroid.1 / METRES_PER_DEGREE_LAT,
        ))
    };
    (area.max(0.0), centroid)
}

pub(crate) fn line_length_m(positions: &[(f64, f64)]) -> f64 {
    let reference_latitude = positions.iter().map(|(_, lat)| lat).sum::<f64>() / positions.len() as f64;
    let mut total = 0.0f64;
    for window in positions.windows(2) {
        let (x1, y1) = project(window[0].0, window[0].1, reference_latitude);
        let (x2, y2) = project(window[1].0, window[1].1, reference_latitude);
        total += ((x2 - x1).powi(2) + (y2 - y1).powi(2)).sqrt();
    }
    total
}

/// SHA-256 hex of the canonical JSON of a geometry value.
/// `serde_json::Value` serializes object keys in sorted order
/// (the default `Map` is a `BTreeMap`), so the same geometry
/// always hashes the same way regardless of key order in the
/// request body.
pub(crate) fn geometry_hash(value: &Value) -> String {
    let canonical = serde_json::to_string(value).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Extract every position of a geometry (any accepted type) as a
/// flat list, for bounds computation. Returns an empty list for a
/// malformed value (callers validate first).
pub(crate) fn geometry_positions(value: &Value) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    let Some(kind) = value.get("type").and_then(|v| v.as_str()) else {
        return out;
    };
    let Some(coordinates) = value.get("coordinates") else {
        return out;
    };
    match kind {
        "LineString" => {
            if let Some(array) = coordinates.as_array() {
                for raw in array {
                    if let (Some(lng), Some(lat)) = (raw.get(0).and_then(Value::as_f64), raw.get(1).and_then(Value::as_f64)) {
                        out.push((lng, lat));
                    }
                }
            }
        }
        "Polygon" => {
            if let Some(rings) = coordinates.as_array() {
                for ring in rings {
                    if let Some(array) = ring.as_array() {
                        for raw in array {
                            if let (Some(lng), Some(lat)) = (raw.get(0).and_then(Value::as_f64), raw.get(1).and_then(Value::as_f64)) {
                                out.push((lng, lat));
                            }
                        }
                    }
                }
            }
        }
        "MultiPolygon" => {
            if let Some(polygons) = coordinates.as_array() {
                for polygon in polygons {
                    if let Some(rings) = polygon.as_array() {
                        for ring in rings {
                            if let Some(array) = ring.as_array() {
                                for raw in array {
                                    if let (Some(lng), Some(lat)) = (raw.get(0).and_then(Value::as_f64), raw.get(1).and_then(Value::as_f64)) {
                                        out.push((lng, lat));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square() -> serde_json::Value {
        serde_json::json!({
            "type": "Polygon",
            "coordinates": [[[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]]]
        })
    }

    #[test]
    fn validate_geometry_accepts_closed_polygon() {
        let checked = validate_geometry(&square()).unwrap();
        assert_eq!(checked.kind, "Polygon");
        assert!(checked.area_sqm.unwrap() > 0.0);
        assert!(checked.length_m.is_none());
        assert!(checked.centroid.is_some());
    }

    #[test]
    fn validate_geometry_accepts_line_string() {
        let value = serde_json::json!({
            "type": "LineString",
            "coordinates": [[0.0, 0.0], [0.0, 1.0]]
        });
        let checked = validate_geometry(&value).unwrap();
        assert_eq!(checked.kind, "LineString");
        assert!(checked.length_m.unwrap() > 0.0);
        assert!(checked.area_sqm.is_none());
    }

    #[test]
    fn validate_geometry_accepts_multi_polygon() {
        let value = serde_json::json!({
            "type": "MultiPolygon",
            "coordinates": [[[[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]]]]
        });
        let checked = validate_geometry(&value).unwrap();
        assert_eq!(checked.kind, "MultiPolygon");
        assert!(checked.area_sqm.unwrap() > 0.0);
    }

    #[test]
    fn validate_geometry_rejects_point() {
        let value = serde_json::json!({
            "type": "Point",
            "coordinates": [0.0, 0.0]
        });
        assert!(validate_geometry(&value).is_err());
    }

    #[test]
    fn validate_geometry_rejects_unclosed_ring() {
        let value = serde_json::json!({
            "type": "Polygon",
            "coordinates": [[[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]]]
        });
        assert!(validate_geometry(&value).is_err());
    }

    #[test]
    fn validate_geometry_rejects_out_of_range_longitude() {
        let value = serde_json::json!({
            "type": "LineString",
            "coordinates": [[200.0, 0.0], [0.0, 1.0]]
        });
        assert!(validate_geometry(&value).is_err());
    }

    #[test]
    fn validate_geometry_rejects_non_object() {
        assert!(validate_geometry(&serde_json::json!("nope")).is_err());
    }
}
