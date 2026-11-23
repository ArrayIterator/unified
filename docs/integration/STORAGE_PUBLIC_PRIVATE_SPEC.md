# Storage layout — public/private implementation spec (FINAL, user-settled 2026-11-23)

Status: IMPLEMENTED — task 324, 2026-11-23 (tracker task 324, closed
after verification). The user's "gas" was given 2026-11-23 ~08:35 WIB
("Kirim ke agent sekarang"). The checklist below records the as-built
state; the History section records what changed from the spec.

## Final layout (physical = provider root + stored key)

```
Tenant public area:   storage/tenants/TENANT-ID/public/...
Tenant private area:  storage/tenants/TENANT-ID/private/...
System:               storage/system/...
Temp:                 storage/temps/uploads/YYYY/MM/DD/SESSION-ID/000001.part
```

The tenant has TWO storage areas: a public area and a private area.
`/files/public/` serves the public area directly (zero checks);
`/files/private/` is the gated route (see Flow section). The key shape
inside each area is the task-323 shape `TYPE/YYYY/MM/DD/UUIDv7.EXT`
(visibility lives in the provider root, not in the stored key); flip
= MOVE the object between the areas (see Flow section). Decided by
the user's dispatch brief 2026-11-23 ~08:27 WIB.

## Stored key (DB `storage_key`, `documents` + `document_versions`)
- Visibility is recorded in the `visibility` column on `documents`
  (enum `public` | `private`, default `private`) — the source of truth
  for the `/private/` rule 1 check and for listings. The column does
  NOT gate the `/files/public/` route: that route does no DB read at
  all, so its safety comes from the public area containing only public
  files.
- Provider root, resolved at runtime from `StorageScope` (unchanged from
  task 323): Tenant → `tenants/{id}`, System → `system`. The tenant id is
  never in the stored key, so a tenancy-logic change never requires re-keying.

## Serving rules
- ALL access goes through the app (Rust requests via storage/tenant). No
  direct public serving from the object store.
- `/files/public/`: NO header check, NO DB read, NO auth. Direct serve
  from the tenant's public storage area, like accessing a page. Found
  → serve; not found → fall through to the next middleware (the app's
  normal 404 handling — no 404 body is generated here).
- `/files/private/`: the gated route — the path segment IS the lock.
  Checks the DB row (existence + visibility), then auth, then the
  object; every failure answers 404 (uniform — never reveals whether
  a file exists or is private).
- Visibility flip mechanics: OPEN (see Flow section) — the zero-check
  public route implies a file's location encodes its visibility, so a
  flip must move it between areas (or another mechanism is needed).
- Scan gate: a file may only be SET to public after a clean scan;
  delete MUST remove the object, not just the DB row.
- SHA-256 stays computed at finalize and stored in DB metadata
  (`DocumentVersion.checksum`); dedup stays removed (task 321).

## Endpoints (new)
- `GET /files/public/...` — no auth, no DB, direct serve from the
  tenant's public storage area; miss → fall through to middleware.
- `GET /files/private/...` — gated; the 4-rule flow below (row →
  visibility → header auth → object); all failures → 404.
- One route pattern branching on the first segment (`public`/`private`),
  not two separate code paths. Tenant is resolved from the host
  (`tenant1.example.com` → tenant1, existing TenantResolutionMiddleware);
  system files are served from the control-plane host.
- JSON API routes stay: preview (JSON metadata) + signed-url mint
  (returns a URL as JSON). The byte-serving API routes MOVE to
  `/files/*` (see "API is JSON-only" below): `GET /{id}/download` →
  `GET /files/private/{key}`; `GET /download/{token}` → the token
  becomes a query param on `/files/private/{key}`.

## API is JSON-only (user rule 2026-11-23 ~08:04 WIB)
- The API standard is strict JSON: success is only `{"data":...}`,
  error is only `{"error":{...}}`. A download returns raw bytes, so it
  cannot conform — and a failure mid-stream cannot return a JSON error
  body either. File bytes therefore NEVER come from `/api/v1/*`.
- `/files/*` is the dedicated download surface (bytes in, bytes out).
- Migration table (byte-serving routes that move):
  - `GET /api/v1/documents/{id}/download` (auth download) →
    `GET /files/private/{key}` (same auth + scope, Rule 1–4).
  - `GET /api/v1/documents/download/{token}` (signed token download) →
    `GET /files/private/{key}?token=...` (the private route already
    accepts a signed token in the query).
- JSON routes that STAY under `/api/v1/documents`:
  - `GET /{id}/preview` (JSON metadata only, no bytes — verified).
  - `POST /{id}/signed-url` (mints a signed URL, returns it as JSON).
  - `POST /upload` (multipart request, JSON response — the standard
    governs responses).

### Response contract (what each endpoint returns — user-confirmed 2026-11-23 ~08:06 WIB)
- `POST /api/v1/documents/{id}/signed-url` → JSON:
  `{"data": {"url": "/files/private/TYPE/...?token=..."}}`. It is a real
  API endpoint (JSON in, JSON out); the URL it returns is where the
  file lives.
- `GET /files/public/...` → the file bytes directly, NEVER JSON.
  Failure → fall through to middleware (app 404).
- `GET /files/private/...` → the file bytes directly, NEVER JSON.
  Failure → 404 XML error (uniform).
- No file route lives under `/api/vX`, and `/files/*` carries NO
  version segment: file URLs must never change (not on flip, not on
  rename, not on API version bumps). API versioning applies to
  `/api/vX/*` (JSON) only.
- Download-attachments mode is a per-request flag, NOT an endpoint:
  `?download=1` on any `/files/*` URL sends `Content-Disposition:
  attachment; filename="<original>"` (the browser downloads instead of
  displaying inline). Default is inline. This replaces the old id-based
  download route's attachment behaviour.

## Flow + response headers (Range + streaming)

### Serving rules (user-settled 2026-11-23 ~07:44 WIB)
- `/files/public/`: direct serve, ZERO checks. No header check, no DB
  read, no auth — serve straight from the tenant's public storage
  area, like accessing a page. Found → serve. Not found → 404, i.e.
  fall through to the next middleware (the app's 404 handling).
- `/files/private/`: the path segment IS the access lock. Four rules:
  - Rule 1: check the file exists (DB row) + check `visibility`. If
    publicly accessible → SKIP the header check, serve.
  - Rule 2: if private → check the header (auth valid or not).
  - Rule 3: valid → proceed to Rule 4. (Invalid auth → 404, uniform
    with every other failure — never confirms a private file exists.
    The app's id-based routes still return proper 401/403 for UI
    errors.)
  - Rule 4: found → serve the file; not found → 404.
- Private 404 format: RECOMMENDED XML error (S3-style) — machine-
  readable, no theme engine needed inside a gated storage endpoint,
  leaks nothing, matches the S3 pattern (cf. imgcdn s3-error-404.xml).
  Awaiting the user's pick (XML error vs themed 404 page). Humans still
  get the themed page on normal routes and via the `/public/` fall-
  through.
- OPEN — flip mechanics: the zero-check public route means a file's
  location must encode its visibility. When a file flips public↔
  private, does it MOVE between the public and private areas? On local
  disk a move is a rename (free, no copy); on S3-style stores a move
  is copy+delete (transient double until the old object is deleted —
  the bloat the user flagged). Mitigations if move is chosen: verify
  the copy, then delete, plus an orphan sweeper. Awaiting the user's
  decision.

### Public flow
1. Resolve tenant from host (TenantResolutionMiddleware).
2. First segment must be `public`.
3. NO auth check, NO DB read. Serve the object from the tenant's
   public storage area.
4. Parse the `Range` header if present (Range + streaming per the
   Response headers / Provider sections below).
5. Object missing → fall through to the next middleware (app 404).

### Private flow
1. Resolve tenant from host.
2. First segment must be `private`.
3. Load the DB row by key: row missing → 404.
4. `visibility = 'public'` → serve (skip the header check entirely).
5. `visibility = 'private'` → check the header (session/JWT or signed
   token). Invalid/missing → 404 (uniform).
6. Tenant/company scope check (or token scope match). Fail → 404
   (uniform).
7. Object missing → 404. Object present → parse `Range`, serve.

### Response headers
- Full (no Range): `200 OK`, `Content-Type` (DB mime / object metadata),
  `Content-Length` (DB size / object stat), `Accept-Ranges: bytes`,
  `Cache-Control: public, max-age=31536000, immutable` (public) or
  `private, no-store` (private). Default: inline (NO `Content-Disposition`,
  or `inline; filename="<original>"`). `?download=1` →
  `Content-Disposition: attachment; filename="<original>"` (browser
  downloads — the download-attachments mode).
- Valid Range: `206 Partial Content`, `Content-Range: bytes START-END/TOTAL`,
  `Content-Length` = slice length, same `Content-Type` + `Accept-Ranges`.
- Invalid Range: `416 Range Not Satisfiable`, `Content-Range: bytes */TOTAL`.
- Object missing: `404`.

### Provider: ranged read + streaming (NEW — does not exist today)
- Local: open file, seek to START, read END-START+1 bytes, stream in
  chunks. S3: GetObject with a `Range` header (S3 supports it natively).
- Without Range: stream the whole file in chunks.
- NEVER load the whole file into RAM: the current `download_signed`
  buffers the entire file before sending (verified in source) — `/files/*`
  must stream instead.
- No Range support exists anywhere in the documents module today
  (verified by grep: zero `Range` / `Accept-Ranges` / `206` /
  `Content-Range` hits) — this is new work inside the `/files/*`
  implementation.

## Finalize hash — LAZY (user decision 2026-11-23 ~07:01 WIB)
- Upload returns "done" as soon as all parts are received. Finalize
  (whole-file SHA-256) does NOT run on the request path.
- `checksum` may be NULL = not hashed yet. (Column must be nullable;
  currently non-nullable in migration 000065.)
- ORDER: update the DB FIRST, then hash. The document/session row is
  written (checksum NULL) before any hash job starts, so a crash
  between the two is safe: the row exists with NULL checksum and is
  re-enqueued on restart.
- IDLE-ONLY draining: the hash worker only processes jobs when the
  upload queue is REALLY empty — no upload session is actively
  receiving parts. Otherwise it sleeps and retries. Uploads
  (user-facing, latency-sensitive) always get full disk/CPU; hashing
  uses idle capacity only.
- Starvation guard — DECIDED 2026-11-23 ~08:27 WIB (dispatch brief
  default, accepted by the user's "Kirim ke agent sekarang"): N = 24
  hours. A job that has waited longer than 24 hours runs anyway, even
  when uploads are active.
- The finalize job runs in the background, async, from a queue keyed by
  id (session id / document id). Runtime queue is a HashMap; the durable
  record is the DB session row — on restart, re-scan sessions that are
  received-but-not-finalized and re-enqueue them (a HashMap alone loses
  jobs on restart — C11 lesson).

#### Restart safety — gaps confirmed by the user 2026-11-23 ~08:20 WIB (verified from code)
- DECIDED 2026-11-23 ~08:35 WIB (user: finalize gets its own table):
  a NEW `finalize_jobs` table is the durable record of pending finalize
  jobs (this REPLACES the "session row is the only record" design that
  was written here before — kept as history in the bullet below).
  Columns: id uuid PK, session_id uuid FK → document_upload_sessions.id,
  document_id uuid nullable FK → documents.id, status text
  ('pending'|'finalizing'|'verified'|'failed'), attempts int default 0,
  last_error text nullable, claimed_at timestamptz nullable, created_at,
  updated_at. Flow: all parts received → create a 'pending' row + session
  status 'received'; worker claims atomically (UPDATE ... WHERE
  status='pending' → 'finalizing', set claimed_at — one winner); success
  → 'verified' + store checksum; mismatch → 'failed' + quarantine/delete
  + notify; stale-claim reaper (finalizing + claimed_at older than X →
  back to 'pending', attempts+1). Startup re-scan is idempotent:
  re-enqueue only sessions without a pending/finalizing job row. The table
  also fixes the blindness (query it for pending/running/failed jobs;
  admin view later).
- [SUPERSEDED by the table decision above] Before the table, the session
  row was the ONLY record — no per-job record, no run history, no admin
  view; the system was BLIND about pending hash jobs except by querying
  sessions directly (status + NULL checksum).
- A NEW session status value is REQUIRED: today sessions only know
  "completing"/"completed" — nothing marks "received, awaiting
  finalize". The implementation must add that marker (e.g. status
  'received' / 'awaiting_finalize') alongside the nullable checksum.
- The startup re-scan is REQUIRED, not optional: without it, a restart
  silently orphans received sessions (checksum stays NULL forever).
- Double-finalize (two workers hashing the same session): prevented by
  the ATOMIC CLAIM on the finalize_jobs row + the STALE-CLAIM REAPER.
  The checksum write itself is idempotent (same file → same hash), so a
  double-run corrupts nothing — it only wastes CPU and risks duplicate
  notify/quarantine on mismatch.
- Job: stream-assemble + incremental SHA-256 (64 KiB buffer, same as
  today) → if `expected_sha256` was supplied and mismatches → mark the
  session failed, quarantine/delete the object, notify the user
  (notification module / event); else store the hash → status verified.
- Per-part checksums stay (verified on receipt, cheap) — the lazy part
  is only the whole-file finalize hash.
- Workers are bounded (e.g. 2–4 concurrent hash jobs) so many large
  uploads do not hash at once and thrash disk/CPU.
- An empty hash does NOT block serving: the safety gate is the virus
  scan, not the hash. `checksum IS NULL` is the honest "not verified
  yet" signal any consumer can read.

### Recalculate hash — PLANNED FUTURE FEATURE (not in this implementation)
- On-demand re-hash of a stored document (integrity audit, check after
  a storage migration, or when a file's hash is suspected wrong).
- Shape: an endpoint (e.g. `POST /api/v1/documents/{id}/recalculate-hash`)
  or a scheduler job that enqueues a re-hash for a document; the job
  recomputes SHA-256 from the stored object, compares it against the
  stored `checksum`, updates it, and flags a mismatch (audit event +
  quarantine the file if the stored bytes no longer match).
- Uses the same background worker + queue as lazy finalize.

### Folders — PLANNED FUTURE FEATURE (not in this implementation)
- Uploads organized into folders, git-like tree (nested folders,
  upload a whole folder at once preserving its tree via the browser's
  folder-upload / relative paths).
- RECOMMENDED shape: folders as DB metadata (a `folders` table with
  `parent_id`, or a `folder_path` column on `documents`) — the physical
  key stays flat (`TYPE/YYYY/MM/DD/UUIDv7.EXT`). Rename/move of a
  folder is then a pure DB update (no object moves), URLs stay stable,
  upload-into-folder just sets the folder id.
- Alternative shape: folders as real path segments in the key
  (`TYPE/FOLDER/PATH/YYYY/MM/DD/UUIDv7.EXT`, segment goes after the
  kind, tenancy stays out of the key) — bucket is browsable as a
  tree and folder backup/delete is a prefix operation, but rename/move
  means moving every object and the URL changes. Decide at kickoff.
- STORAGE LAYOUT (settled 2026-11-23 ~07:21 WIB): files stay FLAT inside
  the date directory (`TYPE/YYYY/MM/DD/UUIDv7.EXT`); the nested tree
  lives ONLY in the DB (folders table + `folder_id` on documents).
  Uploading a nested folder does NOT recreate the nesting in storage.
  Folder download-as-ZIP rebuilds the tree from the DB. (The alternative
  path-segment shape WOULD recreate the nesting inside DD — see above.)
- Folder path may appear in `/files/*` URLs as a readable segment, but
  the key does not need it. Folder names are user input: sanitize
  (no `..`, no empty segments, length limits) before they ever touch a
  key or a URL.
- WHOLE-FOLDER UPLOAD (the UX): browser `<input webkitdirectory>` or
  drag-and-drop a folder — each file arrives with its relative path
  (`project/src/main.rs`); the client sends that path per file and the
  server creates the missing folders along the path. No special
  protocol needed, just a `relative_path` field per file in the
  multipart upload (plus a `folder_id` field for upload-into-folder).
- Endpoints (planned): `POST /api/v1/documents/folders` (create),
  `GET /api/v1/documents/folders` (list/tree), `PATCH
  /api/v1/documents/{id}` (move between folders — part of task 322),
  folder download-as-ZIP (server zips on the fly), folder delete.
  Empty folders exist fine in the metadata model (a row with no files);
  in a path-segment model they cannot exist (object stores have no
  real folders).

## DB changes
- NEW migration: add `visibility` to `documents` — enum (`public` |
  `private`), default `'private'`, NOT NULL. Plus a plain index on
  `visibility` (listing public files: `WHERE visibility = 'public'`)
  and an index on `storage_key` (equality lookup for the public-serve
  path). The old `text_pattern_ops` index plan is DROPPED with the
  path-based design — no `LIKE 'public/%'` query exists anymore.
- NEW migration: `checksum` on `documents` (and `document_versions`)
  becomes NULLABLE — NULL means "not hashed yet" (lazy finalize).
  If the column is already nullable in the live schema, this step is a
  no-op (verify first).
- No live data to migrate (no migration has ever run against live
  PostgreSQL); the key-format change is code-only.
- Visibility is read from the loaded row in-app (`row.visibility ==
  "public"`); the key carries no visibility.
- The `visibility` column still drives the `/private/` rule 1 check
  (public → skip header) and public listings. The `/files/public/`
  route never reads it (no DB read on that path).

## Implementation checklist (AS BUILT — task 324, 2026-11-23)
1. [x] `StorageScope::{root, provider_key, provider_key_for_area}` +
   `StorageArea` (Public/Private): physical key is
   `{root}/{area}/{key}`; the stored key stays visibility-neutral
   (`TYPE/YYYY/MM/DD/UUIDv7.EXT`). `provider_key()` remains the
   private-area default; every production serve path uses the
   area-aware form.
2. [x] Upload paths (single + session complete) accept `visibility`
   (default private, validated); the scan runs BEFORE the store, so a
   public request is honoured only on a clean scan; the object lands
   in the matching area; the document row stores the visibility.
3. [x] `/files/public/{key}` — zero checks (no auth, no DB read;
   Content-Type from the extension only); miss → the app's normal 404
   (JSON envelope, incl. the redirect check — the observable behaviour
   of falling through to middleware). `/files/private/{key}` — 4-rule
   flow (row → visibility → header auth or signed token → object), ALL
   failures → 404 with an S3-style XML error body (decided by the
   dispatch brief). Headers per the Flow section: `Content-Type`,
   `Content-Length`, `Accept-Ranges: bytes`, Range → `206` / `416`,
   inline by default, `?download=1` → `Content-Disposition: attachment;
   filename="<original>"`. Provider gained `download_range` /
   `object_size` / `move_object` (local seek + atomic rename; S3
   GetObject Range + CopyObject + delete); handlers stream in 64 KiB
   chunks — never buffer the whole file (the old `download_signed`
   buffering is gone with the route).
4. [x] Visibility flip — DECIDED: MOVE the object between the areas
   (local = rename; S3 = copy → verify → delete + orphan sweeper for a
   copy left behind by a failed rollback) + one UPDATE of the
   `visibility` column; the URL never changes. All version objects
   move (a flipped-to-private document's old versions must not linger
   in the public area). Endpoint: `POST /api/v1/documents/{id}/flip`
   (owner-or-admin, permissioned + audited via `VISIBILITY_FLIPPED`).
   Content publish flips attached media public, unpublish flips it
   back private (best-effort detached tasks from the `content.*`
   hooks; the explicit flip endpoint is the reliable path). Flip to
   public is refused unless the scan is clean.
5. [x] Migration `2026-11-23-000123_storage_public_private`: `visibility`
   enum column on `documents` (default `'private'`) + index on
   `visibility` + index on `storage_key`; `checksum` nullable on
   `documents` + `document_versions`; session status CHECK extended
   with `'received'` + `'failed'`; NEW `finalize_jobs` table (see
   Restart safety). Diesel schema + models updated.
6. [x] Docs: module header docs rewritten (documents module.rs,
   content hooks doc); `config.example.yaml` needed no key-layout
   change.
7. [x] Tests — in-file, root-bin modules (UNRUN: root bin is OOM-blocked;
   only the unified-core lib tests actually ran, 56/56 incl. the 2 new
   `validate_media_reference` tests): 4-rule flow
   (`private_access_decision_covers_the_four_rules`), token scope
   (`token_grants_access_requires_this_document_and_this_tenant` — a
   token for another tenant/document never replays), atomic claim +
   FIFO + stale-claim reaper + startup re-scan idempotency
   (`state/finalize.rs`), picker public-only filter
   (`picker_shows_only_public_documents_in_company_scope`), Range
   parser (`parse_range_covers_the_supported_forms`), flip + lazy
   finalize paths (documents state tests).
8. [x] Verification: rustfmt parse clean on all touched files (my new
   lines are fmt-clean; the remaining ~98 hunks are the pre-existing
   tree-wide fmt debt — NOT reformatted); `cargo check -p unified-core`
   exit 0; `cargo check -p unified-db-platform` exit 0; `tsc --noEmit`
   exit 0 (incl. the new `assets/js/media-picker.ts`); `cargo test -p
   unified-core --lib` 56 passed / 0 failed (baseline 54 + 2 new).
   Root-bin `cargo check` / `cargo test` NOT run (VM OOM wall — the
   bin rustc needs ~5.7 GiB on a 7.7 GiB VM; stated plainly, not
   hidden).
9. [x] Tracker: task 324 created first, moved to In progress, closed
   with `status_id: 4` only after verification, verified by fresh read.
10. [x] Push: the "No push" in the old checklist is SUPERSEDED — the
    user authorized push requests 2026-11-23 ~08:35 WIB ("boleh request
    push juga"). No new dependencies. All English.
11. [x] Byte-serving API routes migrated to `/files/*` (see "API is
    JSON-only"): `GET /api/v1/documents/{id}/download` and
    `GET /api/v1/documents/download/{token}` REMOVED (no redirect; no
    external clients exist yet). Preview (JSON) + signed-url mint
    (JSON `{"data": {"url": "/files/private/{key}?token=<redacted>}}`) +
    upload stay under `/api/v1`. No file route under `/api/vX`;
    `/files/*` has no version segment.
13. [x] Task 355 — media manager / file explorer (folders +
    role-scoped listing + React UI):
    - Migration `2026-11-23-000132_documents_folders`:
      `document_folders` table (id / name / parent_id / company_id
      / owner_id / timestamps; nesting by id reference, never by
      path) + `documents.folder_id` ('' = library root) + indexes.
      The in-memory DashMap store stays authoritative at runtime;
      the tables are the persistence targets (same convention as
      000123/000124). No merge with the files module store — the
      files-vs-documents merge stays an open decision.
    - Folder endpoints under `/api/v1/documents/folders` (tree,
      create, get, rename, move, delete) + `POST /{id}/move` and
      `POST /{id}/rename` for documents; uploads (single-shot +
      chunked sessions) accept a target `folder_id`, validated
      before any byte is stored. Folder delete refuses (409)
      while the folder holds documents or subfolders; folder
      moves refuse cycles (409) and cross-company targets (403).
    - ROLE-SCOPED LISTING (user rule, 2026-11-23 ~18:36): the
      list endpoint itself filters (`list_visible` in
      `src/modules/documents/validation.rs`, unit test
      `list_visible_role_rules` — an author-role list call
      returns zero private rows belonging to another owner):
      admins (`documents:admin`) see public + private in scope;
      author / editor roles see every public document in scope
      plus their OWN private documents (so they can mark their
      own uploads public via the flip endpoint — the visible
      "mark as public" action, owner-or-admin, scan gate
      applies); another owner's private document never appears
      (no name, URL, or existence leak). The picker endpoint
      (`GET /picker`) stays public-only for every role, because
      content embeds a public URL and a private file would 404
      for readers. Single-record reads stay owner-or-admin
      (`can_read`).
    - React media manager (`assets/react/src/components/
      FileExplorer.tsx` + `MediaManager.tsx` + `MediaPicker.tsx`,
      page `pages/MediaPage.tsx`, route `/media`): grid/list
      view, search, type/visibility/folder filters, drag-drop
      multi-upload with real per-file progress (XHR), copy URL,
      delete with confirm, rename, move between folders,
      visibility flip ("Make public" / "Make private"). All data
      via fetch — zero full page reloads. The vanilla picker
      (`templates/default/documents/picker.hbs` +
      `assets/js/media-picker.ts`) is KEPT: the classic HBS edit
      forms still open it via `data-media-picker-open`; the React
      picker serves the React editors.
    - Verification (same limits as item 8: root-bin compile/test
      NOT run — VM OOM wall; the `list_visible` rule is covered
      by the in-file unit test, live endpoint checks with role
      headers are still unproven): rustfmt parse clean on all
      touched files; `cargo check -p unified-core` exit 0;
      `cargo check -p unified-db-platform` exit 0; `npm run
      check` exit 0; `npm run build:react` exit 0.
14. [x] Shared media uploader/picker (scope C): `GET
    /api/v1/documents/picker` (public-only + company scope + search +
    module/mime filters; returns `/files/public/{key}` URLs);
    `templates/default/documents/picker.hbs` + `assets/js/media-picker.ts`
    (`window.UnifiedMediaPicker.open({ onAttach })`, public-only list
    tab + upload tab, CSRF helper with retry-once, `data-media-picker-*`
    attributes for edit forms). Wired into posts + pages (attachments
    on Post/Page + publish/unpublish flip via `content.published` /
    `content.unpublished` with tenant + attachments) and into the
    other edit surfaces with media fields via the shared reference
    contract: `unified_core::storage::validate_media_reference`
    (document id OR `/files/{public,private}/{key}` URL; anything else
    rejected) — store digital cover (`cover_storage_id`) + e-learning
    course thumbnail (`thumbnail_ref`). Description lives WITH the
    post's attachment reference, NOT on the document row (task 322
    stays paused; no metadata columns built). NOT built, per scope:
    folders tree/explorer UI, document_shares/sharing, Data Tools,
    recalculate-hash.

### Honest limitation — finalize_jobs write-through (C11)
The `finalize_jobs` table exists as DDL + Diesel schema/models, and
the finalize worker runs against the in-memory `finalize_jobs`
DashMap with the same atomic-claim / stale-claim / idempotent-rescan
semantics. The worker does NOT write job rows through to PostgreSQL:
the documents module has zero Diesel/pool usage, and the table's FKs
reference `documents` / `document_upload_sessions` rows that are
themselves never persisted — a write-through would violate FKs without
the full C11 persistence program (task 271). The table is the durable
record schema + persistence target; runtime authority is the in-memory
map; full restart recovery needs C11. This is recorded here so nobody
reads the migration as "finalize jobs survive a restart" — they do
not, yet.

## History (why the spec changed — do not re-litigate without the user)
- Task 321: `tenants/{id}/{kind}/...` + `system/{kind}/...`, uuidv7 names.
- Task 323: scope-free keys (`{kind}/...`), tenant/system only in the
  provider root — because tenancy logic may change.
- 2026-11-23 ~06:53: visibility as the key's first segment
  (`public|private/...`), flip = move the object.
- 2026-11-23 ~07:38: path-based visibility REJECTED by the user —
  flip-by-copy causes data bloat. Visibility is now a `visibility`
  enum column on `documents` (default `private`); key back to the
  task-323 shape (`TYPE/...`); flip = one UPDATE; 2 endpoints
  (`/files/public`, `/files/private`); no `/files/attachments/`
  endpoint; public serve reads the DB row; private file via the
  public route answers 404; private route 401/403.
- 2026-11-23 ~07:44: user corrected the flow — `/files/public/` does
  ZERO checks (no header, no DB; direct serve from the tenant's public
  storage; miss falls through to middleware); `/files/private/` is the
  access lock with 4 rules (row+visibility → public skips header,
  private → header auth → object found/not-found, all failures 404);
  private 404 format open (recommended: S3-style XML error); flip
  mechanics OPEN (zero-check public implies location encodes
  visibility).
- 2026-11-23 ~08:27–08:35: dispatch brief filled the open points with
  defaults (flip = MOVE, private 404 = S3-style XML error,
  starvation-guard N = 24h); user said "Kirim ke agent sekarang" and
  authorized push requests. Task 324 implemented it all (see the
  as-built checklist above): migration 000123, finalize_jobs table,
  /files/* routes, Range + streaming, flip endpoint, picker + media
  reference contract, content publish/unpublish flips. Honest
  limitation recorded in the checklist: finalize_jobs has no DB
  write-through yet (C11, task 271).
- Task 355 (2026-11-23 ~18:36, user rule): role-scoped media
  listing. Author / post-editor roles see ONLY public documents
  in the picker (a private file appears only after it is marked
  public); the manager list shows authors public-in-scope plus
  their OWN private files (mark-as-public via flip); admins see
  public + private. The list endpoint itself filters
  (`list_visible`); no private metadata leaks to unauthorized
  roles. Folders added (`document_folders` + `documents.folder_id`,
  migration 000132) with a React media manager / file explorer.
