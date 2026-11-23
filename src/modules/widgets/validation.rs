// Widgets — input validation: zones and layout items.
//
// A zone is any kebab-case page id — there is no zone registry, so
// a page opts into widgets by declaring its zone (see `module.rs`)
// without touching this module. A layout is valid only when every
// item names a widget that exists in the catalogue AND is allowed
// in the layout's zone. Unknown ids, wrong-zone ids, and duplicates
// are rejected, never stored.

use unified_core::error::{UnifiedError, UnifiedResult};

use super::types::*;

/// True when `value` is a usable zone id: kebab-case, at most 64
/// characters (lowercase letters, digits, single hyphens).
pub(crate) fn is_valid_zone(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
        && !value.starts_with('-')
        && !value.ends_with('-')
        && !value.contains("--")
}

/// Parse a zone id from a path segment or query value. Any
/// well-formed zone is accepted — zones are declared by the pages
/// that use them, not registered here.
pub(crate) fn validate_zone(raw: &str) -> UnifiedResult<&str> {
    if is_valid_zone(raw) {
        Ok(raw)
    } else {
        Err(UnifiedError::bad_request(format!(
            "Unknown widget zone \"{raw}\": expected a kebab-case zone id (lowercase letters, digits, hyphens, at most 64 characters)"
        )))
    }
}

/// Validate and normalize layout items for a zone: cap the size,
/// drop nothing silently — an unknown id, a wrong-zone id, or a
/// duplicate id is a 400, so a broken client cannot store a layout
/// the render path cannot honour. The returned order is the stored
/// order.
pub(crate) fn validate_layout_items(
    items: Vec<WidgetItem>,
    zone: &str,
    catalogue: &[WidgetDefinition],
) -> UnifiedResult<Vec<WidgetItem>> {
    if items.len() > MAX_LAYOUT_ITEMS {
        return Err(UnifiedError::bad_request(format!(
            "A layout holds at most {MAX_LAYOUT_ITEMS} widgets, got {}",
            items.len()
        )));
    }
    let mut seen: Vec<String> = Vec::new();
    for item in &items {
        let widget_id = item.widget_id.trim();
        if widget_id.is_empty() {
            return Err(UnifiedError::bad_request(
                "A layout item needs a widget_id",
            ));
        }
        if seen.iter().any(|id| id == widget_id) {
            return Err(UnifiedError::bad_request(format!(
                "Duplicate widget \"{widget_id}\" in layout"
            )));
        }
        seen.push(widget_id.to_string());
        match catalogue.iter().find(|definition| definition.id == widget_id) {
            None => {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown widget \"{widget_id}\""
                )));
            }
            Some(definition) if !definition.for_context(zone) => {
                return Err(UnifiedError::bad_request(format!(
                    "Widget \"{widget_id}\" is not available in zone \"{zone}\""
                )));
            }
            _ => {}
        }
    }
    Ok(items
        .into_iter()
        .map(|item| WidgetItem {
            widget_id: item.widget_id.trim().to_string(),
            pinned: item.pinned,
        })
        .collect())
}
