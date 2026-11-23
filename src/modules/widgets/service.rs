// Widgets — layout service: built-in defaults and DB-backed load /
// save / reset.
//
// The database is the ONLY authority for stored layouts. Reads
// without a configured pool degrade to the built-in default (a
// read must never fail for a missing database); writes without a
// pool fail with 503, exactly like the dashboards' `db_pool`.
// There is no in-memory layout cache: every read hits the table, so
// a layout can never go stale in this process.

use chrono::{DateTime, Utc};
use diesel::pg::PgConnection;
use diesel::prelude::*;
use serde_json::{json, Value};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::db::schema::widget_layouts;
use unified_core::db::DbPool;
use unified_core::error::{UnifiedError, UnifiedResult};
use uuid::Uuid;

use super::registry::collect_widget_types;
use super::types::*;
use super::validation::validate_layout_items;

/// One stored layout row (migration 000129).
#[derive(Debug, Clone, Queryable, Insertable, Selectable)]
#[diesel(table_name = widget_layouts)]
#[diesel(check_for_backend(diesel::pg::Pg))]
struct WidgetLayoutRow {
    id: Uuid,
    context: String,
    scope_kind: String,
    scope_id: String,
    owner_id: String,
    items: Value,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

/// The database pool, or 503 when no database is configured.
fn db_pool(ctx: &AppContext) -> UnifiedResult<DbPool> {
    ctx.db
        .read()
        .expect("db lock poisoned")
        .clone()
        .ok_or_else(|| {
            UnifiedError::new(
                "Database is not configured: widget layouts need a database connection",
                503,
            )
            .with_metadata("kind", json!("database_not_configured"))
        })
}

/// Run blocking Diesel work off the async worker threads.
async fn run_blocking<F, T>(pool: DbPool, work: F) -> UnifiedResult<T>
where
    F: FnOnce(&mut PgConnection) -> UnifiedResult<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let mut connection = pool.get().map_err(|error| {
            UnifiedError::internal(format!("Cannot borrow a database connection: {error}"))
        })?;
        work(&mut connection)
    })
    .await
    .map_err(|error| UnifiedError::internal(format!("Database task failed: {error}")))?
}

/// The built-in default items for a zone, in display order. The
/// seeded zones keep their explicit defaults; any other zone starts
/// from the catalogue in order (first item pinned). A zone's default
/// is only the starting point — a user's own row, then the shared
/// row, win over it (see `load_layout`).
fn default_items(context: &str) -> Vec<WidgetItem> {
    let pinned = true;
    let plain = false;
    let item = |widget_id: &str, pinned: bool| WidgetItem {
        widget_id: widget_id.to_string(),
        pinned,
    };
    match context {
        CONTEXT_ADMIN_DASHBOARD => vec![
            item("stat-cards", pinned),
            item("market-strip", plain),
            item("announcements-ticker", plain),
            item("module-grid", plain),
            item("recent-activity", plain),
            item("system-status", plain),
        ],
        CONTEXT_TENANT_DASHBOARD => vec![
            item("market-strip", pinned),
            item("announcements-ticker", plain),
            item("module-grid", plain),
        ],
        CONTEXT_USER_DASHBOARD | CONTEXT_RESELLER_DASHBOARD => vec![
            item("stat-cards", pinned),
            item("market-strip", plain),
            item("announcements-ticker", plain),
            item("module-grid", plain),
            item("recent-activity", plain),
            item("system-status", plain),
        ],
        _ => vec![
            item("stat-cards", pinned),
            item("market-strip", plain),
            item("module-grid", plain),
            item("recent-activity", plain),
            item("system-status", plain),
            item("announcements-ticker", plain),
        ],
    }
}

/// The built-in default layout for a context (no row stored).
pub(crate) fn default_layout(
    context: &str,
    scope_kind: &str,
    scope_id: &str,
    owner_id: &str,
) -> WidgetLayout {
    WidgetLayout {
        context: context.to_string(),
        scope_kind: scope_kind.to_string(),
        scope_id: scope_id.to_string(),
        owner_id: owner_id.to_string(),
        items: default_items(context),
        is_default: true,
    }
}

/// Parse stored items back into a layout: drop ids the catalogue no
/// longer knows (a catalogue change must not break a stored layout),
/// keep the stored order and pinned flags.
fn parse_stored_items(stored: Value, context: &str, ctx: &AppContext) -> Vec<WidgetItem> {
    let catalogue = collect_widget_types(ctx);
    let items: Vec<WidgetItem> = serde_json::from_value(stored).unwrap_or_default();
    items
        .into_iter()
        .filter(|item| {
            catalogue
                .iter()
                .any(|definition| definition.id == item.widget_id && definition.for_context(context))
        })
        .take(MAX_LAYOUT_ITEMS)
        .collect()
}

/// Load one stored row by its full key.
async fn load_row(
    pool: DbPool,
    context: &str,
    scope_kind: &str,
    scope_id: &str,
    owner_id: &str,
) -> UnifiedResult<Option<WidgetLayoutRow>> {
    let owned = (
        context.to_string(),
        scope_kind.to_string(),
        scope_id.to_string(),
        owner_id.to_string(),
    );
    run_blocking(pool, move |connection| {
        widget_layouts::table
            .filter(widget_layouts::context.eq(&owned.0))
            .filter(widget_layouts::scope_kind.eq(&owned.1))
            .filter(widget_layouts::scope_id.eq(&owned.2))
            .filter(widget_layouts::owner_id.eq(&owned.3))
            .select(WidgetLayoutRow::as_select())
            .first::<WidgetLayoutRow>(connection)
            .optional()
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot load widget layout: {error}"))
            })
    })
    .await
}

fn layout_from_row(row: WidgetLayoutRow, ctx: &AppContext) -> WidgetLayout {
    WidgetLayout {
        context: row.context.clone(),
        scope_kind: row.scope_kind,
        scope_id: row.scope_id,
        owner_id: row.owner_id,
        items: parse_stored_items(row.items, &row.context, ctx),
        is_default: false,
    }
}

/// Load a layout: the user's own row first, then the shared (team)
/// default row for the zone, then the built-in default. A user
/// always starts from the shared default and diverges by saving —
/// saving never touches the shared row unless `shared` was asked
/// for. Without a database pool the built-in default is returned
/// (reads degrade, writes do not).
pub(crate) async fn load_layout(
    ctx: &Arc<AppContext>,
    context: &str,
    scope_kind: &str,
    scope_id: &str,
    owner_id: &str,
) -> UnifiedResult<WidgetLayout> {
    let pool = match db_pool(ctx) {
        Ok(pool) => pool,
        Err(_) => {
            return Ok(default_layout(context, scope_kind, scope_id, owner_id));
        }
    };
    if let Some(row) = load_row(
        pool.clone(),
        context,
        scope_kind,
        scope_id,
        owner_id,
    )
    .await?
    {
        return Ok(layout_from_row(row, ctx));
    }
    if owner_id != OWNER_DEFAULT {
        if let Some(row) = load_row(pool, context, scope_kind, scope_id, OWNER_DEFAULT).await? {
            return Ok(layout_from_row(row, ctx));
        }
    }
    Ok(default_layout(context, scope_kind, scope_id, owner_id))
}

/// Save a layout (pin / add / remove / reorder): validates the items
/// against the catalogue, then upserts the row. Emits
/// `widgets.layout_changed` afterwards.
pub(crate) async fn save_layout(
    ctx: &Arc<AppContext>,
    context: &str,
    scope_kind: &str,
    scope_id: &str,
    owner_id: &str,
    items: Vec<WidgetItem>,
) -> UnifiedResult<WidgetLayout> {
    let catalogue = collect_widget_types(ctx);
    let items = validate_layout_items(items, context, &catalogue)?;
    let pool = db_pool(ctx)?;
    let (context, scope_kind, scope_id, owner_id) = (
        context.to_string(),
        scope_kind.to_string(),
        scope_id.to_string(),
        owner_id.to_string(),
    );
    let stored = serde_json::to_value(&items)
        .map_err(|error| UnifiedError::internal(format!("Cannot encode widget layout: {error}")))?;
    let row = WidgetLayoutRow {
        id: Uuid::now_v7(),
        context: context.clone(),
        scope_kind: scope_kind.clone(),
        scope_id: scope_id.clone(),
        owner_id: owner_id.clone(),
        items: stored,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    run_blocking(pool, move |connection| {
        diesel::insert_into(widget_layouts::table)
            .values(&row)
            .on_conflict((
                widget_layouts::context,
                widget_layouts::scope_kind,
                widget_layouts::scope_id,
                widget_layouts::owner_id,
            ))
            .do_update()
            .set((
                widget_layouts::items.eq(&row.items),
                widget_layouts::updated_at.eq(Utc::now()),
            ))
            .execute(connection)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot save widget layout: {error}"))
            })?;
        Ok(())
    })
    .await?;
    ctx.hooks.emit(
        EVENT_LAYOUT_CHANGED,
        json!({
            "context": context,
            "scope_kind": scope_kind,
            "scope_id": scope_id,
            "owner_id": owner_id,
        }),
    );
    Ok(WidgetLayout {
        context,
        scope_kind,
        scope_id,
        owner_id,
        items,
        is_default: false,
    })
}

/// Reset a layout to its built-in default: deletes the stored row
/// (a missing row is already the default). Emits
/// `widgets.layout_changed` afterwards.
pub(crate) async fn reset_layout(
    ctx: &Arc<AppContext>,
    context: &str,
    scope_kind: &str,
    scope_id: &str,
    owner_id: &str,
) -> UnifiedResult<WidgetLayout> {
    if let Ok(pool) = db_pool(ctx) {
        let owned = (
            context.to_string(),
            scope_kind.to_string(),
            scope_id.to_string(),
            owner_id.to_string(),
        );
        run_blocking(pool, move |connection| {
            diesel::delete(
                widget_layouts::table
                    .filter(widget_layouts::context.eq(&owned.0))
                    .filter(widget_layouts::scope_kind.eq(&owned.1))
                    .filter(widget_layouts::scope_id.eq(&owned.2))
                    .filter(widget_layouts::owner_id.eq(&owned.3)),
            )
            .execute(connection)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot reset widget layout: {error}"))
            })?;
            Ok(())
        })
        .await?;
        ctx.hooks.emit(
            EVENT_LAYOUT_CHANGED,
            json!({
                "context": context,
                "scope_kind": scope_kind,
                "scope_id": scope_id,
                "owner_id": owner_id,
            }),
        );
    }
    Ok(default_layout(context, scope_kind, scope_id, owner_id))
}
