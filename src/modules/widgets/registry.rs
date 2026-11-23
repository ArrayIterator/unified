// Widgets — the catalogue: the built-in seed plus hook-registered
// definitions.
//
// The seed lists ONLY widgets whose data already exists in the
// running system (the real dashboard content: stat cards, market
// strip, module grid, recent activity, system status, announcements
// ticker). No widget here invents a data source. Every built-in is
// available in every zone (empty `contexts`); a page that opts into
// widgets gets the whole catalogue unless a definition restricts
// itself to named zones.
// Modules add types by answering `widgets.widget_types`; the render
// path never hard-codes a widget list.

use unified_core::context::AppContext;
use serde_json::Value;

use super::types::*;

/// The built-in catalogue, seeded from the real dashboard content.
pub(crate) fn builtin_definitions() -> Vec<WidgetDefinition> {
    vec![
        WidgetDefinition {
            id: "stat-cards".to_string(),
            title: "Stat cards".to_string(),
            description: "Key figures from the dashboard overview (users, revenue, orders, storage)."
                .to_string(),
            contexts: vec![],
            source_module: "dashboards".to_string(),
            data_source: "dashboard.section_requested:stat-cards".to_string(),
        },
        WidgetDefinition {
            id: "market-strip".to_string(),
            title: "Market strip".to_string(),
            description: "Gold, currency, and crypto snapshot from the market widgets."
                .to_string(),
            contexts: vec![],
            source_module: "dashboards".to_string(),
            data_source: "dashboard.section_requested:market-strip".to_string(),
        },
        WidgetDefinition {
            id: "module-grid".to_string(),
            title: "Module grid".to_string(),
            description: "The registered modules, with status and version.".to_string(),
            contexts: vec![],
            source_module: "extensions".to_string(),
            data_source: "dashboard.section_requested:module-grid".to_string(),
        },
        WidgetDefinition {
            id: "recent-activity".to_string(),
            title: "Recent activity".to_string(),
            description: "The latest invoices, orders, licenses, and tickets.".to_string(),
            contexts: vec![],
            source_module: "dashboards".to_string(),
            data_source: "dashboard.section_requested:recent-activity".to_string(),
        },
        WidgetDefinition {
            id: "system-status".to_string(),
            title: "System status".to_string(),
            description: "API, database, job queue, and uptime checks.".to_string(),
            contexts: vec![],
            source_module: "service-monitor".to_string(),
            data_source: "dashboard.section_requested:system-status".to_string(),
        },
        WidgetDefinition {
            id: "announcements-ticker".to_string(),
            title: "Announcements ticker".to_string(),
            description: "Breaking announcements, rotating through the latest items."
                .to_string(),
            contexts: vec![],
            source_module: "announcements".to_string(),
            data_source: "dashboard.section_requested:announcements-ticker".to_string(),
        },
    ]
}

/// A hook answer is valid only when it fits the definition shape:
/// a kebab-case id, a short title, at least one known context, and a
/// named source module. Invalid answers are skipped, never stored.
fn definition_from_value(value: &Value) -> Option<WidgetDefinition> {
    let id = value.get("id")?.as_str()?;
    if id.is_empty()
        || id.len() > 64
        || !id
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
        || !id.chars().next().is_some_and(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
    {
        return None;
    }
    let title = value.get("title")?.as_str()?;
    if title.is_empty() || title.len() > 120 {
        return None;
    }
    let description = value.get("description").and_then(|item| item.as_str()).unwrap_or("");
    if description.len() > 500 {
        return None;
    }
    // Empty or absent means every zone; a listed zone must be a
    // valid zone id (see `validation::is_valid_zone`). A present
    // `contexts` that is not an array is invalid, not "all zones".
    let mut parsed_contexts = Vec::new();
    if let Some(contexts_value) = value.get("contexts") {
        let contexts = contexts_value.as_array()?;
        for item in contexts {
            let context = item.as_str()?;
            if !super::validation::is_valid_zone(context) {
                return None;
            }
            if !parsed_contexts.contains(&context.to_string()) {
                parsed_contexts.push(context.to_string());
            }
        }
    }
    let source_module = value.get("source_module")?.as_str()?;
    if source_module.is_empty() || source_module.len() > 120 {
        return None;
    }
    let data_source = value
        .get("data_source")
        .and_then(|item| item.as_str())
        .unwrap_or("");
    if data_source.len() > 200 {
        return None;
    }
    Some(WidgetDefinition {
        id: id.to_string(),
        title: title.to_string(),
        description: description.to_string(),
        contexts: parsed_contexts,
        source_module: source_module.to_string(),
        data_source: data_source.to_string(),
    })
}

/// The full catalogue: the built-in seed first, then every valid
/// definition other modules contribute through the
/// `widgets.widget_types` hook. A hook definition whose id collides
/// with a built-in (or an earlier hook answer) is ignored, so the
/// built-ins always win and the catalogue is deterministic.
pub(crate) fn collect_widget_types(ctx: &AppContext) -> Vec<WidgetDefinition> {
    let mut definitions = builtin_definitions();
    let answers = ctx.hooks.emit(EVENT_WIDGET_TYPES, serde_json::json!({}));
    for result in &answers.results {
        let candidates: Vec<&Value> = match result {
            Value::Array(items) => items.iter().collect(),
            single => vec![single],
        };
        for candidate in candidates {
            if let Some(definition) = definition_from_value(candidate) {
                if !definitions.iter().any(|item| item.id == definition.id) {
                    definitions.push(definition);
                }
            }
        }
    }
    definitions
}
