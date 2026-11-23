// Diesel models for the routes domain of the unified-db-infra bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Named routes (2026-11-22-000021_routes).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = named_routes)]
pub struct NamedRoute {
    pub name: String,
    pub default_path: String,
    pub current_path: String,
    pub source: String,
    pub description: String,
    pub config_override: Option<String>,
    pub settings_override: Option<String>,
    pub previous_path: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = named_routes)]
pub struct NewNamedRoute {
    pub name: String,
    pub default_path: String,
    pub current_path: String,
    pub config_override: Option<String>,
    pub settings_override: Option<String>,
    pub previous_path: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = route_redirects)]
pub struct RouteRedirect {
    pub route: String,
    pub old_path: String,
    pub new_path: String,
    pub changed_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = route_redirects)]
pub struct NewRouteRedirect {
    pub route: String,
    pub old_path: String,
    pub new_path: String,
}
