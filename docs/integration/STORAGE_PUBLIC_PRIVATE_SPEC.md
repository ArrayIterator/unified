# Storage layout — public/private implementation spec (FINAL, user-settled 2026-11-23)

Status: SPEC ONLY — implementation awaits the user's "gas" before any agent is
dispatched. Do not implement from this file until then.

## Final layout (physical = provider root + stored key)

```
Tenant public area:   storage/tenants/TENANT-ID/public/...
Tenant private area:  storage/tenants/TENANT-ID/private/...
System:               storage/system/...
Temp:                 storage/temps/uploads/YYYY/MM/DD/SESSION-ID/000001.part
```

The tenant has TWO storage areas: a public area and a private area.
`/files/public/` serves the public area directly (zero checks);
`/files/private/` is the gated route (see Flow section). The exact key
shape inside each area is OPEN (part of the flip question): either the
task-323 shape `TYPE/YYYY/MM/DD/UUIDv7.EXT` inside each area
(visibility in the provider root), or visibility as the key's first
segment. The flip mechanics decide it.

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
- OPEN POINT (starvation guard): on a system with constant uploads,
  an idle-only queue can starve a job forever. Recommended: a job
  that has waited longer than N hours runs anyway (or runs in a
  nightly maintenance window). Confirm the value of N with the user.
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

## Implementation checklist (for the agent brief)
1. `build_object_key` + `StorageScope::{root, provider_key}`: the public
   area and private area are separate roots (exact placement decided
   with the flip question); the stored key stays visibility-neutral
   until that decision lands.
2. Upload paths (single + session complete) accept visibility (default
   private), store it in the `visibility` column, and land the file in
   the matching area.
3. New `/files/{visibility}/...` route + handler (branch on segment):
   public = direct serve from the public area, no auth, no DB, miss →
   fall through to middleware; private = 4-rule flow (row → visibility
   → header auth → object), all failures → 404 (XML error format per
   the user's pick). Response headers per the Flow section above:
   `Content-Type`, `Content-Length`, `Accept-Ranges: bytes`, Range →
   `206` / `416`, inline by default, `?download=1` →
   `Content-Disposition: attachment`. Provider gains a ranged-read
   method (local seek / S3 GetObject Range) and the handler streams in
   chunks — never buffers the whole file.
4. Visibility-flip operation (OPEN — move between areas vs column-only;
   see Flow section), wired to the future PATCH metadata endpoint
   (task 322) or its own endpoint; content publish/unpublish flips it.
5. Migration: add `visibility` enum column (default `'private'`) +
   index on `visibility` + index on `storage_key`; add nullable
   `checksum` (lazy finalize). Diesel schema updated (new column).
6. Docs: module header docs + `config.example.yaml` comments if they
   mention key layout.
7. Tests: public serve without auth and without DB, public miss falls
   through to middleware, private serve of a public file without auth
   (Rule 1), private serve 404 without auth / with invalid auth /
   wrong scope (uniform 404), private serve with valid auth, flip
   (mechanics per decision), Range 206/416.
8. Verification: rustfmt parse on touched files; grep zero
   `public/`/`private/` first-segment key building and zero
   object-move flip code; no `cargo check -p unified` / root-bin tests
   (OOM wall — state plainly); `cargo test -p unified-core` only if
   core files are touched (baseline 54/0).
9. Tracker: create a new task (task 323 is closed; this is new work), close
   with `status_id: 4` only after verification, verified by fresh read.
10. No push. No new dependencies. All English.
11. Migrate the byte-serving API routes to `/files/*` (see "API is
    JSON-only" section): retire `GET /api/v1/documents/{id}/download`
    and `GET /api/v1/documents/download/{token}` (removal; no external
    clients exist yet — the API is not public). Preview + signed-url
    mint + upload stay (JSON).

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
