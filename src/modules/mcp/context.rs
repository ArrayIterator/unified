//! mcp - context.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::{MAX_PAGE, PageResult, PaginationParams};
use unified_core::db::DbPool;
use crate::modules::ai::{assemble_shared_context, SiteContextInput};
use diesel::prelude::*;
use diesel::sql_types::{BigInt, Text};
use serde_json::{json, Value};
use super::protocol::*;
use super::catalog::*;
use super::state::*;
use super::validation::*;

/// One page of context from a source.
#[derive(Debug, Clone)]
pub(crate) struct ContextPage {
    pub(crate) items: Vec<Value>,
    pub(crate) total: u64,
    /// False when no module answered the source's hook event (or the
    /// database fallback was unavailable): the caller must say so
    /// instead of presenting an empty list as a complete answer.
    pub(crate) available: bool,
    pub(crate) gap: Option<String>,
}

/// Fetch one context source for a server.
///
/// The owning module is asked through `mcp.context.<source>` with
/// `{"server_id", "company_id", "site_id", "page", "per_page",
/// "search", "filters"}`. A handler answers by contributing
/// `{"items": [...], "total": n}` as a hook result; the first
/// well-formed result wins. When no handler answers and the source
/// has a database fallback table, the read-only query below runs
/// instead. When neither exists, the page is empty with
/// `available: false` and a gap note -- the module reports the gap
/// honestly rather than inventing context.
pub(crate) fn fetch_context(
    ctx: &AppContext,
    server: &McpServer,
    source: &str,
    params: &PaginationParams,
) -> ContextPage {
    let pagination = params.normalized();
    // MERGE POINT -- shared site context. The AI module owns the one
    // context assembler (`assemble_shared_context` in ai.rs); the MCP
    // module calls it directly for the shared `site` source instead
    // of building a second assembler or asking through a
    // `mcp.context.site` hook. `include_site_hooks` is true, so
    // modules answering `site.context_requested` still contribute
    // their site facts. The per-source `mcp.context.<source>` hook
    // assembly below stays for the MCP-specific sources only.
    if source == "site" {
        let query = pagination.search.clone().unwrap_or_default();
        let shared = assemble_shared_context(
            Some(&ctx.hooks),
            &query,
            &SiteContextInput::default(),
            true,
        );
        let item = scrub_value(json!(shared));
        ctx.hooks.emit(
            mcp_events::CONTEXT_SERVED,
            json!({
                "server_id": server.id,
                "source": source,
                "items": 1,
                "total": 1,
            }),
        );
        return ContextPage {
            items: vec![item],
            total: 1,
            available: true,
            gap: None,
        };
    }
    let event = format!("{}{}", mcp_events::CONTEXT_PREFIX, source);
    if ctx.hooks.has(&event) {
        let answer = ctx.hooks.emit(
            &event,
            json!({
                "server_id": server.id,
                "company_id": server.company_id,
                "site_id": server.site_id,
                "page": pagination.page,
                "per_page": pagination.per_page,
                "search": pagination.search,
                "filters": pagination.filters,
            }),
        );
        for result in &answer.results {
            if let Some(items) = result.get("items").and_then(Value::as_array) {
                let total = result
                    .get("total")
                    .and_then(Value::as_u64)
                    .unwrap_or(items.len() as u64);
                let items: Vec<Value> =
                    items.iter().cloned().map(scrub_value).collect();
                ctx.hooks.emit(
                    mcp_events::CONTEXT_SERVED,
                    json!({
                        "server_id": server.id,
                        "source": source,
                        "items": items.len(),
                        "total": total,
                    }),
                );
                return ContextPage {
                    items,
                    total,
                    available: true,
                    gap: None,
                };
            }
        }
    }
    if let Some(table_name) = find_source(source).and_then(|entry| entry.db_table) {
        if let Some(table) = find_table(table_name) {
            let search = pagination.search.clone().unwrap_or_default();
            match read_only_query_blocking(
                ctx,
                table,
                None,
                &search,
                pagination.page,
                pagination.per_page,
                server.read_only_role.clone(),
            ) {
                Ok(page) => {
                    return ContextPage {
                        items: page.items,
                        total: page.pagination.total,
                        available: true,
                        gap: None,
                    };
                }
                Err(error) => {
                    return ContextPage {
                        items: Vec::new(),
                        total: 0,
                        available: false,
                        gap: Some(format!(
                            "Database fallback for \"{source}\" failed: {}",
                            error.message
                        )),
                    };
                }
            }
        }
    }
    ContextPage {
        items: Vec::new(),
        total: 0,
        available: false,
        gap: Some(format!(
            "No module answers the \"{event}\" hook yet, so the \"{source}\" context source has no data. The owning module ({}) needs to register a handler that answers with {{\"items\": [...], \"total\": n}}.",
            find_source(source).map(|entry| entry.owner_module).unwrap_or("unknown")
        )),
    }
}

/// Parameters for one read-only query, already validated against the
/// allowlist. Only owned strings travel into the blocking task.
#[derive(Debug, Clone)]
pub(crate) struct ReadOnlyQuerySpec {
    table: String,
    columns: Vec<String>,
    search_column: String,
    search: String,
    page: u32,
    per_page: u32,
    read_only_role: Option<String>,
}

/// One row of `row_to_json(...)::text`.
#[derive(Debug, QueryableByName)]
pub(crate) struct RowJson {
    #[diesel(sql_type = Text)]
    row_json: String,
}

/// One row of `SELECT COUNT(*)`.
#[derive(Debug, QueryableByName)]
pub(crate) struct CountRow {
    #[diesel(sql_type = BigInt)]
    total: i64,
}

/// Run one validated read-only query inside a read-only transaction.
///
/// The SQL text is assembled only from allowlist constants (table and
/// column names) and fixed keywords. Every client-supplied value --
/// the search pattern, the limit, and the offset -- travels as a bound
/// Diesel parameter, never interpolated into the SQL string. The
/// transaction's first statement is `SET TRANSACTION READ ONLY`, so
/// PostgreSQL refuses any write even if a future code path tried one;
/// when an admin configured a read-only role, `SET LOCAL ROLE` runs
/// first (the role name was validated as a bare identifier at
/// configuration time and is quoted here).
pub(crate) fn run_read_only_query(
    pool: &DbPool,
    spec: &ReadOnlyQuerySpec,
) -> UnifiedResult<PageResult<Value>> {
    let mut connection = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot borrow a database connection: {error}"))
    })?;
    connection.transaction(|conn| {
        if let Some(role) = &spec.read_only_role {
            diesel::sql_query(format!("SET LOCAL ROLE \"{role}\"")).execute(conn)?;
        }
        diesel::sql_query("SET TRANSACTION READ ONLY").execute(conn)?;
        // Saturating math: a client-supplied page near u32::MAX must
        // not overflow the offset (panics in debug, wraps in
        // release); the offset saturates and the query then returns
        // an empty page.
        let offset = (i64::from(spec.page.saturating_sub(1)))
            .saturating_mul(i64::from(spec.per_page));
        let limit = spec.per_page as i64;
        let columns = spec
            .columns
            .iter()
            .map(|column| format!("\"{column}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let has_search = !spec.search.trim().is_empty();
        let search_pattern = format!("%{}%", spec.search.trim());
        // Total first, so pagination metadata is exact.
        let total: i64 = if has_search {
            let rows = diesel::sql_query(format!(
                "SELECT COUNT(*) AS total FROM \"{}\" WHERE \"{}\" ILIKE $1",
                spec.table, spec.search_column
            ))
            .bind::<Text, _>(search_pattern.clone())
            .load::<CountRow>(conn)?;
            rows.first().map(|row| row.total).unwrap_or(0)
        } else {
            let rows = diesel::sql_query(format!(
                "SELECT COUNT(*) AS total FROM \"{}\"",
                spec.table
            ))
            .load::<CountRow>(conn)?;
            rows.first().map(|row| row.total).unwrap_or(0)
        };
        // Rows as one JSON document each: a single generic shape works
        // for every allowlisted table, and no per-table model is needed.
        let rows: Vec<RowJson> = if has_search {
            diesel::sql_query(format!(
                "SELECT row_to_json(t)::text AS row_json FROM (SELECT {columns} FROM \"{}\" WHERE \"{}\" ILIKE $3 ORDER BY \"created_at\" DESC LIMIT $1 OFFSET $2) t",
                spec.table, spec.search_column
            ))
            .bind::<BigInt, _>(limit)
            .bind::<BigInt, _>(offset)
            .bind::<Text, _>(search_pattern)
            .load::<RowJson>(conn)?
        } else {
            diesel::sql_query(format!(
                "SELECT row_to_json(t)::text AS row_json FROM (SELECT {columns} FROM \"{}\" ORDER BY \"created_at\" DESC LIMIT $1 OFFSET $2) t",
                spec.table
            ))
            .bind::<BigInt, _>(limit)
            .bind::<BigInt, _>(offset)
            .load::<RowJson>(conn)?
        };
        let mut items: Vec<Value> = Vec::new();
        for row in rows {
            let parsed: Value = serde_json::from_str(&row.row_json).map_err(|error| {
                UnifiedError::internal(format!("Cannot parse database row as JSON: {error}"))
            })?;
            items.push(scrub_value(parsed));
        }
        let pagination = unified_core::pagination::Pagination {
            page: spec.page,
            per_page: spec.per_page,
            sort: None,
            order: "desc".to_string(),
            search: if spec.search.is_empty() {
                None
            } else {
                Some(spec.search.clone())
            },
            filters: Default::default(),
        };
        Ok(PageResult::new(items, total.max(0) as u64, &pagination))
    })
}

/// Validate requested columns for one allowlisted table: every
/// requested column must pass the denylist and be on the table's
/// allowlist. `None` or an empty list selects every allowlisted
/// column. Shared by the async and the blocking query paths, so no
/// caller can reach SQL with unvalidated columns: a `"` in a
/// client-supplied column name can no longer break out of the
/// quoted identifier in `run_read_only_query`.
pub(crate) fn validate_requested_columns(
    table: &ReadOnlyTable,
    columns: Option<Vec<String>>,
) -> UnifiedResult<Vec<String>> {
    let requested: Vec<String> = match columns {
        Some(columns) if !columns.is_empty() => columns,
        _ => table.columns.iter().map(|entry| entry.to_string()).collect(),
    };
    if requested.len() > table.columns.len() {
        return Err(UnifiedError::bad_request("Too many columns requested"));
    }
    for column in &requested {
        if column_is_denied(column) {
            return Err(UnifiedError::forbidden(format!(
                "Column \"{column}\" is denylisted and is never exposed by MCP"
            ))
            .with_metadata("column", json!(column)));
        }
        if !table.columns.contains(&column.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "Column \"{column}\" is not allowlisted for table \"{}\"",
                table.name
            ))
            .with_metadata("column", json!(column))
            .with_metadata("table", json!(table.name)));
        }
    }
    Ok(requested)
}

/// Validate a `db.query` request against the allowlist and run it.
/// Returns a 503 when no database pool is configured (the hook
/// context path keeps working without a database).
pub(crate) async fn read_only_query(
    ctx: &AppContext,
    table_name: &str,
    columns: Option<Vec<String>>,
    search: &str,
    page: u32,
    per_page: u32,
    read_only_role: Option<String>,
) -> UnifiedResult<PageResult<Value>> {
    let table = find_table(table_name).ok_or_else(|| {
        UnifiedError::bad_request(format!(
            "Table \"{table_name}\" is not in the MCP read-only allowlist"
        ))
        .with_metadata("table", json!(table_name))
        .with_metadata(
            "allowlisted_tables",
            json!(READ_ONLY_TABLES.iter().map(|entry| entry.name).collect::<Vec<_>>()),
        )
    })?;
    let requested = validate_requested_columns(table, columns)?;
    let pool = ctx
        .db
        .read()
        .expect("db lock poisoned")
        .clone()
        .ok_or_else(|| {
            UnifiedError::new(
                "No database pool is configured: the read-only database query path is unavailable (hook context still works)",
                503,
            )
            .with_metadata("reason", json!("database_unavailable"))
        })?;
    let spec = ReadOnlyQuerySpec {
        table: table.name.to_string(),
        columns: requested,
        search_column: table.search_column.to_string(),
        search: search.trim().to_string(),
        page: page.max(1).min(MAX_PAGE),
        per_page: per_page.clamp(1, 100),
        read_only_role,
    };
    // The blocking task owns the spec, so the pool stays Send.
    tokio::task::spawn_blocking(move || run_read_only_query(&pool, &spec))
        .await
        .map_err(|error| {
            UnifiedError::internal(format!("Read-only database task failed: {error}"))
        })?
}

/// Synchronous wrapper used by the hook fallback inside
/// `fetch_context` (which runs on a handler thread, not inside a
/// blocking task). Kept separate from the async `read_only_query` so
/// the fallback never blocks an Actix worker on a pool wait: it only
/// runs when a pool is already configured and fails fast otherwise.
pub(crate) fn read_only_query_blocking(
    ctx: &AppContext,
    table: &ReadOnlyTable,
    columns: Option<Vec<String>>,
    search: &str,
    page: u32,
    per_page: u32,
    read_only_role: Option<String>,
) -> UnifiedResult<PageResult<Value>> {
    let pool = ctx
        .db
        .read()
        .expect("db lock poisoned")
        .clone()
        .ok_or_else(|| {
            UnifiedError::new("No database pool is configured", 503)
                .with_metadata("reason", json!("database_unavailable"))
        })?;
    // Same allowlist + denylist validation as the async path: this
    // function is only called with `None` today, but a future caller
    // passing client-supplied columns must not reach SQL unchecked.
    let requested = validate_requested_columns(table, columns)?;
    let spec = ReadOnlyQuerySpec {
        table: table.name.to_string(),
        columns: requested,
        search_column: table.search_column.to_string(),
        search: search.trim().to_string(),
        page: page.max(1).min(MAX_PAGE),
        per_page: per_page.clamp(1, 100),
        read_only_role,
    };
    run_read_only_query(&pool, &spec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_requested_columns_defaults_to_all_table_columns() {
        let table = find_table("companies").unwrap();
        let all = validate_requested_columns(table, None).unwrap();
        assert_eq!(all, table.columns.iter().map(|entry| entry.to_string()).collect::<Vec<_>>());
        let all = validate_requested_columns(table, Some(vec![])).unwrap();
        assert_eq!(all.len(), table.columns.len());
    }

    #[test]
    fn validate_requested_columns_accepts_an_allowlisted_subset() {
        let table = find_table("store_products").unwrap();
        let subset = validate_requested_columns(
            table,
            Some(vec!["id".to_string(), "name".to_string(), "price".to_string()]),
        )
        .unwrap();
        assert_eq!(subset, vec!["id", "name", "price"]);
    }

    #[test]
    fn validate_requested_columns_rejects_denied_columns() {
        let table = find_table("companies").unwrap();
        // Denylisted by fragment, even though the name is not a real
        // column: the denylist check runs before the allowlist check.
        assert!(validate_requested_columns(table, Some(vec!["password_hash".to_string()])).is_err());
        assert!(validate_requested_columns(table, Some(vec!["encryption_key".to_string()])).is_err());
        assert!(validate_requested_columns(table, Some(vec!["secretkey".to_string()])).is_err());
    }

    #[test]
    fn validate_requested_columns_rejects_a_quote_injection_column() {
        // The blocking path used to interpolate columns straight into
        // SQL: a `"` in the name must be rejected here, never quoted.
        let table = find_table("companies").unwrap();
        assert!(validate_requested_columns(
            table,
            Some(vec!["id\" FROM companies; --".to_string()])
        )
        .is_err());
    }

    #[test]
    fn validate_requested_columns_rejects_unknown_and_too_many_columns() {
        let table = find_table("store_orders").unwrap();
        // `user_id` is a real column in the schema but deliberately
        // not allowlisted for this table.
        assert!(validate_requested_columns(table, Some(vec!["user_id".to_string()])).is_err());
        assert!(validate_requested_columns(table, Some(vec!["nope".to_string()])).is_err());
        let too_many: Vec<String> = (0..=table.columns.len())
            .map(|index| format!("col{index}"))
            .collect();
        assert!(validate_requested_columns(table, Some(too_many)).is_err());
    }
}
