// Documents module - chunked / resumable uploads, multi-file upload,
// per-module allowlists, pluggable virus scanning, versioning,
// per-file access control with signed expiring download URLs,
// quotas, and preview metadata.
//
// Bytes live in the core storage abstraction (`unified_core::storage`):
// the default provider is `LocalProvider` under `data/documents/`.
// This module builds no second storage layer; stored objects are
// addressed by validated, scope-free storage keys, dated:
// `<kind>/YYYY/MM/DD/<uuidv7>[.<ext>]` - the SAME shape for tenant
// and control-plane content (`kind` is `documents`, `media`, or
// `others`; the date is the UTC upload date; `<ext>` is the
// validated original extension, `[a-z0-9]{1,10}` lowercased, or
// absent). The key carries NO scope and NO tenant id: there is no
// `tenants/<tenancy-id>/` or `system/` prefix. The tenant/system
// distinction lives in the provider layer - `StorageScope::root`
// resolves the provider root (`tenants/<tenant-id>` or `system`)
// at runtime from the scope, and the provider only ever sees the
// rooted (effective) key - plus the DB metadata
// (`documents.scope`, `documents.company_id`). A later change to
// the tenancy-id logic therefore never requires re-keying objects:
// only the scope-to-root mapping changes. Per-tenant backup is
// DB-driven (list the tenant's documents via
// `documents.company_id`) plus the tenant's provider root, not
// prefix-based on the key. File names are fresh
// UUIDv7s - one per upload, never the checksum - so the same content
// uploaded twice is two distinct objects, listings sort by creation
// (UUIDv7 is time-ordered), and the name is known at upload start.
// SHA-256 is still computed at finalize and stored in the DB metadata
// (checksum column) as the integrity value; the DB is the source of
// truth for metadata (original name, size, MIME, checksum). The
// extension -> kind mapping lives in a data-driven registry (default
// list in `extension_kinds.yaml`, admin overrides via
// `documents.extensions` in config.yaml); it validates + normalizes
// extensions and derives the storage kind when the caller does not
// specify one explicitly. There is no cross-record deduplication:
// every upload stores its own copy (storage is the tenant's own
// quota). The same bytes uploaded again mint a fresh UUIDv7 key and
// land as a separate copy.
//
// Chunked uploads never hold parts in RAM: each part is spooled to a
// temp file under the session's spool directory (default
// `<storage>/temps/uploads/YYYY/MM/DD/<session-id>`, overridable with the
// `spool_dir` setting; part files are 6-digit zero-padded,
// `000001.part`), and completion stream-assembles the parts
// into the final object chunk by chunk. The `max_chunk_ram_bytes`
// setting (100 MiB..=8 GiB, default 100 MiB) caps the in-RAM
// buffering of the chunked path. Both settings live in the
// `documents:` section of the config file and on the admin settings
// page; a session keeps the spool directory it started in.
// The in-memory DashMap store is authoritative at runtime; the
// tables in `migrations/2026-11-22-000064_documents` are persistence
// targets only (same convention as the other batch modules).
//
// Visibility (public / private): every document carries a
// `visibility` column (`public` | `private`, default `private`;
// migration 000123). The stored key is visibility-neutral - the
// area lives in the provider key (`{root}/{area}/{key}`), so a
// flip MOVES the object between the public and private areas and
// the URL never changes. Files are served from `/files/*` (never
// from `/api/v1/*` - the API is JSON-only):
//   GET /files/public/{key}   zero checks: no auth, no DB read.
//                              Found -> serve; miss -> fall through
//                              to the app's normal 404 handling.
//   GET /files/private/{key}  the access lock (4-rule flow):
//                              rule 1 - load the DB row +
//                              visibility; public -> skip the
//                              header check, serve. Rule 2 -
//                              private -> check the header (or a
//                              signed `?token=`). Rule 3 - valid ->
//                              proceed. Rule 4 - found -> serve,
//                              not found -> 404. ALL failures ->
//                              404 uniform (never 401/403), with an
//                              S3-style XML error body.
// Both routes stream in 64 KiB chunks (never buffer a whole
// file), support `Range` (206 / 416), and take `?download=1` for
// an attachment (default inline). A file may only be public when
// its scan is clean (the scan gate); publish flips attached media
// public, unpublish flips it back private (see the content hooks
// below).
//
// Role-scoped media listing (`list_visible` in
// validation.rs - the LIST endpoint itself filters; the UI never
// receives a row the caller may not see):
//   - `GET /api/v1/documents/picker` (the embed picker): PUBLIC
//     documents only, for EVERY role. Content embeds a public
//     URL; a private file would 404 for readers, so it never
//     appears here until it is flipped public.
//   - `GET /api/v1/documents` (the manager list): admins
//     (`documents:admin`) see public + private in the company
//     scope; author / editor roles see every public document in
//     scope plus their OWN private documents - so they can mark
//     their own uploads public ("mark as public", the flip
//     endpoint, owner-or-admin, scan gate applies). Another
//     owner's private document never appears: not its name, its
//     URL, or its existence. Out-of-scope rows are hidden for
//     every role.
//   - Single-record reads (`GET /{id}`, preview, versions) stay
//     owner-or-admin (`can_read`): the list may show another
//     owner's public document, but its metadata detail still
//     requires ownership or the admin capability.
//
// Lazy finalize: a chunked upload returns as soon as all parts
// are received - the whole-file SHA-256 does NOT run on the
// request path. The session moves to `received` and a `pending`
// row is created in `finalize_jobs` (migration 000123); the
// worker hashes the stored object afterwards (idle-only draining,
// 2-4 concurrent jobs, 24h starvation guard), then either stores
// the checksum (`verified`) or fails the job (mismatch ->
// `failed`, object deleted, session `failed`,
// `document.finalize.failed` emitted). The document row carries
// `checksum: NULL` until the worker verifies it. The single-shot
// upload still hashes synchronously (<= 100 MiB) and needs no job.
//
// Routes live under `/api/v1/documents`:
//
//   Documents:
//     GET    /                              list documents (paginated, filtered)
//     POST   /upload                        multipart multi-file upload
//     GET    /picker                        shared media picker (public only)
//     GET    /{id}                          one document
//     PATCH  /{id}                          update metadata (title, alt
//                                            text, caption, description,
//                                            custom keys; emits search.index)
//     DELETE /{id}                          delete document + all versions
//     POST   /{id}/rename                   rename document (file name;
//                                            title follows when it still
//                                            equals the old name)
//     POST   /{id}/move                     move document between folders
//     GET    /{id}/preview                  preview metadata (mime, size,
//                                            image dimensions, checksum,
//                                            version count, scan status)
//     POST   /{id}/flip                     flip visibility public/private
//     POST   /{id}/signed-url               issue a signed expiring URL
//                                            (returns /files/private/{key}?token=...)
//     POST   /{id}/versions                 re-upload (new version)
//     GET    /{id}/versions                 list versions
//     GET    /{id}/versions/{version}       one version
//     POST   /{id}/versions/{version}/restore
//                                            restore a version (new version)
//     POST   /{id}/rescan                   re-run the virus scan (admin)
// Folders (migration 000132):
//     GET    /folders                       folder tree (nested, with
//                                            direct document counts)
//     POST   /folders                       create a folder
//     GET    /folders/{id}                  one folder
//     PATCH  /folders/{id}                  rename a folder
//     POST   /folders/{id}/move             move a folder (cycle-safe)
//     DELETE /folders/{id}                  delete an empty folder
//                                            (409 while it holds files
//                                            or subfolders)
//   Files (direct serving, no /api/vX, no version segment):
//     GET    /files/public/{key}            serve, zero checks
//     GET    /files/private/{key}           serve, 4-rule access lock
//   Chunked / resumable uploads:
//     POST   /uploads/sessions              initiate a session
//     GET    /uploads/sessions              list sessions
//     GET    /uploads/sessions/{id}         session status (missing parts)
//     POST   /uploads/sessions/{id}/parts   upload one part
//     POST   /uploads/sessions/{id}/complete
//                                            assemble + store + finalize
//                                            (lazy hash - see above)
//     DELETE /uploads/sessions/{id}         abort (delete partial parts)
//     POST   /uploads/sessions/purge-expired
//                                            delete expired sessions (admin)
//   Allowlist (admin):
//     GET    /allowlist                     list rules
//     POST   /allowlist                     create a rule
//     GET    /allowlist/{id}                one rule
//     PATCH  /allowlist/{id}                update a rule
//     DELETE /allowlist/{id}                delete a rule
//   Quotas:
//     GET    /quotas/usage                  quota usage (company / user)
//     GET    /quotas                        list local quotas
//     POST   /quotas                        create / replace a local quota (admin)
//   Scanners / settings / downloads:
//     GET    /scanners                      registered scanners
//     GET    /settings                      document settings
//     PATCH  /settings                      update settings (admin)
//     GET    /downloads                     download records (paginated)
//
// Events emitted on the hook bus (never direct calls to other modules):
//   - `document.uploaded` (new document finalized, incl. via a
//     completed chunked session), `document.version.created`
//     (re-upload or restore), `document.downloaded` (every download,
//     including unscanned ones), `document.deleted`,
//     `document.scan.completed` (every scan verdict),
//     `document.quota.exceeded` (upload rejected by quota),
//     `document.finalize.verified` / `document.finalize.failed`
//     (lazy-finalize outcome), `document.visibility.flipped`
//     (a visibility flip, with the previous value and the actor),
//     `search.index` (a document is created or its metadata
//     changes, so the Search module's instant index carries the
//     current title and description).
// This module also LISTENS to the content module:
//   - `content.published` / `content.unpublished`: attached media
//     flips public on publish and back to private on unpublish
//     (best-effort detached tasks - the hook bus is synchronous
//     and the flip is async; the explicit `POST /{id}/flip`
//     endpoint is the reliable path).
// Quota checks ask the Storage Management module through its
// `storage.object.upload_requested` event (payload
// `{"module": "documents", "owner", "company_id", "key_hint",
// "bytes", "mime"}`); the answer carries `quota_ok` /
// `quota_exceeded`. When no Storage Management handler answers,
// the module falls back to its own local per-company / per-user
// quotas, and an upload with no quota row at all is unlimited
// locally. A quota rejection is always fail-closed: the upload is
// refused with 409 and `document.quota.exceeded` is emitted.
//
// Virus scanning - pluggable, nothing bundled:
//   Implement `DocumentScanner` (`scan(bytes) -> ScanVerdict`) and
//   register it with `DocumentsState::register_scanner` (or
//   `DocumentsModule::register_scanner`). Scanners are listed at
//   `GET /api/v1/documents/scanners`. A scan runs synchronously
//   before a file becomes downloadable and the verdict is recorded
//   on the document and on every version. With no scanner
//   configured the verdict is `Skipped`. Download rules:
//   `Clean` -> allowed; `Infected` -> blocked, always;
//   `Pending` -> blocked; `Error` -> blocked; `Skipped` -> allowed
//   ONLY when the admin setting `allow_unscanned_downloads` is true
//   (default false - fail closed), and every skipped download is
//   audit-logged through `document.downloaded` with
//   `"scan_status": "skipped"`.
//
// Signed download URLs: HMAC-SHA-256 over
// `"<document-id>:<scope>:<expiry-unix>"` with a per-process random key
// generated at startup (the key is never persisted, never logged,
// and never returned; links stop working after a restart, by
// design). The scope (tenant UUID or the literal `system`) travels
// inside the token, so a token for one tenant cannot replay
// against another scope's tree. Verification uses a constant-time
// comparison and rejects expired or tampered tokens. The token
// itself is the capability for `GET /files/private/{key}?token=...`;
// every other route requires the `X-User-Id` / `X-Role` headers
// and the matching `documents:read` / `documents:write` /
// `documents:admin` capability through the `role.check` hook (fail
// closed: no answer means not allowed).
//
// Follow-up, deliberately out of this round: no thumbnail
// rendering. `GET /{id}/preview` returns metadata only (including
// image dimensions parsed from the image header bytes); generating
// thumbnail images is follow-up work.
//
// Actor identity and capability checks follow the Storage Manager /
// AI-module shape: the caller is identified by the `X-User-Id`
// header and the role by the `X-Role` header, and every route asks
// the roles module through the `role.check` hook.

use super::*;
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::HookBus;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::modules::ai;
use actix_multipart::Multipart;
use actix_web::web;
use std::sync::Arc;

// Module

pub struct DocumentsModule {
    state: Arc<DocumentsState>,
}

impl DocumentsModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(DocumentsState::new()),
        }
    }

    /// Register a virus scanner with this module's state.
    pub fn register_scanner(&self, scanner: Arc<dyn DocumentScanner>) {
        self.state.register_scanner(scanner);
    }

    /// Remove a virus scanner by name.
    pub fn unregister_scanner(&self, name: &str) -> bool {
        self.state.unregister_scanner(name)
    }
}

impl Default for DocumentsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for DocumentsModule {
    fn id(&self) -> &'static str {
        "documents"
    }

    fn name(&self) -> &'static str {
        "Documents"
    }

    fn description(&self) -> &'static str {
        "Documents: chunked uploads, per-module allowlists, virus-scan hook, versioning, signed downloads, quotas, and document metadata."
    }

    fn requires(&self) -> &[&'static str] {
        &["storage_manager", "roles"]
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &["infer", "hmac", "sha2", "actix-multipart", "dashmap"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        ai::register_ai_capabilities("documents", AI_CAPABILITIES);
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        // Config-file settings seed the admin-managed settings
        // (`documents.spool_dir` / `documents.max_chunk_ram_bytes` /
        // `documents.extensions` from config.yaml). The admin PATCH
        // endpoint can override them at runtime; the config file
        // wins again on restart.
        let config = ctx.config.read().expect("config lock poisoned").clone();
        self.state.apply_config_settings(&config.documents)?;
        // The lazy-finalize worker starts with the module (it
        // re-scans received sessions on first start, so a restart
        // never orphans a pending hash).
        self.state.start_finalize_worker();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        // Content publish state drives attachment visibility
        // (publish -> public, unpublish -> private).
        register_content_hooks(self.state.clone(), bus);
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/documents", owner: "documents", description: "List documents", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/documents/upload", owner: "documents", description: "Multipart multi-file upload", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/documents/picker", owner: "documents", description: "Shared media picker (public only)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/documents/{id}", owner: "documents", description: "Get document", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/documents/{id}", owner: "documents", description: "Update document metadata", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/documents/{id}", owner: "documents", description: "Delete document", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/documents/{id}/rename", owner: "documents", description: "Rename document", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/documents/{id}/move", owner: "documents", description: "Move document between folders", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/documents/folders", owner: "documents", description: "List folder tree", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/documents/folders", owner: "documents", description: "Create folder", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/documents/folders/{id}", owner: "documents", description: "Get folder", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/documents/folders/{id}", owner: "documents", description: "Rename folder", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/documents/folders/{id}/move", owner: "documents", description: "Move folder", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/documents/folders/{id}", owner: "documents", description: "Delete empty folder", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/documents/{id}/preview", owner: "documents", description: "Preview metadata", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/documents/{id}/flip", owner: "documents", description: "Flip visibility public/private", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/documents/{id}/signed-url", owner: "documents", description: "Issue signed download URL", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/files/public/{key}", owner: "documents", description: "Serve public file (zero checks)", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "GET", path: "/files/private/{key}", owner: "documents", description: "Serve private file (4-rule access lock)", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "POST", path: "/api/v1/documents/{id}/versions", owner: "documents", description: "Re-upload as new version", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/documents/{id}/versions", owner: "documents", description: "List versions", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/documents/{id}/versions/{version}", owner: "documents", description: "Get version", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/documents/{id}/versions/{version}/restore", owner: "documents", description: "Restore version", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/documents/{id}/rescan", owner: "documents", description: "Re-run virus scan", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/documents/uploads/sessions", owner: "documents", description: "Initiate chunked upload session", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/documents/uploads/sessions", owner: "documents", description: "List upload sessions", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/documents/uploads/sessions/{id}", owner: "documents", description: "Upload session status", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/documents/uploads/sessions/{id}/parts", owner: "documents", description: "Upload one chunk part", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/documents/uploads/sessions/{id}/complete", owner: "documents", description: "Complete chunked upload", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/documents/uploads/sessions/{id}", owner: "documents", description: "Abort upload session", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/documents/uploads/sessions/purge-expired", owner: "documents", description: "Purge expired upload sessions", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/documents/allowlist", owner: "documents", description: "List allowlist rules", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/documents/allowlist", owner: "documents", description: "Create allowlist rule", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/documents/allowlist/{id}", owner: "documents", description: "Get allowlist rule", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/documents/allowlist/{id}", owner: "documents", description: "Update allowlist rule", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/documents/allowlist/{id}", owner: "documents", description: "Delete allowlist rule", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/documents/quotas/usage", owner: "documents", description: "Quota usage", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/documents/quotas", owner: "documents", description: "List local quotas", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/documents/quotas", owner: "documents", description: "Create local quota", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/documents/scanners", owner: "documents", description: "List registered scanners", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/documents/settings", owner: "documents", description: "Get document settings", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/documents/settings", owner: "documents", description: "Update document settings", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/documents/downloads", owner: "documents", description: "List download records", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/documents")
                // Literal segments first: `/picker` must win over
                // `/{id}` for the shared media picker, and
                // `/folders` must win over `/{id}` for the folder
                // tree (a folder id must never be read as a
                // document id).
                .route("/picker", web::get().to(list_picker))
                .route("/folders", web::get().to(list_folders))
                .route("/folders", web::post().to(create_folder))
                .route("/folders/{id}", web::get().to(get_folder))
                .route("/folders/{id}", web::patch().to(rename_folder))
                .route("/folders/{id}/move", web::post().to(move_folder))
                .route("/folders/{id}", web::delete().to(delete_folder))
                .route("", web::get().to(list_documents))
                .route("/upload", web::post().to(upload_documents))
                .route("/{id}", web::get().to(get_document))
                .route("/{id}", web::patch().to(super::handlers::update_document_metadata))
                .route("/{id}", web::delete().to(delete_document))
                .route("/{id}/rename", web::post().to(rename_document))
                .route("/{id}/move", web::post().to(move_document))
                .route("/{id}/preview", web::get().to(preview_document))
                .route("/{id}/flip", web::post().to(flip_document_visibility))
                .route("/{id}/signed-url", web::post().to(issue_signed_url))
                .route("/{id}/versions", web::post().to(upload_new_version))
                .route("/{id}/versions", web::get().to(list_versions))
                .route("/{id}/versions/{version}", web::get().to(get_version))
                .route("/{id}/versions/{version}/restore", web::post().to(restore_version))
                .route("/{id}/rescan", web::post().to(rescan_document))
                .route("/uploads/sessions", web::post().to(initiate_session))
                .route("/uploads/sessions", web::get().to(list_sessions))
                .route("/uploads/sessions/purge-expired", web::post().to(purge_expired_sessions))
                .route("/uploads/sessions/{id}", web::get().to(get_session))
                .route("/uploads/sessions/{id}/parts", web::post().to(upload_part))
                .route("/uploads/sessions/{id}/complete", web::post().to(complete_session))
                .route("/uploads/sessions/{id}", web::delete().to(abort_session))
                .route("/allowlist", web::get().to(list_allowlist))
                .route("/allowlist", web::post().to(create_allowlist))
                .route("/allowlist/{id}", web::get().to(get_allowlist))
                .route("/allowlist/{id}", web::patch().to(update_allowlist))
                .route("/allowlist/{id}", web::delete().to(delete_allowlist))
                .route("/quotas/usage", web::get().to(quota_usage))
                .route("/quotas", web::get().to(list_quotas))
                .route("/quotas", web::post().to(create_quota))
                .route("/scanners", web::get().to(list_scanners))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::patch().to(update_settings))
                .route("/downloads", web::get().to(list_downloads)),
        );
        // Direct file serving (no /api/vX, no version segment):
        // the key is a tail path (`{key:.*}`) so the full dated
        // key reaches the handler. Both routes are `Public` in the
        // catalog because the handlers are their own auth boundary
        // (the public route checks nothing; the private route
        // enforces the 4-rule flow + token auth in-handler and
        // answers every failure with a uniform 404).
        cfg.service(
            web::scope("/files")
                .route("/public/{key:.*}", web::get().to(serve_public_file))
                .route("/private/{key:.*}", web::get().to(serve_private_file)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(DocumentsModule::new()));
}
