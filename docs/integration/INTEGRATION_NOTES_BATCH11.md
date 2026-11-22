# INTEGRATION NOTES — BATCH 11 (ERP backbone hardening)

Coordinator-verified. All six workers done. Nothing pushed. Remote head still `7cf6853c`.
Tree at verification: 174 `.rs` files, 398,563 Rust lines, 96 migration dirs.

## 0. What this round did (audit-first, fix in place — nothing rebuilt)

| Area | Files changed | Delta |
|---|---|---|
| API surface | `products.rs`, `api_manager.rs` | +~55, +~145 lines |
| Backup/restore | `backup.rs`, migration 000080 | +1,057 lines |
| Synchronization | `clusters.rs`, `app_sync.rs`, migration 000078 | +1,725, +124 lines |
| Import/export | `import_export.rs`, migration 000079, `INTEGRATION_NOTES_IMPORT_EXPORT.md` | +2,507 lines |
| Security | `security.rs`, `auth_manager.rs`, `hardening.rs`, `webhooks.rs`, `downloader.rs`, `dashboards.rs`, `config.example.yaml` | +320, +742, +115, +3, ~+120, +7, +8 lines |
| Storage + caching | `core/storage.rs`, `core/cache.rs`, `core/config.rs`, `config.example.yaml`, `modules/cache.rs`, `modules/storage.rs`, `storage_manager.rs`, `INTEGRATION_NOTES_BATCH11_STORAGE_CACHE.md` | +228, +631, +14, +8, +262, +229, +642 lines |

Coordinator verification (read-back, not compilation): all line counts above match the
workers' reports exactly; `grep` over all 17 changed files: 0 forbidden reference-project
name hits, 0 `todo!`/`unimplemented!`/`unreachable!`, 0 Indonesian word-boundary hits.
Spot-checks confirmed on disk: products 69 `.route(` vs 70 `RouteInfo` (the +1 is the
`*` catch-all, as the worker documented); backup 21 `require_permission` sites
(20 handlers + helper); import_export 28 sites (27 handlers + helper); TOTP routes and
password-login `jwt/issue` present in `auth_manager.rs`; `/sync/health`, `/sync/conflicts`,
`/sync/retries` routes present in `clusters.rs`; `tenant_key`, `FailoverCache`,
`delete_by_prefix` present in `core/cache.rs`; `storage.object.checksum_mismatch` present
in `storage_manager.rs`. No shared file (`mod.rs`, `schema.rs`, `models.rs`, `README.md`,
`TODO.md`) was edited by this round. `cargo check` has NEVER run anywhere — compilation
is unverified for everything below.

## 1. Scheduler arms to add (scheduler.rs — integrator / scheduler owner)

Add to `KNOWN_KINDS` + a dispatch arm for each (mirror the existing `backup.run` arm;
kind → `kind_requested` naming rule):

- `"backup.verify"` → emit `backup.verify_requested` (backup side already handles it;
  verifies runs stale per `backup.verify_interval_hours`, default 24h).
- `"backup.prune"` → emit `backup.prune_requested` (backup side already handles it).
- `"export.run"` → emit `export.run_requested` (import_export side already handles it;
  until this lands, scheduled exports only run via `run-now` / hook, and the
  `scheduler.enqueue` offer is refused).
- `"cluster.sync.retry_due"` → the clusters module already listens for this event and
  processes due retry entries (no arm name was given in the worker report; follow the
  same kind → `kind_requested` pattern, i.e. arm kind `cluster.sync.retry_due`).
- `"storage.byo.health_check"` is a batch-2/storage-product item, NOT batch 11 — listed
  here only so it is not confused with the four above.

## 2. AUDITED_EVENTS additions (audit.rs)

Add these event names to `AUDITED_EVENTS` (audit only records listed events):

- Import/export: all 16 `import.*` / `export.*` / `mapping.*` /
  `import_export.settings.updated` names (exact list in
  `INTEGRATION_NOTES_IMPORT_EXPORT.md`).
- Backup: `backup.verification_failed`, `backup.drill_failed`
  (plus the pre-existing `backup.failed` / `backup.completed` / `backup.restored`).
- Sync: `cluster.sync.conflict.resolved`, `cluster.sync.dead_letter`
  (plus `cluster.sync.retry.queued`, `cluster.sync.settings.updated` if the audit
  module wants them).
- Security: `auth.login` / `auth.failed` already feed the audit path via the login
  rewrite — confirm they are in `AUDITED_EVENTS`; if not, add them.

## 3. AppConfig backup section (src/core/config.rs — shared file, integrator)

The `backup.*` settings are currently dead config: `AppConfig` has no `backup` field,
so the section is silently ignored and every value is always the default. Add:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BackupSection {
    pub base_dir: String, pub drill_dir: String,
    pub max_total_bytes: u64, pub max_file_bytes: u64,
    pub default_retention_keep_last: u32, pub default_retention_keep_days: u32,
    pub verify_interval_hours: u64, pub encryption: String,
    pub pg_dump_bin: String, pub psql_bin: String,
}
// Defaults: base_dir "data/backups", drill_dir "data/backup-drills",
// max_total_bytes 268435456, max_file_bytes 67108864, keep_last 7,
// keep_days 30, verify_interval_hours 24, encryption "none",
// pg_dump_bin "pg_dump", psql_bin "psql".
// Add `pub backup: BackupSection` to AppConfig + Default impl.
```

The new keys (`drill_dir`, `verify_interval_hours`, `encryption`) follow the same read
path, so they activate with this snippet. Until it lands, all `backup.*` config stays
at defaults.

## 4. Schema / models mirrors (schema.rs, models.rs — integrator)

- Migration `000078_cluster_sync_reliability` (5 tables):
  `cluster_sync_conflicts`, `cluster_sync_retries`, `cluster_sync_idempotency_keys`,
  `cluster_sync_settings` (seeded with defaults), `cluster_sync_domains` (seeded with
  the 5 defaults). Column detail is in the migration's `up.sql`.
- Migration `000079_import_export_hardening` (3 new tables + columns):
  `mapping_templates`, `export_schedules`, `import_sessions`; new columns on
  `import_jobs` / `export_jobs`: `actor`, `company_id`, `pii_redacted`,
  `redacted_fields`, `row_limit`, `row_offset`, `has_more`,
  `rows_excluded_by_tenant_scope`. Column detail is in the migration's `up.sql`.
- Migration `000080_backup_drill_verify_tenant` (columns, no new tables):
  `company_id` on `backup_jobs` and `backup_runs`; `verified`, `last_verified_at`,
  `manifest_verified`, `verification_error`, `last_drill_at`, `drill_verified`,
  `encryption` on `backup_runs`; indexes on `company_id`, `last_verified_at`.
  WARNING: `000080` also exists as `000080_marketing` (batch 10) — see §7.

## 5. Capabilities to seed (roles)

New capability strings (fail-closed: endpoints 403 until a role grants them; no role
definitions were changed by this round):

- `backup:view`, `backup:manage` (all 20 backup routes).
- `clusters:view`, `clusters:manage` (all clusters admin handlers; app-sync admin
  handlers too).
- `import_export:read`, `import_export:import`, `import_export:export`,
  `import_export:export_pii`, `import_export:manage` (all 27 import/export routes).
- `cache.read`, `cache.write`, `cache.manage` (all 11 cache-module routes).
- `storage.read`, `storage.write`, `storage.manage_targets` (all 10 raw storage
  routes).
- `security:view`, `security:manage` (all 18 security routes; `*` / `admin:access`
  also grant).

## 6. Behaviour changes to communicate (breaking / gating)

- `POST /api/v1/auth-manager/jwt/issue` is now a real password login:
  was `{user_id, scopes?, expires_in_seconds?}`, now
  `{email, password, totp_code?, recovery_code?, expires_in_seconds?}`, response gains
  `scopes`. No in-repo callers of the old contract found — but any external client
  calling `jwt/issue` must be updated.
- 48 products routes now deny-by-default for keyed callers (need an allow rule +
  `products:read/write` scope or 403); keyless/session traffic unaffected.
- `list_access_rules` and `list_versions` now return standard `PageResult`
  (`{"items","pagination"}`) instead of `{"rules": [...]}` / `{"versions": [...]}`.
  Admin-only endpoints; no in-repo consumers of the old shapes found.
- 429 metadata key `retry_after_seconds` → `retry_after_secs` in api_manager paths
  (canonical name kept; `limit_per_minute` retained as alias).
- Backup / clusters / import-export / cache / storage-module routes now require
  capabilities (previously zero on backup 16, clusters 27, import/export 9, cache 11,
  storage 10 routes). Callers without the capabilities get 403 fail-closed.
- `security.trusted_proxies` config (default empty = peer address always wins).
  `X-Forwarded-For` is no longer trusted from unconfigured sources — deployments
  behind a proxy MUST set this or IP-based controls (blocklist, rate limiter,
  brute-force keys) will key on the proxy address.
- Session cookie gains `Secure` when TLS is configured (`ssl.listen`/`ssl.cert`
  non-empty).
- Backup encryption: mode `none` only; any other configured `backup.encryption`
  value refuses to run with an explicit error (fail-closed, never silent plaintext).
- `GET /sync/health` is a new aggregate endpoint; sync settings/domains are
  runtime-only until Diesel persistence is wired (edits lost on restart).

## 7. Migration collisions (verified by the coordinator at 2026-11-22 ~12:10 WIB)

Duplicate numbers on disk (verified by listing, not by report): **000056, 000070,
000075, 000077, 000080, 000081** — plus `000100_calculator` out of sequence.
Batch 11's own numbers: 000078 (sync), 000079 (import/export), 000080 (backup).
`000080` collides with batch 10's `000080_marketing` — renumber ONE of them.
Earlier claims of 000068 ×2 and 000074 ×2 are STALE (verified single). Numbers are
still churning (batches 12–15 were running in parallel per parent memory) — re-verify
at merge time; renumber only the colliding dirs, never others.

## 8. AI credential encryption — exact fix snippet (ai.rs, integrator)

`ai.rs` encrypts credentials with a home-grown XOR stream cipher + HMAC tag, not AEAD
(the code itself says to replace it when an AEAD crate is added; no AEAD crate is in
`Cargo.toml`). Do NOT edit ai.rs while another round owns it — apply when free:

1. Add `aes-gcm = "0.10"` to `[dependencies]` in `Cargo.toml`.
2. Replace `encrypt_api_key` / `decrypt_api_key` in `src/modules/ai.rs` with
   AES-256-GCM (32-byte key = SHA-256 of the secret, 12-byte random nonce, output
   `v2.<base64url(nonce || ciphertext || tag)>`).
3. Keep the `v1.` XOR path in `decrypt_api_key` for already-stored values and
   re-encrypt on next write, exactly as the existing comment prescribes. Do NOT reuse
   the XOR keystream afterwards. Unverified until `cargo check`.

## 9. OPEN list (consolidated — integrator / follow-up rounds)

1. `cargo check` / `cargo build` / `cargo test` / `cargo fmt` NEVER ran anywhere
   (no toolchain, crates.io blocked). Compilation unverified for all six areas.
   Highest-risk compile points (from workers): `auth_manager.rs` Diesel
   `QueryableByName` + `sql_query(...).bind::<Array<Uuid>, _>`; `downloader.rs`
   `reqwest::redirect::Policy::custom` closure; `hardening.rs`
   `AtomicU64::fetch_update`; `security.rs` `&web::Data<Arc<AppContext>>` →
   `&AppContext` deref coercion.
2. **Repo-wide per-handler auth** (biggest follow-up): most modules still trust the
   API-Manager gate, which passes keyless internal traffic. The three highest-risk
   surfaces are fixed (security admin, token minting, recovery codes) plus backup /
   clusters / import-export / cache / storage-module routes in this round. The rest
   needs a dedicated round, module by module, WITH a compiler. Pattern: read
   `X-User-Id` + `X-Role`, ask `role.check` via the hook bus, fail closed — or the
   stronger JWT pattern now in `security.rs`.
3. No security control in this round is proven by a live test — code-reviewed, not
   penetration-tested. No login attempt, lockout, token-mint, SSRF probe, restore,
   drill, two-node sync, retry, or idempotency replay was executed end-to-end.
4. Backup at-rest encryption needs an AEAD crate (e.g. `aes-gcm`) + envelope
   encryption before upload, or an SSE flag on `StorageProvider::upload`.
5. Backup jobs/runs still in-memory (restart loses them) — needs Diesel persistence
   against the 000009 + 000080 tables. Same for sync conflicts/retries/idempotency/
   settings/domains (000078 tables exist as migration only) and import/export stores
   + settings (000079).
6. SSRF TOCTOU rebinding: webhook delivery and downloader creation both
   resolve-then-fetch; DNS can change between check and connect. Fully closing it
   needs IP-pinned connections (custom resolver / `to_socket_addrs` + connecting to
   the validated IP with Host/SNI preserved).
7. Inbound webhooks: no receiver exists; when built it must use the existing
   `verify_signature` (constant-time, `sha256=` header) plus a timestamp window —
   the helper enforces no replay window on its own.
8. Downloader `destination` is an arbitrary local path (no confinement to a download
   directory) — needs a config decision (base directory); until then its safety rests
   on handler auth (see item 2).
9. Security settings runtime-only (reset on restart); TOTP recovery codes + pending
   secrets in-memory (restart silently invalidates enrolments in flight).
10. Owning-module wiring (pre-existing): no consumer for `import.completed` staged
    rows; no row provider for `export.requested` in users/software/store/billing/
    tasks/licensing. `export.requested` now also carries `limit`/`offset`/`company_id`
    — providers should honour all three and tag rows with `company_id`.
11. Scheduled-run PII: import/export runs use the capability captured at creation; a
    role revoked afterwards is not re-checked at run time (run-now re-checks live).
12. Storage/caching leftovers: pre-existing local/SFTP objects without a sidecar are
    served unverified until re-uploaded or verified once; Drive re-upload overwrite
    delta misses (pre-existing identity quirk); move/archive paths don't reserve plan
    quota (narrow over-admit window); config-declared storage targets carry no
    target-level quota in the raw module (hub is the quota path);
    `redeem_download` intentionally unauthenticated (token possession is the
    authorization).
13. Version gating is rule-driven only; no module declares `route_version_support`
    restrictions. Dashboard HTML page routes pollute the API registry (cosmetic).
14. `has_more` on chunked exports is a heuristic (exactly-`limit` rows returned).
