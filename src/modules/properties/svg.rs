// Properties — SVG rendering.

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
// SVG rendering
// ---------------------------------------------------------------------------

/// A projected plan: bounds in local metres plus the scale that
/// maps metres to SVG units.
pub(crate) struct PlanProjection {
    reference_latitude: f64,
    min_x: f64,
    min_y: f64,
    scale: f64,
    /// Height of the fitted plan in SVG units (y span * scale).
    fitted_height_svg: f64,
}

impl PlanProjection {
    fn fit(positions: &[(f64, f64)]) -> Option<Self> {
        if positions.is_empty() {
            return None;
        }
        let reference_latitude =
            positions.iter().map(|(_, lat)| lat).sum::<f64>() / positions.len() as f64;
        let mut min_x = f64::MAX;
        let mut max_x = f64::MIN;
        let mut min_y = f64::MAX;
        let mut max_y = f64::MIN;
        for (lng, lat) in positions {
            let (x, y) = project(*lng, *lat, reference_latitude);
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            min_y = min_y.min(y);
            max_y = max_y.max(y);
        }
        let width_m = (max_x - min_x).max(1.0);
        let height_m = (max_y - min_y).max(1.0);
        let scale = ((SVG_WIDTH - 2.0 * SVG_PADDING) / width_m)
            .min((SVG_HEIGHT - 2.0 * SVG_PADDING) / height_m);
        Some(Self {
            reference_latitude,
            min_x,
            min_y,
            scale,
            fitted_height_svg: height_m * scale,
        })
    }

    /// Project a lon/lat point to SVG coordinates (y flipped, so
    /// north is up).
    fn to_svg(&self, longitude: f64, latitude: f64) -> (f64, f64) {
        let (x, y) = project(longitude, latitude, self.reference_latitude);
        let svg_x = SVG_PADDING + (x - self.min_x) * self.scale;
        let svg_y = SVG_PADDING + self.fitted_height_svg - (y - self.min_y) * self.scale;
        (svg_x, svg_y)
    }
}

/// Build an SVG path `d` attribute from a ring of positions.
pub(crate) fn ring_path(projection: &PlanProjection, ring: &[(f64, f64)], close: bool) -> String {
    let mut out = String::new();
    for (index, (lng, lat)) in ring.iter().enumerate() {
        let (x, y) = projection.to_svg(*lng, *lat);
        if index == 0 {
            out.push_str(&format!("M{x:.2} {y:.2}"));
        } else {
            out.push_str(&format!(" L{x:.2} {y:.2}"));
        }
    }
    if close {
        out.push('Z');
    }
    out
}

/// Render one plan (boundary + blocks) as an SVG document.
/// Blocks are filled with their status colour and labelled with
/// their title; the plan boundary (when present) is a dashed
/// outline. All text is XML-escaped. Class hooks
/// (`site-plan`, `plan-boundary`, `block`, `block-<status>`,
/// `block-label`) let a print stylesheet restyle the output.
pub fn render_plan_svg(
    plan: &PropertyPlan,
    blocks: &[PropertyBlock],
    title: &str,
) -> UnifiedResult<String> {
    let mut positions: Vec<(f64, f64)> = Vec::new();
    if let Some(geometry) = &plan.geometry {
        positions.extend(geometry_positions(geometry));
    }
    for block in blocks {
        positions.extend(geometry_positions(&block.geometry));
    }
    let Some(projection) = PlanProjection::fit(&positions) else {
        return Err(UnifiedError::bad_request(
            "plan has no geometry to render: add a plan boundary or blocks first",
        ));
    };
    let mut out = String::new();
    out.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" class=\"site-plan\" viewBox=\"0 0 {SVG_WIDTH:.0} {SVG_HEIGHT:.0}\" role=\"img\" aria-label=\"{}\">\n",
        xml_escape(title)
    ));
    out.push_str(&format!(
        "<title>{}</title>\n",
        xml_escape(title)
    ));
    if let Some(geometry) = &plan.geometry {
        let kind = geometry
            .get("type")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let mut boundary_d = String::new();
        if kind == "Polygon" {
            if let Some(rings) = geometry.get("coordinates").and_then(Value::as_array) {
                for ring_value in rings {
                    if let Some(array) = ring_value.as_array() {
                        let ring: Vec<(f64, f64)> = array
                            .iter()
                            .filter_map(|raw| {
                                match (raw.get(0).and_then(Value::as_f64), raw.get(1).and_then(Value::as_f64)) {
                                    (Some(lng), Some(lat)) => Some((lng, lat)),
                                    _ => None,
                                }
                            })
                            .collect();
                        boundary_d.push_str(&ring_path(&projection, &ring, true));
                    }
                }
            }
        } else if kind == "MultiPolygon" {
            if let Some(polygons) = geometry.get("coordinates").and_then(Value::as_array) {
                for polygon in polygons {
                    if let Some(rings) = polygon.as_array() {
                        for ring_value in rings {
                            if let Some(array) = ring_value.as_array() {
                                let ring: Vec<(f64, f64)> = array
                                    .iter()
                                    .filter_map(|raw| {
                                        match (raw.get(0).and_then(Value::as_f64), raw.get(1).and_then(Value::as_f64)) {
                                            (Some(lng), Some(lat)) => Some((lng, lat)),
                                            _ => None,
                                        }
                                    })
                                    .collect();
                                boundary_d.push_str(&ring_path(&projection, &ring, true));
                            }
                        }
                    }
                }
            }
        }
        if !boundary_d.is_empty() {
            out.push_str(&format!(
                "<path class=\"plan-boundary\" d=\"{boundary_d}\" fill=\"none\" stroke=\"#111827\" stroke-width=\"2\" stroke-dasharray=\"8 4\" />\n"
            ));
        }
    }
    for block in blocks {
        let kind = block
            .geometry
            .get("type")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let status = block.status.as_str();
        let fill = block.status.svg_fill();
        let label = xml_escape(&block.title);
        let detail = xml_escape(&format!(
            "{} — {} — {} {}",
            block.title,
            status,
            format_money(block.price, &block.currency),
            block.currency
        ));
        match kind {
            "Polygon" | "MultiPolygon" => {
                let mut d = String::new();
                let mut label_at: Option<(f64, f64)> = None;
                if kind == "Polygon" {
                    if let Some(rings) = block.geometry.get("coordinates").and_then(Value::as_array) {
                        for ring_value in rings {
                            if let Some(array) = ring_value.as_array() {
                                let ring: Vec<(f64, f64)> = array
                                    .iter()
                                    .filter_map(|raw| {
                                        match (raw.get(0).and_then(Value::as_f64), raw.get(1).and_then(Value::as_f64)) {
                                            (Some(lng), Some(lat)) => Some((lng, lat)),
                                            _ => None,
                                        }
                                    })
                                    .collect();
                                d.push_str(&ring_path(&projection, &ring, true));
                            }
                        }
                    }
                } else if let Some(polygons) = block.geometry.get("coordinates").and_then(Value::as_array) {
                    for polygon in polygons {
                        if let Some(rings) = polygon.as_array() {
                            for ring_value in rings {
                                if let Some(array) = ring_value.as_array() {
                                    let ring: Vec<(f64, f64)> = array
                                        .iter()
                                        .filter_map(|raw| {
                                            match (raw.get(0).and_then(Value::as_f64), raw.get(1).and_then(Value::as_f64)) {
                                                (Some(lng), Some(lat)) => Some((lng, lat)),
                                                _ => None,
                                            }
                                        })
                                        .collect();
                                    d.push_str(&ring_path(&projection, &ring, true));
                                }
                            }
                        }
                    }
                }
                // Label at the stored centroid (computed at write
                // time); fall back to the first position.
                if let Some(validated) = validate_geometry(&block.geometry).ok().and_then(|v| v.centroid) {
                    label_at = Some(projection.to_svg(validated.0, validated.1));
                } else if let Some((lng, lat)) = geometry_positions(&block.geometry).into_iter().next() {
                    label_at = Some(projection.to_svg(lng, lat));
                }
                out.push_str(&format!(
                    "<path class=\"block block-{status}\" d=\"{d}\" fill=\"{fill}\" fill-opacity=\"0.55\" stroke=\"#111827\" stroke-width=\"1.5\"><title>{detail}</title></path>\n"
                ));
                if let Some((x, y)) = label_at {
                    out.push_str(&format!(
                        "<text class=\"block-label\" x=\"{x:.2}\" y=\"{y:.2}\" text-anchor=\"middle\" font-size=\"14\" fill=\"#111827\">{label}</text>\n"
                    ));
                }
            }
            "LineString" => {
                let positions = geometry_positions(&block.geometry);
                let d = ring_path(&projection, &positions, false);
                out.push_str(&format!(
                    "<path class=\"block block-{status} block-line\" d=\"{d}\" fill=\"none\" stroke=\"{fill}\" stroke-width=\"3\"><title>{detail}</title></path>\n"
                ));
                if let Some((lng, lat)) = positions.get(positions.len() / 2) {
                    let (x, y) = projection.to_svg(*lng, *lat);
                    out.push_str(&format!(
                        "<text class=\"block-label\" x=\"{x:.2}\" y=\"{y:.2}\" text-anchor=\"middle\" font-size=\"14\" fill=\"#111827\">{label}</text>\n"
                    ));
                }
            }
            _ => {}
        }
    }
    out.push_str("</svg>\n");
    Ok(out)
}

/// Format a minor-unit amount as major units with two decimals,
/// for display strings and the CSV export. Zero-decimal currencies
/// convert without dividing by 100 (see `minor_to_major`).
pub(crate) fn format_money(amount_minor: i64, currency: &str) -> String {
    format!("{:.2}", minor_to_major(amount_minor, currency))
}

/// Render the print page: HTML with the inline SVG, a per-block
/// list table, and a print stylesheet. `show_buyer` controls
/// whether the buyer column is included (admin print only; the
/// public print never shows buyer ids).
pub fn render_print_html(
    property: &Property,
    plan: &PropertyPlan,
    blocks: &[PropertyBlock],
    svg: &str,
    show_buyer: bool,
) -> String {
    let mut rows = String::new();
    for block in blocks {
        let area = block
            .area_sqm
            .map(|value| format!("{value:.2} m²"))
            .unwrap_or_else(|| "-".to_string());
        let length = block
            .length_m
            .map(|value| format!("{value:.2} m"))
            .unwrap_or_else(|| "-".to_string());
        rows.push_str("<tr>");
        rows.push_str(&format!("<td>{}</td>", xml_escape(&block.title)));
        rows.push_str(&format!("<td>{}</td>", xml_escape(block.status.as_str())));
        rows.push_str(&format!("<td>{area}</td>"));
        rows.push_str(&format!("<td>{length}</td>"));
        rows.push_str(&format!(
            "<td>{} {}</td>",
            format_money(block.price, &block.currency),
            xml_escape(&block.currency)
        ));
        if show_buyer {
            rows.push_str(&format!(
                "<td>{}</td>",
                xml_escape(block.assigned_customer_id.as_deref().unwrap_or("-"))
            ));
        }
        rows.push_str("</tr>\n");
    }
    let buyer_header = if show_buyer { "<th>Buyer</th>" } else { "" };
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\" />\n<title>{} — {}</title>\n<style>\nbody {{ font-family: sans-serif; color: #111827; margin: 24px; }}\nh1 {{ font-size: 22px; margin-bottom: 4px; }}\nh2 {{ font-size: 16px; margin: 24px 0 8px; }}\n.meta {{ color: #4b5563; font-size: 13px; margin-bottom: 16px; }}\n.site-plan {{ width: 100%; height: auto; border: 1px solid #d1d5db; }}\ntable {{ border-collapse: collapse; width: 100%; font-size: 13px; }}\nth, td {{ border: 1px solid #d1d5db; padding: 6px 8px; text-align: left; }}\nth {{ background: #f3f4f6; }}\n@media print {{\n  body {{ margin: 0; }}\n  .no-print {{ display: none; }}\n  .site-plan {{ border: none; }}\n}}\n</style>\n</head>\n<body>\n<h1>{} — {}</h1>\n<p class=\"meta\">Plan: {} &middot; {} block(s) &middot; Printed site plan</p>\n<div class=\"plan-svg\">{}</div>\n<h2>Blocks</h2>\n<table>\n<thead><tr><th>Title</th><th>Status</th><th>Area</th><th>Length</th><th>Price</th>{}</tr></thead>\n<tbody>\n{}\n</tbody>\n</table>\n<p class=\"no-print\"><button onclick=\"window.print()\">Print</button></p>\n</body>\n</html>\n",
        xml_escape(&property.title),
        xml_escape(&plan.name),
        xml_escape(&property.title),
        xml_escape(&plan.name),
        xml_escape(&plan.name),
        blocks.len(),
        svg,
        buyer_header,
        rows
    )
}

/// Export one plan's blocks as CSV (RFC 4180 escaping).
pub fn blocks_to_csv(blocks: &[PropertyBlock]) -> String {
    let mut out = String::from("id,title,status,area_sqm,length_m,price,currency,assigned_customer_id,order_id\n");
    for block in blocks {
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{},{}\n",
            block.id,
            csv_escape(&block.title),
            block.status.as_str(),
            block.area_sqm.map(|v| format!("{v:.4}")).unwrap_or_default(),
            block.length_m.map(|v| format!("{v:.4}")).unwrap_or_default(),
            format!("{:.4}", minor_to_major(block.price, &block.currency)),
            csv_escape(&block.currency),
            csv_escape(block.assigned_customer_id.as_deref().unwrap_or("")),
            csv_escape(block.order_id.as_deref().unwrap_or("")),
        ));
    }
    out
}
