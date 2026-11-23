// Storage Manager module — the storage hub every other module uses.
//
// This module is the MANAGEMENT layer on top of the existing storage
// abstraction (`unified_core::storage`): it does not re-implement any
// provider. Buckets/containers, per-module bindings, quotas, the file
// browser, usage stats, lifecycle rules, signed-URL revocation, and
// health checks live here; the actual bytes move through the
// `StorageProvider` trait (local, S3, R2, Google Drive, and the SFTP
// provider added in crates/unified-core/src/storage.rs`).
//
// Routes live under `/api/v1/storage-manager` (NOT `/api/v1/storage`,
// which belongs to the existing storage module):
//
//   Status / defaults:
//     GET    /                              status (counts, providers, defaults)
//     GET    /defaults                      global + per-company default buckets
//     POST   /defaults                      set a global or per-company default
//   Buckets:
//     GET    /buckets                       list buckets (paginated, filtered)
//     POST   /buckets                       create a bucket (credentials write-only)
//     GET    /buckets/{id}                  one bucket (secrets never returned)
//     PATCH  /buckets/{id}                  update a bucket
//     DELETE /buckets/{id}                  delete a bucket
//     POST   /buckets/{id}/test             test the bucket connection
//     POST   /buckets/{id}/health           run a health check (probe + cleanup)
//     GET    /buckets/{id}/health           recent health checks of a bucket
//     GET    /health                        recent health checks (all buckets)
//   Objects (file browser):
//     GET    /buckets/{id}/objects          list recorded objects (paginated)
//     POST   /buckets/{id}/objects?key=     upload object bytes (raw body)
//     GET    /buckets/{id}/objects/download?key=
//                                          download object bytes (raw body, SHA-256 verified when recorded)
//     DELETE /buckets/{id}/objects?key=     delete an object
//     POST   /buckets/{id}/objects/move     move / rename within a bucket
//     POST   /buckets/{id}/objects/verify?key=
//                                          verify one object against its recorded SHA-256
//     POST   /buckets/{id}/objects/orphans/cleanup
//                                          delete provider objects with no recorded row (dry-run by default)
//   Signed URLs (revocable hub tokens):
//     POST   /buckets/{id}/signed-url       issue a hub download token (+ provider URL when supported)
//     GET    /signed-urls                   list issued tokens (paginated)
//     POST   /signed-urls/{id}/revoke       revoke a token
//     GET    /download/{token}              redeem a hub token (expiry + revocation enforced)
//   Bindings:
//     GET    /bindings                      list bindings (paginated, filtered)
//     POST   /bindings                      create a binding (module -> bucket + prefix)
//     GET    /bindings/{id}                 one binding
//     PATCH  /bindings/{id}                 update a binding (emits storage.binding.changed)
//     DELETE /bindings/{id}                 delete a binding
//     GET    /bindings/resolve?module=&company_id=
//                                          resolve the fallback chain for a module
//   Quotas:
//     GET    /quotas                        list quotas (paginated, filtered)
//     POST   /quotas                        create a quota (company / user / module)
//     GET    /quotas/{id}                   one quota
//     PATCH  /quotas/{id}                   update quota limits
//     DELETE /quotas/{id}                   delete a quota
//   Stats:
//     GET    /stats                         totals + per-bucket / company / user / module breakdowns
//     GET    /stats/daily                   daily usage rollups (from, to, filters)
//     GET    /stats/top                     top consumers by bytes
//   Lifecycle:
//     GET    /lifecycle                     list lifecycle rules
//     POST   /lifecycle                     create a lifecycle rule
//     GET    /lifecycle/{id}                one rule
//     PATCH  /lifecycle/{id}                update a rule
//     DELETE /lifecycle/{id}                delete a rule
//     POST   /lifecycle/run                 run lifecycle rules now (sync; also enqueues via scheduler)
//
// Events emitted on the hook bus:
//   - `storage.bucket.created`, `storage.bucket.updated`,
//     `storage.bucket.deleted`
//   - `storage.binding.changed` (binding created / updated / deleted,
//     and when a default bucket changes)
//   - `storage.object.stored`, `storage.object.deleted`,
//     `storage.object.archived`
//   - `storage.quota.exceeded` (hard limit, upload rejected with 409),
//     `storage.quota.warning` (soft limit crossed, upload allowed)
//   - `storage.health.failed`, `storage.health.ok`
//   - `storage.signed_url.issued`, `storage.signed_url.revoked`
//   - `storage.lifecycle.run.completed`
//   - `storage.plan.created`, `storage.plan.updated`,
//     `storage.plan.deleted`, `storage.plan.assigned`,
//     `storage.plan.changed`, `storage.plan.cancelled`,
//     `storage.plan.quota_refused` (plan change refused because
//     current usage exceeds the new plan quota)
//   - `storage.metering.reported` (buyer + plan + bytes + files +
//     period, for billing metered plans; also emitted for BYO usage)
//   - `storage.byo.created`, `storage.byo.updated`,
//     `storage.byo.validated`, `storage.byo.validation_failed`,
//     `storage.byo.activated`, `storage.byo.suspended`,
//     `storage.byo.revoked`, `storage.byo.terms_accepted`,
//     `storage.byo.terms_changed`, `storage.byo.health_ok`,
//     `storage.byo.health_failed`, `storage.byo.quota_exceeded`
// Events this module listens to:
//   - `storage.object.upload_requested` — answered with the resolved
//     binding + upload endpoint (see `on_upload_requested`). Payload:
//     `{"module", "owner", "company_id"?, "key_hint", "bytes", "mime"?}`.
//     Result: `{"resolved", "bucket_id", "bucket_name",
//     "provider_kind", "key", "prefix", "upload_endpoint",
//     "quota_ok", "quota_warning"}` or `{"resolved": false, "error", ...}`.
//   - `storage.lifecycle.run_requested` — the event the scheduler
//     emits when a `storage.lifecycle.run` job fires (see the
//     scheduler note below). The handler spawns the run in the
//     background and answers `{"accepted": true, "queued": true}`.
//   - `storage.byo.health_check_requested` — the event the
//     scheduler emits when a `storage.byo.health_check` job fires
//     (see the scheduler note below). The handler spawns health
//     checks for every active BYO connection in the background and
//     answers `{"accepted": true, "queued": true}`.
//   - This module EMITS `scheduler.enqueue` with kind
//     `storage.byo.health_check` when `POST /byo/health/run` is
//     called with `enqueue: true`.
//
// Scheduler note (integrator): the scheduler's `KNOWN_KINDS` does
// not contain `storage.byo.health_check` yet, so today the enqueue
// answers `{"queued": false, "error": "Unknown job kind: ..."}` and
// the run endpoint falls back to running synchronously in-process.
// The follow-up is to add `"storage.byo.health_check"` to
// `KNOWN_KINDS` in `src/modules/scheduler.rs` and arm it, e.g.:
//   `"storage.byo.health_check" => {
//       ctx.hooks.emit("storage.byo.health_check_requested", payload);
//   }`
// Until then BYO health runs only through `POST /byo/health/run`
// or the in-process fallback — never pretend a job was scheduled
// when the scheduler declined it. The same note applies to
// `storage.lifecycle.run` above.
//
// Storage plans (storage as a product): plans are products sold
// through Product Management / billing — this module stores only
// the quota, the included provider kinds, and a `product_id`
// billing reference plus a billing-cycle hint; it never invents a
// money column. A buyer holds at most one active plan assignment
// (company and/or user scoped); cancelled assignments are retained
// by id, never overwritten, so an old assignment stays resolvable.
// Changing a plan re-checks current usage against the new quota
// and refuses with 409 when usage exceeds it. Usage metering
// (bytes + files per buyer) is reported with
// `storage.metering.reported` so billing can charge metered plans:
// the current month is read from live usage (hub + BYO objects),
// a past month from the daily rollup, so a backdated report states
// that period's usage, not today's. Plan-gated use checks the
// module entitlement in `crate::modules::licensing` for module id
// `storage_manager`.
//
// Hub objects are tenant-scoped like BYO objects: bucket
// `company_id` is checked against the verified caller's company
// and object `owner_user` / `company_id` against the
// caller (admin override only); cross-tenant access answers 404.
// Bucket endpoint hosts may only come from the operator allowlist
// (`storage.allowed_hosts` in the application config) — a bucket's
// own `allowed_hosts` may only narrow that list — and the endpoint
// hostname is DNS-revalidated at request time (private /
// link-local resolution is refused). Hub download tokens are
// single-use and stored as SHA-256 hashes only; BYO activation
// requires a validation no older than 24 hours.
//
// Bring-your-own storage (BYO): a user or company connects their
// own external storage as a backend. Only provider kinds on the
// admin-managed allowlist may be connected; every connection is
// validated (connect, list a test prefix, write + read + delete a
// probe object, then remove it) before it can activate; the owner
// must accept the current terms version first; and every object
// operation through the connection is scoped to the connection's
// path prefix, checked against its allowed operations, file-type
// allowlists, and quotas, with ownership checked on every access.
// A cross-owner access is answered 404 (never 403), so existence
// does not leak. BYO credentials follow the bucket pattern exactly:
// write-only, encrypted with the same versioned `v1.` construction,
// never returned, logged, or placed in a hook payload.
//
// Provider-kind names used by plans and the BYO allowlist are the
// canonical strings `local`, `s3`, `r2`, `drive`, `sftp`, and
// `s3-compatible`. `drive` is the Google Drive kind and
// `s3-compatible` is the S3 kind with a custom endpoint (an endpoint
// is required for it); there is no separate `StorageKind` for
// either alias.
//
// Scheduler note (integrator): the scheduler's `KNOWN_KINDS` does
// not contain `storage.lifecycle.run` yet, so today the enqueue
// answers `{"queued": false, "error": "Unknown job kind: ..."}` and
// the run endpoint falls back to running synchronously in-process.
// The follow-up is to add `"storage.lifecycle.run"` to `KNOWN_KINDS`
// in `src/modules/scheduler.rs` and arm it, e.g.:
//   `"storage.lifecycle.run" => {
//       ctx.hooks.emit("storage.lifecycle.run_requested", payload);
//   }`
// Until then lifecycle runs only through `POST /lifecycle/run` or
// the in-process fallback — never pretend a job was scheduled when
// the scheduler declined it.
//
// Encryption at rest — honest matrix (the flag is stored per bucket;
// enforcement is provider-side where supported):
//   - S3 / R2: the existing S3 provider does NOT send a
//     server-side-encryption header today, so the flag is recorded
//     but NOT enforced by this codebase. The bucket must be created
//     with default encryption at the provider console for at-rest
//     encryption to hold. Do not read the flag as a guarantee.
//   - Local: OS / filesystem-level only (disk encryption, file
//     permissions). No application-level object encryption.
//   - Google Drive: transport encryption only (TLS + Google's
//     platform encryption); no customer-managed key support here.
//   - SFTP: transport encryption only (SSH); at-rest encryption
//     depends on the remote server's filesystem.
// The flag therefore means "the operator requires at-rest
// encryption for this bucket" and is surfaced in the bucket view so
// operators can audit it; it does not change provider behaviour.
//
// Credentials are write-only: bucket credentials are accepted once,
// encrypted with the same versioned `v1.` HMAC-keystream construction
// the AI module uses (`encrypt_secret_value` / `decrypt_secret_value`
// below), and never returned, logged, or placed in a hook payload.
// Views expose only `credentials_configured` and a short fingerprint.
// A shared core crypto helper would be better than a second copy;
// that deduplication is integrator work, not done here.
//
// File browser honesty: the browser lists objects RECORDED through
// this hub (uploads via `POST /buckets/{id}/objects` and the
// `upload_requested` flow). Providers that implement `list()`
// (local, SFTP) are also enumerated live and unrecorded keys are
// shown with `recorded: false`; providers without `list()` (S3, R2,
// Drive) cannot enumerate, so objects uploaded outside the hub are
// invisible to the browser until they are recorded. The browser
// never claims a bucket is empty when listing is unsupported.
//
// Actor identity and capability checks follow the AI module: the
// caller is identified by the `X-User-Id` header and the role by the
// `X-Role` header, and every route asks the roles module through the
// `role.check` hook. The check fails closed: no answer means not
// allowed.

// Module registration: struct, Module impl and register().

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;

use super::*;

// ---------------------------------------------------------------------------

pub struct StorageManagerModule {
    pub(crate) state: StorageManagerState,
}

impl StorageManagerModule {
    pub fn new() -> Self {
        Self {
            state: StorageManagerState::default(),
        }
    }
}

impl Default for StorageManagerModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for StorageManagerModule {
    fn id(&self) -> &'static str {
        "storage_manager"
    }

    fn name(&self) -> &'static str {
        "Storage Manager"
    }

    fn description(&self) -> &'static str {
        "Storage hub: bucket registry, per-module bindings with a fallback chain, quotas, file browser, usage stats, lifecycle rules, revocable signed URLs, and health checks on top of the local, S3, R2, Google Drive, and SFTP providers. Sold as a product with storage plans (tiers, quotas, metering for billing) and bring-your-own storage connections under admin allowlist, validation, rules, and terms."
    }

    /// Storage Manager is sold as a product: a customer buys a
    /// module entitlement for it (see `crate::modules::licensing`).
    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("infrastructure")
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "company", "notifications"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "company", "notifications", "storage", "scheduler"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("storage_manager", AI_CAPABILITIES);
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        *self
            .state
            .app_config
            .write()
            .expect("app config lock poisoned") = Some(Arc::clone(&ctx.config));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            events::UPLOAD_REQUESTED,
            "storage_manager",
            Arc::new(move |ctx: &mut HookContext| {
                on_upload_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            events::LIFECYCLE_RUN_REQUESTED,
            "storage_manager",
            Arc::new(move |ctx: &mut HookContext| {
                on_lifecycle_run_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            events::BYO_HEALTH_CHECK_REQUESTED,
            "storage_manager",
            Arc::new(move |ctx: &mut HookContext| {
                on_byo_health_check_requested(state.clone(), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/storage-manager")
                .app_data(state)
                .route("", web::get().to(get_status))
                .route("/defaults", web::get().to(get_defaults))
                .route("/defaults", web::post().to(set_default))
                .route("/buckets", web::get().to(list_buckets))
                .route("/buckets", web::post().to(create_bucket))
                .route("/buckets/{id}", web::get().to(get_bucket))
                .route("/buckets/{id}", web::patch().to(update_bucket))
                .route("/buckets/{id}", web::delete().to(delete_bucket))
                .route("/buckets/{id}/test", web::post().to(test_bucket))
                .route("/buckets/{id}/health", web::post().to(health_bucket))
                .route("/buckets/{id}/health", web::get().to(list_bucket_health))
                .route("/health", web::get().to(list_health))
                .route("/buckets/{id}/objects", web::get().to(list_objects))
                .route("/buckets/{id}/objects", web::post().to(upload_object))
                .route("/buckets/{id}/objects/download", web::get().to(download_object))
                .route("/buckets/{id}/objects", web::delete().to(delete_object))
                .route("/buckets/{id}/objects/move", web::post().to(move_object))
                .route("/buckets/{id}/objects/verify", web::post().to(verify_object))
                .route(
                    "/buckets/{id}/objects/orphans/cleanup",
                    web::post().to(cleanup_orphans),
                )
                .route("/buckets/{id}/signed-url", web::post().to(issue_signed_url))
                .route("/signed-urls", web::get().to(list_signed_urls))
                .route("/signed-urls/{id}/revoke", web::post().to(revoke_signed_url))
                .route("/download/{token}", web::get().to(redeem_download))
                .route("/bindings", web::get().to(list_bindings))
                .route("/bindings", web::post().to(create_binding))
                .route("/bindings/resolve", web::get().to(resolve_binding_endpoint))
                .route("/bindings/{id}", web::get().to(get_binding))
                .route("/bindings/{id}", web::patch().to(update_binding))
                .route("/bindings/{id}", web::delete().to(delete_binding))
                .route("/quotas", web::get().to(list_quotas))
                .route("/quotas", web::post().to(create_quota))
                .route("/quotas/{id}", web::get().to(get_quota))
                .route("/quotas/{id}", web::patch().to(update_quota))
                .route("/quotas/{id}", web::delete().to(delete_quota))
                .route("/stats", web::get().to(get_stats))
                .route("/stats/daily", web::get().to(get_stats_daily))
                .route("/stats/top", web::get().to(get_stats_top))
                .route("/lifecycle", web::get().to(list_lifecycle))
                .route("/lifecycle", web::post().to(create_lifecycle))
                .route("/lifecycle/run", web::post().to(run_lifecycle))
                .route("/lifecycle/{id}", web::get().to(get_lifecycle))
                .route("/lifecycle/{id}", web::patch().to(update_lifecycle))
                .route("/lifecycle/{id}", web::delete().to(delete_lifecycle))
                .route("/plans", web::get().to(list_plans))
                .route("/plans", web::post().to(create_plan))
                .route("/plans/public", web::get().to(list_public_plans))
                .route("/plans/my-plan", web::get().to(get_my_plan))
                .route("/plans/assignments", web::get().to(list_plan_assignments))
                .route("/plans/assignments/{id}", web::get().to(get_plan_assignment))
                .route("/plans/assignments/{id}/cancel", web::post().to(cancel_plan_assignment))
                .route("/plans/metering/report", web::post().to(report_metering))
                .route("/plans/metering", web::get().to(list_metering))
                .route("/plans/{id}", web::get().to(get_plan))
                .route("/plans/{id}", web::patch().to(update_plan))
                .route("/plans/{id}", web::delete().to(delete_plan))
                .route("/plans/{id}/assign", web::post().to(assign_plan))
                .route("/byo/settings", web::get().to(get_byo_settings))
                .route("/byo/settings", web::patch().to(update_byo_settings))
                .route("/byo/terms", web::get().to(list_byo_terms))
                .route("/byo/terms", web::post().to(create_byo_terms))
                .route("/byo/terms/current", web::get().to(get_current_byo_terms))
                .route("/byo/terms/{id}", web::get().to(get_byo_terms))
                .route("/byo/terms/{id}", web::patch().to(update_byo_terms))
                .route("/byo/terms/{id}", web::delete().to(delete_byo_terms))
                .route("/byo/terms/accept", web::post().to(accept_byo_terms))
                .route("/byo/terms/enforce-reacceptance", web::post().to(enforce_byo_reacceptance))
                .route("/byo/connections", web::get().to(list_byo_connections))
                .route("/byo/connections", web::post().to(create_byo_connection))
                .route("/byo/connections/{id}", web::get().to(get_byo_connection))
                .route("/byo/connections/{id}", web::patch().to(update_byo_connection))
                .route("/byo/connections/{id}", web::delete().to(delete_byo_connection))
                .route("/byo/connections/{id}/validate", web::post().to(validate_byo_connection))
                .route("/byo/connections/{id}/activate", web::post().to(activate_byo_connection))
                .route("/byo/connections/{id}/suspend", web::post().to(suspend_byo_connection))
                .route("/byo/connections/{id}/revoke", web::post().to(revoke_byo_connection))
                .route("/byo/connections/{id}/health", web::post().to(health_byo_connection))
                .route("/byo/connections/{id}/health", web::get().to(list_byo_connection_health))
                .route("/byo/connections/{id}/objects", web::get().to(list_byo_objects))
                .route("/byo/connections/{id}/objects", web::post().to(upload_byo_object))
                .route("/byo/connections/{id}/objects/download", web::get().to(download_byo_object))
                .route("/byo/connections/{id}/objects", web::delete().to(delete_byo_object))
                .route("/byo/health", web::get().to(list_byo_health))
                .route("/byo/health/run", web::post().to(run_byo_health)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/storage-manager", owner: "storage_manager", description: "Storage Manager status: counts, providers, defaults, product declaration.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/defaults", owner: "storage_manager", description: "Global + per-company default buckets.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/defaults", owner: "storage_manager", description: "Set a global or per-company default bucket.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/buckets", owner: "storage_manager", description: "List buckets (paginated, filter by provider_kind, company_id, enabled).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/buckets", owner: "storage_manager", description: "Create a bucket (credentials write-only, encrypted).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/buckets/{id}", owner: "storage_manager", description: "Get one bucket (secrets never returned).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/storage-manager/buckets/{id}", owner: "storage_manager", description: "Update a bucket (credentials replace when present).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/storage-manager/buckets/{id}", owner: "storage_manager", description: "Delete a bucket (refused while bindings or lifecycle rules reference it).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/buckets/{id}/test", owner: "storage_manager", description: "Test a bucket connection.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/buckets/{id}/health", owner: "storage_manager", description: "Run a health check (connectivity + write/read/delete probe + cleanup).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/buckets/{id}/health", owner: "storage_manager", description: "Recent health checks of one bucket.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/health", owner: "storage_manager", description: "Recent health checks (all buckets, filter by bucket_id, ok).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/buckets/{id}/objects", owner: "storage_manager", description: "File browser: recorded objects + live provider listing where supported (paginated, filter by prefix, module, user_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/buckets/{id}/objects", owner: "storage_manager", description: "Upload object bytes (?key=, raw body, quota-checked).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/buckets/{id}/objects/download", owner: "storage_manager", description: "Download object bytes (?key=, raw body).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/storage-manager/buckets/{id}/objects", owner: "storage_manager", description: "Delete an object (?key=).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/buckets/{id}/objects/move", owner: "storage_manager", description: "Move / rename an object within a bucket.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/buckets/{id}/objects/verify", owner: "storage_manager", description: "Verify one object against its recorded SHA-256 (backfills unrecorded checksums; mismatch reported as data).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/buckets/{id}/objects/orphans/cleanup", owner: "storage_manager", description: "Delete provider objects with no recorded hub row (dry-run by default; needs a listable provider).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/buckets/{id}/signed-url", owner: "storage_manager", description: "Issue a revocable hub download token (+ provider URL when supported).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/signed-urls", owner: "storage_manager", description: "List issued signed-URL tokens (token values never shown).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/signed-urls/{id}/revoke", owner: "storage_manager", description: "Revoke a signed-URL token.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/download/{token}", owner: "storage_manager", description: "Redeem a hub download token (expiry + revocation enforced; possession is the authorization).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/bindings", owner: "storage_manager", description: "List bindings (paginated, filter by module_id, bucket_id, company_id, enabled).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/bindings", owner: "storage_manager", description: "Create a binding (module -> bucket + key prefix).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/bindings/resolve", owner: "storage_manager", description: "Resolve the binding fallback chain for a module (?module=&company_id=).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/bindings/{id}", owner: "storage_manager", description: "Get one binding.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/storage-manager/bindings/{id}", owner: "storage_manager", description: "Update a binding (emits storage.binding.changed).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/storage-manager/bindings/{id}", owner: "storage_manager", description: "Delete a binding (emits storage.binding.changed).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/quotas", owner: "storage_manager", description: "List quotas (paginated, filter by scope, scope_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/quotas", owner: "storage_manager", description: "Create a quota (company / user / module; soft + hard limits).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/quotas/{id}", owner: "storage_manager", description: "Get one quota.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/storage-manager/quotas/{id}", owner: "storage_manager", description: "Update quota limits.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/storage-manager/quotas/{id}", owner: "storage_manager", description: "Delete a quota.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/stats", owner: "storage_manager", description: "Usage totals + per-bucket / company / user / module breakdowns.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/stats/daily", owner: "storage_manager", description: "Daily usage rollups (from, to, bucket_id, company_id, user_id, module).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/stats/top", owner: "storage_manager", description: "Top consumers by bytes (?by=module|user|company|bucket, limit).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/lifecycle", owner: "storage_manager", description: "List lifecycle rules (filter by bucket_id, enabled).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/lifecycle", owner: "storage_manager", description: "Create a lifecycle rule (retention days, auto-delete, archive transition).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/lifecycle/run", owner: "storage_manager", description: "Run lifecycle rules now (sync; optionally also enqueues scheduler job storage.lifecycle.run).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/lifecycle/{id}", owner: "storage_manager", description: "Get one lifecycle rule.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/storage-manager/lifecycle/{id}", owner: "storage_manager", description: "Update a lifecycle rule.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/storage-manager/lifecycle/{id}", owner: "storage_manager", description: "Delete a lifecycle rule.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/plans", owner: "storage_manager", description: "List storage plans (admin; paginated, filter by is_public, enabled).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/plans", owner: "storage_manager", description: "Create a storage plan (quota, provider kinds, product_id billing reference; never a money column).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/plans/public", owner: "storage_manager", description: "List public enabled storage plans.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/plans/my-plan", owner: "storage_manager", description: "Current plan + usage vs quota for the caller (?company_id=&user_id= for admins).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/plans/assignments", owner: "storage_manager", description: "List plan assignments (admin; filter by company_id, user_id, plan_id, status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/plans/assignments/{id}", owner: "storage_manager", description: "Get one plan assignment.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/plans/assignments/{id}/cancel", owner: "storage_manager", description: "Cancel a plan assignment.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/plans/metering/report", owner: "storage_manager", description: "Record a metering report for a buyer and emit storage.metering.reported for billing.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/plans/metering", owner: "storage_manager", description: "List metering reports (filter by company_id, user_id, period, plan_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/plans/{id}", owner: "storage_manager", description: "Get one storage plan.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/storage-manager/plans/{id}", owner: "storage_manager", description: "Update a storage plan.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/storage-manager/plans/{id}", owner: "storage_manager", description: "Delete a storage plan (refused while active assignments exist).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/plans/{id}/assign", owner: "storage_manager", description: "Assign or change a buyer's plan (usage re-checked against the new quota; 409 on exceed; entitlement-checked).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/byo/settings", owner: "storage_manager", description: "BYO settings: provider allowlist, terms requirement, per-user connection limit.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/storage-manager/byo/settings", owner: "storage_manager", description: "Update BYO settings (admin).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/byo/terms", owner: "storage_manager", description: "List BYO terms versions (filter by is_current, enabled).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/byo/terms", owner: "storage_manager", description: "Create a BYO terms version (admin).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/byo/terms/current", owner: "storage_manager", description: "Current BYO terms version.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/byo/terms/{id}", owner: "storage_manager", description: "Get one BYO terms version.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/storage-manager/byo/terms/{id}", owner: "storage_manager", description: "Update a BYO terms version (incl. re-acceptance deadline).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/storage-manager/byo/terms/{id}", owner: "storage_manager", description: "Delete a BYO terms version (refused while current or accepted).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/byo/terms/accept", owner: "storage_manager", description: "Accept the current BYO terms version (recorded with version + timestamp).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/byo/terms/enforce-reacceptance", owner: "storage_manager", description: "Suspend connections whose owner missed the terms re-acceptance deadline (admin).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/byo/connections", owner: "storage_manager", description: "List BYO connections (owner-scoped; admins see all; filter by status, provider_kind, company_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/byo/connections", owner: "storage_manager", description: "Create a BYO connection (allowlist-checked, credentials write-only, prefix-scoped).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/byo/connections/{id}", owner: "storage_manager", description: "Get one BYO connection (cross-owner access answers 404; secrets never returned).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/storage-manager/byo/connections/{id}", owner: "storage_manager", description: "Update a BYO connection (credential replacement resets it to draft for re-validation).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/storage-manager/byo/connections/{id}", owner: "storage_manager", description: "Delete (revoke) a BYO connection.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/byo/connections/{id}/validate", owner: "storage_manager", description: "Validate a BYO connection (connect + list test prefix + write/read/delete probe).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/byo/connections/{id}/activate", owner: "storage_manager", description: "Activate a BYO connection (requires validation, allowlist, terms acceptance, entitlement).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/byo/connections/{id}/suspend", owner: "storage_manager", description: "Suspend a BYO connection (admin or owner).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/byo/connections/{id}/revoke", owner: "storage_manager", description: "Revoke a BYO connection (terminal; admin or owner).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/byo/connections/{id}/health", owner: "storage_manager", description: "Run a BYO health check (connectivity + write/read/delete probe + cleanup).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/byo/connections/{id}/health", owner: "storage_manager", description: "Recent health checks of one BYO connection.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/byo/connections/{id}/objects", owner: "storage_manager", description: "List objects through a BYO connection (prefix-scoped, recorded + live listing where supported).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/byo/connections/{id}/objects", owner: "storage_manager", description: "Upload object bytes through a BYO connection (?key=, prefix-scoped, rules + quotas enforced).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/byo/connections/{id}/objects/download", owner: "storage_manager", description: "Download object bytes through a BYO connection (?key=, prefix-scoped).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/storage-manager/byo/connections/{id}/objects", owner: "storage_manager", description: "Delete an object through a BYO connection (?key=, prefix-scoped).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/storage-manager/byo/health", owner: "storage_manager", description: "Recent BYO health checks (owner-scoped; filter by connection_id, ok).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/storage-manager/byo/health/run", owner: "storage_manager", description: "Run BYO health checks now (sync; optionally also enqueues scheduler job storage.byo.health_check).", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(StorageManagerModule::new()));
}
