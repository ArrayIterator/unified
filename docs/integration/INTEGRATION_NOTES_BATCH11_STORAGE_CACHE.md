# Integration notes — Batch 11: storage + caching hardening

Audited the existing storage abstraction (`src/core/storage.rs`, the
storage-manager hub, the backup providers) and the cache layer
(`src/core/cache.rs`, the cache module). Fixed the gaps in place; nothing
was rebuilt. No migration is needed: all new state is in-memory
(DashMap) or in the config file (serde defaults), and no database table
changed. No push was made.

## What changed, by file

| File | Lines before → after | Change |
| --- | --- | --- |
| `src/core/storage.rs` | 2366 → 2594 | Trait `download_verified` / `verify_checksum`; durable SHA-256 sidecars for local + SFTP with verification on read; sidecars hidden from `list`; `.sha256` keys reserved on local/SFTP; `storage.object.checksum_mismatch` event; optional per-target `quota_bytes` / `max_objects` on `StorageTargetConfig` (validated) |
| `src/core/cache.rs` | 790 → 1421 | Tenant-scoped keys + methods; `get_or_compute` stampede protection; `FailoverCache` (Redis primary, filesystem fallback); `delete_by_prefix` / `invalidate_prefix` / `invalidate_tenant`; filesystem entry bound + prune; Redis SETEX sub-second rounding; `MAX_KEY_LEN` 250 → 512 (headroom for the tenant prefix) |
| `src/core/config.rs` | +14 | `CacheSection.max_entries` (default 10000, 0 = unlimited), `CacheSection.failover_enabled` (default true) |
| `config.example.yaml` | +8 | Documents the two new cache keys |
| `src/modules/cache.rs` | 205 → 467 | Capability checks on all 11 routes (`cache.read` / `cache.write` / `cache.manage` via `role.check`, fails closed); status reports `failover_active` + `max_entries`; new routes `POST /prune`, `POST /invalidate`, tenant-scoped `GET/POST/DELETE /tenant/{tenant}[/{key}]`, `POST /tenant/{tenant}/invalidate` |
| `src/modules/storage.rs` | 458 → 687 | Capability checks on all 10 routes (`storage.read` / `storage.write` / `storage.manage_targets`); uploads return `sha256_hex` and record it in the event; downloads accept `?expected_sha256=` (verified, corrupt bytes refused); per-target quota enforced on upload (overwrite counts only the size delta) |
| `src/modules/storage_manager.rs` | 8271 → 8913 | Plan quota enforced on the upload write path; `check_quotas` soft-warning early-return fixed; check + reserve under `quota_guard` (TOCTOU closed for explicit quotas; plan reservations tracked in `plan_reserved`); overwrite delta accounting everywhere; SHA-256 recorded on `StoredObject` / `ByoObject` and verified on download (`X-Object-Integrity` header); new `POST /buckets/{id}/objects/verify` and `POST /buckets/{id}/objects/orphans/cleanup` (dry-run by default); BYO quota delta-aware + reserved before upload |

New routes total: 8 (6 cache, 2 storage-manager). Storage module route
count unchanged (10). All new/changed routes carry a capability check;
the two new storage-manager routes use the existing `storage_manager.browse`
(verify) and `storage_manager.manage_buckets` (orphan cleanup) capabilities.

## New hook events

- `storage.object.checksum_mismatch` (core storage events + storage-manager
  events): emitted when a download or verification finds bytes whose SHA-256
  does not match the recorded checksum. The corrupt bytes are never served.

## Behaviour notes for the integrator

- Local/SFTP integrity sidecars are `<key>.sha256` files next to the object.
  They are written atomically; a failed sidecar write removes the object
  again rather than leaving unverified bytes. `list()` hides them, and keys
  ending in `.sha256` are rejected on those two providers only.
- S3 / R2 / Drive have no provider-side checksum record: verification there
  runs through `download_verified` with the caller-supplied digest (the
  storage-manager records it at upload). This is documented in the trait.
- Objects written before checksums existed (or outside the hub) have no
  record: hub downloads serve them with `X-Object-Integrity: unverified`,
  and `POST .../objects/verify` backfills the record from the bytes read.
- The cache `role.check` capabilities (`cache.read`, `cache.write`,
  `cache.manage`) and storage capabilities (`storage.read`,
  `storage.write`, `storage.manage_targets`) are new permission strings;
  roles must grant them or the endpoints answer 403 (fail closed). No role
  definitions were changed here.
- Cache `MAX_KEY_LEN` is now 512. Plain `get`/`set` reject keys starting
  with `tenant:`; tenant entries are only reachable through the
  tenant-scoped methods.
- No module currently caches application data through the facade (verified
  by audit: the only callers are the cache module itself). Any module that
  starts caching MUST invalidate on its write paths (`delete`,
  `invalidate_prefix`, `invalidate_tenant`) or document its staleness
  window; the invalidation primitives now exist.
- Plan quota on upload uses `buyer_usage` (recorded hub + BYO objects) plus
  in-flight reservations. Plan provider-kind gating is enforced too: a plan
  whose `included_provider_kinds` is non-empty rejects uploads to a bucket
  whose kind is not listed (409, scope `plan`).
- `delete_bucket` still deliberately leaves provider bytes in place (see
  its comment). Use the orphan cleanup endpoint on a live bucket first;
  after a bucket is deleted its provider config is gone, so provider-side
  purge must happen before deleting the bucket.

## Migration collisions observed (not mine — integrator must renumber)

Direct listing of `migrations/` shows duplicate numbers: 000056,
000070, 000075, 000077, 000080, 000081; plus 000100_calculator out of
sequence. This round added no migration.

## Verification done by this round (read-back only)

- Delimiter-balance check passed on all 6 changed Rust files.
- Greps: 0 `todo!` / `unimplemented!` / `unreachable!`; 0 Indonesian
  words in the changed files; 0 references to the forbidden reference
  project name (no such string was written anywhere).
- Capability coverage: cache module 11/11 handlers authorize, storage
  module 10/10 handlers authorize, both new storage-manager handlers
  authorize; every `check_quotas` / `record_stored` /
  `check_byo_operation` call site matches the new signatures.
- NOT verified: compilation. No Rust toolchain exists in this VM and
  crates.io is blocked; `cargo check` has never run here. The integrator
  must run `cargo check` / `cargo fmt` before pushing.

## Open items

1. Live compile + `cargo fmt` + `cargo clippy` (blocked: no toolchain).
2. Pre-existing local/SFTP objects without a sidecar are served
   unverified until re-uploaded or verified once (verify endpoint
   backfills the hub record; the local sidecar is only written on
   upload — a verify sweep does not backfill sidecars).
3. Drive uploads return a file id as the stored key, so the hub's
   overwrite-delta lookup (keyed by the request key) misses on Drive
   re-uploads and counts them as new objects. Pre-existing identity
   quirk, unchanged.
4. Move/archive paths do not reserve plan quota (only the direct upload
   path does); a concurrent upload racing a move can briefly over-admit
   past a plan quota. Moves do not change totals, so the window is
   narrow.
5. Config-declared storage targets carry no target-level quota in the
   raw storage module (only runtime-created targets do); quota-sensitive
   use must go through the storage-manager hub.
6. The storage-manager `redeem_download` route remains intentionally
   unauthenticated (token possession is the authorization) — pre-existing,
   not changed here.
