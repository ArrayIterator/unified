// routes module — Request types.
//
// Split from the former single-file module `src/modules/routes.rs`.
// The module root (`routes.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::routes::{
    OverrideSource, RedirectEntry, RouteChange, RouteEntry, RouteRegistry,
    ROUTE_CHANGED_EVENT,
};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::{Arc, RwLock};

// ---------------------------------------------------------------------------
// Request types.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct SetOverrideInput {
    pub(crate) path: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ClearOverrideQuery {
    /// Which override layer to clear: "settings" (default) or "config".
    #[serde(default)]
    pub(crate) source: Option<String>,
}

pub(crate) fn parse_source(raw: Option<String>, default: OverrideSource) -> UnifiedResult<OverrideSource> {
    match raw.as_deref() {
        None => Ok(default),
        Some("settings") => Ok(OverrideSource::Settings),
        Some("config") => Ok(OverrideSource::Config),
        Some(other) => Err(UnifiedError::bad_request(format!(
            "source must be \"settings\" or \"config\", got \"{other}\""
        ))),
    }
}

/// Announce an override change on the hook bus. The payload is the
/// audit record: actor, route name, old path, new path, source.
pub(crate) fn emit_route_changed(ctx: &AppContext, change: &RouteChange, actor: &str) {
    ctx.hooks.emit(
        ROUTE_CHANGED_EVENT,
        json!({
            "route": change.route,
            "old_path": change.old_path,
            "new_path": change.new_path,
            "source": change.source.as_str(),
            "actor": actor,
            "changed_at": change.changed_at,
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_override_input_ignores_a_body_supplied_actor() {
        // The audit actor comes from the authenticated caller, never
        // from the request body: a supplied "actor" field is ignored.
        let input: SetOverrideInput =
            serde_json::from_str(r#"{"path": "/app", "actor": "mallory"}"#).unwrap();
        assert_eq!(input.path, "/app");
        let input: SetOverrideInput =
            serde_json::from_str(r#"{"path": "/app"}"#).unwrap();
        assert_eq!(input.path, "/app");
    }

    #[test]
    fn parse_source_accepts_known_layers() {
        assert_eq!(
            parse_source(None, OverrideSource::Config).unwrap(),
            OverrideSource::Config
        );
        assert_eq!(
            parse_source(Some("settings".to_string()), OverrideSource::Config).unwrap(),
            OverrideSource::Settings
        );
        assert_eq!(
            parse_source(Some("config".to_string()), OverrideSource::Settings).unwrap(),
            OverrideSource::Config
        );
    }

    #[test]
    fn parse_source_rejects_unknown_layers() {
        assert!(parse_source(Some("bogus".to_string()), OverrideSource::Settings).is_err());
        assert!(parse_source(Some("".to_string()), OverrideSource::Settings).is_err());
    }
}
