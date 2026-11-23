//! In-memory stores and Diesel persistence for tool records.

use unified_core::context::{AppContext};
use unified_core::error::{UnifiedError, UnifiedResult};
use crate::modules::geo;
use dashmap::DashMap;
use diesel::prelude::*;
use std::sync::{Arc, OnceLock};
use std::time::{Instant};
use uuid::Uuid;
use super::types::{NetworkToolCertWatch, NetworkToolRun, NetworkToolSavedCheck, NetworkToolUptimeEvent, NetworkToolUptimeMonitor};

// ---------------------------------------------------------------------------
// In-memory stores (authoritative until the integrator lands the
// schema.rs / models.rs additions; the Diesel helpers below mirror
// them 1:1 and take over as the read path once a pool is attached).
// ---------------------------------------------------------------------------

pub(crate) static SAVED_CHECKS: OnceLock<DashMap<Uuid, NetworkToolSavedCheck>> = OnceLock::new();

pub(crate) static RUNS: OnceLock<DashMap<Uuid, NetworkToolRun>> = OnceLock::new();

pub(crate) static UPTIME_MONITORS: OnceLock<DashMap<Uuid, NetworkToolUptimeMonitor>> = OnceLock::new();

pub(crate) static UPTIME_EVENTS: OnceLock<DashMap<Uuid, NetworkToolUptimeEvent>> = OnceLock::new();

pub(crate) static CERT_WATCHES: OnceLock<DashMap<Uuid, NetworkToolCertWatch>> = OnceLock::new();

/// Rate-limit buckets: key `actor:scope` -> (window start, hits).
pub(crate) static RATE_LIMITS: OnceLock<DashMap<String, (Instant, u64)>> = OnceLock::new();

pub(crate) static NETWORK_TOOLS_CTX: OnceLock<Arc<AppContext>> = OnceLock::new();

pub(crate) fn saved_checks_store() -> &'static DashMap<Uuid, NetworkToolSavedCheck> {
    SAVED_CHECKS.get_or_init(DashMap::new)
}

pub(crate) fn runs_store() -> &'static DashMap<Uuid, NetworkToolRun> {
    RUNS.get_or_init(DashMap::new)
}

pub(crate) fn uptime_monitors_store() -> &'static DashMap<Uuid, NetworkToolUptimeMonitor> {
    UPTIME_MONITORS.get_or_init(DashMap::new)
}

pub(crate) fn uptime_events_store() -> &'static DashMap<Uuid, NetworkToolUptimeEvent> {
    UPTIME_EVENTS.get_or_init(DashMap::new)
}

pub(crate) fn cert_watches_store() -> &'static DashMap<Uuid, NetworkToolCertWatch> {
    CERT_WATCHES.get_or_init(DashMap::new)
}

pub(crate) fn rate_limits_store() -> &'static DashMap<String, (Instant, u64)> {
    RATE_LIMITS.get_or_init(DashMap::new)
}

pub(crate) fn stash_ctx(ctx: &AppContext) -> Arc<AppContext> {
    // Same approach as the licensing module: rebuild an AppContext
    // from the shared Arcs, so synchronous hook handlers can reach
    // the hook bus, the database pool, and the TLS storage. Only the
    // uptime and maintenance flags are snapshots.
    Arc::new(AppContext {
        config: ctx.config.clone(),
        hooks: ctx.hooks.clone(),
        modules: ctx.modules.clone(),
        cache: ctx.cache.clone(),
        geo: ctx.geo.clone(),
        db: ctx.db.clone(),
        storage_registry: ctx.storage_registry.clone(),
        routes: ctx.routes.clone(),
        tls: ctx.tls.clone(),
        start_time: std::time::Instant::now(),
        paused: std::sync::atomic::AtomicBool::new(ctx.is_paused()),
        maintenance: std::sync::atomic::AtomicBool::new(ctx.is_maintenance()),
    })
}

pub(crate) fn hook_ctx() -> Option<Arc<AppContext>> {
    NETWORK_TOOLS_CTX.get().cloned()
}

pub(crate) fn db_pool(ctx: &AppContext) -> Option<unified_core::db::DbPool> {
    ctx.db.read().expect("db lock poisoned").clone()
}

// ---------------------------------------------------------------------------
// Diesel helpers (persistence path; in-memory stores above stay the
// read path until the integrator adds the matching `table!` blocks
// to crates/unified-core/src/db/schema.rs` - see the delivery report).
// ---------------------------------------------------------------------------

pub(crate) fn db_insert_run(pool: &unified_core::db::DbPool, run: &NetworkToolRun) -> UnifiedResult<()> {
    use unified_core::db::schema::network_tool_runs::dsl as runs_dsl;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    diesel::insert_into(runs_dsl::network_tool_runs)
        .values(run)
        .execute(&mut conn)
        .map_err(|error| UnifiedError::internal(format!("Cannot store tool run: {error}")))?;
    Ok(())
}

pub(crate) fn db_load_runs(pool: &unified_core::db::DbPool) -> UnifiedResult<Vec<NetworkToolRun>> {
    use unified_core::db::schema::network_tool_runs::dsl as runs_dsl;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    runs_dsl::network_tool_runs
        .select(NetworkToolRun::as_select())
        .order(runs_dsl::created_at.desc())
        .load(&mut conn)
        .map_err(|error| UnifiedError::internal(format!("Cannot load tool runs: {error}")))
}

pub(crate) fn db_load_saved_checks(pool: &unified_core::db::DbPool) -> UnifiedResult<Vec<NetworkToolSavedCheck>> {
    use unified_core::db::schema::network_tool_saved_checks::dsl as checks_dsl;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    checks_dsl::network_tool_saved_checks
        .select(NetworkToolSavedCheck::as_select())
        .order(checks_dsl::created_at.desc())
        .load(&mut conn)
        .map_err(|error| {
            UnifiedError::internal(format!("Cannot load saved checks: {error}"))
        })
}

pub(crate) fn db_insert_saved_check(
    pool: &unified_core::db::DbPool,
    check: &NetworkToolSavedCheck,
) -> UnifiedResult<()> {
    use unified_core::db::schema::network_tool_saved_checks::dsl as checks_dsl;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    diesel::insert_into(checks_dsl::network_tool_saved_checks)
        .values(check)
        .execute(&mut conn)
        .map_err(|error| {
            UnifiedError::internal(format!("Cannot store saved check: {error}"))
        })?;
    Ok(())
}

pub(crate) fn db_upsert_saved_check(
    pool: &unified_core::db::DbPool,
    check: &NetworkToolSavedCheck,
) -> UnifiedResult<()> {
    use unified_core::db::schema::network_tool_saved_checks::dsl as checks_dsl;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    diesel::insert_into(checks_dsl::network_tool_saved_checks)
        .values(check)
        .on_conflict(checks_dsl::id)
        .do_update()
        .set((
            checks_dsl::name.eq(&check.name),
            checks_dsl::tool.eq(&check.tool),
            checks_dsl::target.eq(&check.target),
            checks_dsl::parameters.eq(&check.parameters),
            checks_dsl::updated_at.eq(check.updated_at),
        ))
        .execute(&mut conn)
        .map_err(|error| {
            UnifiedError::internal(format!("Cannot store saved check: {error}"))
        })?;
    Ok(())
}

pub(crate) fn db_delete_saved_check(pool: &unified_core::db::DbPool, id: Uuid) -> UnifiedResult<()> {
    use unified_core::db::schema::network_tool_saved_checks::dsl as checks_dsl;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    diesel::delete(checks_dsl::network_tool_saved_checks.filter(checks_dsl::id.eq(id)))
        .execute(&mut conn)
        .map_err(|error| {
            UnifiedError::internal(format!("Cannot delete saved check: {error}"))
        })?;
    Ok(())
}

pub(crate) fn db_load_monitors(pool: &unified_core::db::DbPool) -> UnifiedResult<Vec<NetworkToolUptimeMonitor>> {
    use unified_core::db::schema::network_tool_uptime_monitors::dsl as mon_dsl;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    mon_dsl::network_tool_uptime_monitors
        .select(NetworkToolUptimeMonitor::as_select())
        .order(mon_dsl::created_at.desc())
        .load(&mut conn)
        .map_err(|error| {
            UnifiedError::internal(format!("Cannot load uptime monitors: {error}"))
        })
}

pub(crate) fn db_upsert_monitor(
    pool: &unified_core::db::DbPool,
    monitor: &NetworkToolUptimeMonitor,
) -> UnifiedResult<()> {
    use unified_core::db::schema::network_tool_uptime_monitors::dsl as mon_dsl;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    diesel::insert_into(mon_dsl::network_tool_uptime_monitors)
        .values(monitor)
        .on_conflict(mon_dsl::id)
        .do_update()
        .set((
            mon_dsl::name.eq(&monitor.name),
            mon_dsl::url.eq(&monitor.url),
            mon_dsl::interval_seconds.eq(monitor.interval_seconds),
            mon_dsl::expected_status.eq(monitor.expected_status),
            mon_dsl::failure_threshold.eq(monitor.failure_threshold),
            mon_dsl::consecutive_failures.eq(monitor.consecutive_failures),
            mon_dsl::enabled.eq(monitor.enabled),
            mon_dsl::last_status.eq(&monitor.last_status),
            mon_dsl::last_checked_at.eq(monitor.last_checked_at),
            mon_dsl::updated_at.eq(monitor.updated_at),
        ))
        .execute(&mut conn)
        .map_err(|error| {
            UnifiedError::internal(format!("Cannot store uptime monitor: {error}"))
        })?;
    Ok(())
}

pub(crate) fn db_delete_monitor(pool: &unified_core::db::DbPool, id: Uuid) -> UnifiedResult<()> {
    use unified_core::db::schema::network_tool_uptime_monitors::dsl as mon_dsl;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    diesel::delete(mon_dsl::network_tool_uptime_monitors.filter(mon_dsl::id.eq(id)))
        .execute(&mut conn)
        .map_err(|error| {
            UnifiedError::internal(format!("Cannot delete uptime monitor: {error}"))
        })?;
    Ok(())
}

pub(crate) fn db_insert_uptime_event(
    pool: &unified_core::db::DbPool,
    event: &NetworkToolUptimeEvent,
) -> UnifiedResult<()> {
    use unified_core::db::schema::network_tool_uptime_events::dsl as ev_dsl;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    diesel::insert_into(ev_dsl::network_tool_uptime_events)
        .values(event)
        .execute(&mut conn)
        .map_err(|error| {
            UnifiedError::internal(format!("Cannot store uptime event: {error}"))
        })?;
    Ok(())
}

pub(crate) fn db_load_cert_watches(pool: &unified_core::db::DbPool) -> UnifiedResult<Vec<NetworkToolCertWatch>> {
    use unified_core::db::schema::network_tool_cert_watches::dsl as watch_dsl;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    watch_dsl::network_tool_cert_watches
        .select(NetworkToolCertWatch::as_select())
        .order(watch_dsl::created_at.desc())
        .load(&mut conn)
        .map_err(|error| {
            UnifiedError::internal(format!("Cannot load certificate watches: {error}"))
        })
}

pub(crate) fn db_upsert_cert_watch(pool: &unified_core::db::DbPool, watch: &NetworkToolCertWatch) -> UnifiedResult<()> {
    use unified_core::db::schema::network_tool_cert_watches::dsl as watch_dsl;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    diesel::insert_into(watch_dsl::network_tool_cert_watches)
        .values(watch)
        .on_conflict(watch_dsl::id)
        .do_update()
        .set((
            watch_dsl::domain.eq(&watch.domain),
            watch_dsl::enabled.eq(watch.enabled),
            watch_dsl::last_days_remaining.eq(watch.last_days_remaining),
            watch_dsl::last_checked_at.eq(watch.last_checked_at),
            watch_dsl::alerted_thresholds.eq(&watch.alerted_thresholds),
            watch_dsl::updated_at.eq(watch.updated_at),
        ))
        .execute(&mut conn)
        .map_err(|error| {
            UnifiedError::internal(format!("Cannot store certificate watch: {error}"))
        })?;
    Ok(())
}

pub(crate) fn db_delete_cert_watch(pool: &unified_core::db::DbPool, id: Uuid) -> UnifiedResult<()> {
    use unified_core::db::schema::network_tool_cert_watches::dsl as watch_dsl;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    diesel::delete(watch_dsl::network_tool_cert_watches.filter(watch_dsl::id.eq(id)))
        .execute(&mut conn)
        .map_err(|error| {
            UnifiedError::internal(format!("Cannot delete certificate watch: {error}"))
        })?;
    Ok(())
}
