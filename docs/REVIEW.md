# Review-and-Adapt Round — Reference Implementation

Date: 2026-11-22. Method: read the reference implementation (plain file viewing
only), review each area, then adapt into Unified only what is genuinely stronger
than what Unified already had. Nothing was copied file-by-file; every adaptation
was written as new code in this repository. The reference implementation is never
named in this repository. Stack decisions were kept throughout: Actix-web 4,
Diesel + PostgreSQL, Argon2 passwords.

## Process note

During this round, several builders briefly used shell commands (`ls`, `grep`,
`find`, `wc`, `head`, `python3`) against the reference tree before a reminder
arrived; one builder's first probe was a whole-filesystem `find` that traversed
the reference directory. All of them stopped that method immediately after the
reminder and used plain file viewing only afterwards. No reference file was ever
run, built, tested, copied, moved, or imported. This is recorded here for
honesty; the adaptations below come from reading only.

## Verdicts

### 1. Session tokenizer — SKIPPED (Unified equal or better)

Reference: stateless 208-hex-char token, nested HMAC-SHA256, user id + timestamps
+ expiry + 16-byte nonce, constant-time verification.

- Unified's `src/auth/session.rs` has the same layout and is strictly better
  where they differ: current-second tokens are valid (the reference's
  `>= now` expiry check flakes on freshly minted tokens), `generate` uses
  `saturating_add`, `parse` returns errors instead of `.unwrap()`, no
  `generate_unchecked` panic path, and `generate` takes `u64` so a negative-id
  guard is impossible by construction.
- Rejected: age-based `is_expired_with` (buggy and caller-less in the
  reference). Follow-up idea (not implemented): a corrected absolute max-age
  check to bound indefinite sliding renewal, and a cookie-lifetime margin of
  15 minutes over the renew window so renewal can fire before the cookie dies.

### 2. Flash messages — SKIPPED (Unified equal or better)

Reference: three-level in-memory manager (session → kind → items), DashMap +
atomics, same TTL/threshold constants as Unified (5–60 s, default 10 s;
threshold 10–1000, default 100), per-item ids, type-erased metadata,
`move_flash` / `merge_flash`, throttled background recycling.

- Unified's `src/auth/flash.rs` enforces the per-kind threshold as a real cap
  by evicting the oldest message (the reference only prunes expired items, so
  a flood of unexpired messages grows unboundedly), has true consume-on-read,
  and a working periodic cleanup.
- Verified defects in the reference, all reasons to skip: `add_arc` silently
  drops a message when a `FlashItems` already exists for that kind (only the
  first message per kind per session is kept); the background recycler is dead
  code (an async function called without `.await`, so the spawn never runs);
  `move_flash`, `merge_flash`, metadata, and the age-based expiry helper have
  zero callers in the reference.
- Follow-up idea (not implemented): when a session/cookie middleware is built,
  decide how flash survives token rotation on login (the reference's
  `move_flash` concept, implemented against Unified's correct manager).

### 3. SSL storage / SNI resolver — PARTLY ADAPTED

Reference: three maps (predefined / defaults / auto), key-match check by
sign-and-verify, SAN parsing, single-level wildcard derivation, 300-byte
minimum file sizes, silent config errors.

- Skipped (Unified already equal or better): key-match check (identical idea
  already in `tls.rs`); SAN parsing (Unified handles multi-byte lengths and
  SEQUENCE recursion; the reference uses a single-byte length with unchecked
  indexing); wildcard lookup (Unified walks all parent levels; the reference
  tries only one); `add()` wildcard handling; minimum file sizes (would reject
  valid ~220-byte P-256 keys); silent config errors (Unified collects them).
- Adapted in `src/http/tls.rs` (+21 lines, now 508): self-signed fallbacks
  moved into a separate `auto_created` map, so the 8192-entry cap eviction can
  never drop a configured or ACME-renewed certificate; fixed a real bug where
  `remove()` deleted configured defaults despite its docstring; added
  `certificate_names()` exposing leaf CN+SANs for renewal.

### 4. ACME — ADAPTED

Reference: multi-provider (Let's Encrypt, Google, ZeroSSL, BuyPass, Custom),
EAB, authorization deactivation on failure, challenge-error checks, SAN
preserving renewal with a clamped 7–90 day window and a force flag, identity
dedup, chain parsing / leaf detection, IP identifiers.

- Adapted in `src/http/acme.rs` (+250 lines, now 540): `AcmeProvider` enum
  with per-provider staging URLs, wildcard/IP capability flags, and EAB
  (`new_with_provider`, `new_with_directory`; ZeroSSL without EAB rejected
  upfront); `set_ready()` only on pending challenges, challenge errors
  honoured, authorizations deactivated on failure (rate-limit hygiene);
  challenge tokens cleaned up via a Drop guard on every exit path (they
  previously leaked on error); renewal re-requests every renewable DNS name
  from the old cert (previously only the triggering domain, silently dropping
  e.g. `www.`), window clamped to 7–90 days, plus a forced-renewal variant.
- Skipped: post-`set_ready` immediate-validity check (over-strict vs the
  upstream async flow; `poll_ready` waits for it); key reuse on renewal
  (Unified does not retain private key material; a fresh key per renewal is
  standard practice); chain/leaf parsing (Unified already validates the leaf);
  IP identifiers (HTTP-01-only; needs config surface + challenge work).

### 5. Downloader — ADAPTED (rewritten)

Reference: capacity-bounded registry, per-item metadata bundle, `Status` +
`can_transition_to`, `ResumableState`, one cached reqwest client per item,
default headers, retry policy, progress (percentage, oversize detection, retry
count, timestamps), clamped metadata (retries/redirects/timeouts), staging +
move-into-place saver, multi-algorithm hashing.

- Adapted in `src/modules/downloader.rs` (rewritten, now 1,201 lines):
  `DownloadStatus` enum with validated transitions (illegal transitions
  impossible by construction); retry loop with exponential backoff (500 ms ×
  2ⁿ, cap 30 s) honouring `Retry-After` (cap 60 s); retryable vs terminal
  error classification; per-job clamped fields (`max_retries` 0..=10,
  `timeout_secs`, `connect_timeout_secs`, `max_redirects`, `follow_redirect`,
  `max_bytes`); true resumability (`resumable` from `Accept-Ranges` / 206,
  `Content-Range` total parsing, stored ETag/`Last-Modified` sent back as
  `If-Range`, stale oversized partials dropped); atomic finalisation (stream
  to `<destination>.part`, `sync_all`, verify checksum, then rename);
  `progress_percent`, `eta_seconds`, `retry_count`, timestamps; new
  `POST /jobs/{id}/retry` endpoint; sha512 support; constant-time checksum
  comparison; checksum verification now streams in 64 KiB chunks (the old
  code loaded whole files into memory).
- Skipped: lifecycle `fn`-pointer callbacks (Unified emits hook-bus events);
  `DownloadGuard` cancel-on-drop (cancellation is explicit via flag +
  endpoint); reqwest 0.13 built-in retry (Unified pins reqwest 0.12, and it
  cannot resume from a partial file anyway); `danger_accept_invalid_certs`;
  custom DNS pinning (owned by the network module); `ftp://`/`file://` URLs
  (http/https only, SSRF safety); the reference's large commented-out
  `execute()` (dead code with wrong duration math); MD5.

### 6. Cache adapters — ADAPTED (new layer)

Reference: typed `CacheValue` enum + pool-trait shape, filesystem adapter
(sharded by key hash, JSON envelope), Redis adapter (`SETEX`, batched reads),
lazy pool with clamped config, deferred-save queue, `QueueGuard`.

- New `src/core/cache.rs` (790 lines): `CacheValue` (Text/Number/Boolean/
  Bytes/Object) with conversions, async `CacheAdapter` trait, `FilesystemCache`,
  `RedisCache`, `Cache` facade, `build_cache`. Filesystem entries are sharded
  by sha256 (not sha1) with atomic temp-file + rename writes; the reference
  writes directly to the target (crash corrupts the entry) and joins the raw
  key into the path (traversal risk for keys containing `/` or `..`).
  Redis uses an async multiplexed connection (not a blocking r2d2 pool) and a
  prefix-scoped SCAN clear (the reference's `FLUSHDB` wipes the whole shared
  database). Keys validated (non-empty, ≤250 chars, no control chars or
  whitespace). Bad Redis config logs a warning and falls back to filesystem
  so boot never fails on cache config.
- New `src/modules/cache.rs` (195 lines): `CacheModule` with
  `GET/POST /api/v1/cache`, `GET/DELETE /api/v1/cache/{key}`,
  `POST /api/v1/cache/clear`. Registered in `src/modules/mod.rs`.
- Wiring: `CacheSection` + `cache` field in `src/core/config.rs` (now 607
  lines), `pub cache: Arc<Cache>` in `src/core/context.rs` (now 137 lines),
  `redis.enabled` flag on `RedisSection`, `redis = "1.0"` with `tokio-comp`
  added to `Cargo.toml` (only new dependency), `cache:` section in
  `config.example.yaml` (now 149 lines).
- Skipped: PSR-6-style two-level item/pool model with deferred save/commit
  (complexity with no use case); `is_regenerating` stampede protection
  (follow-up); `QueueGuard`; the reference's buggy URL-building for Redis
  connection info (Unified builds `ConnectionInfo` structurally).

### 7. i18n / l10n — ADAPTED

Reference: CLDR-style plural rules, gettext adapter (`.po`/`.mo` via magic
bytes, `msgctxt`/plural storage), memory adapter, translator, ISO data files
(country/language/currency/timezone), `date.rs` formatting.

- Adapted in `src/modules/i18n.rs` (558 → 1,349 lines, +791): corrected
  CLDR plural families (no-plural; French/Portuguese 0-and-1 singular; Slavic,
  Polish, Czech/Slovak, Romanian, Lithuanian 3-form; Arabic 6-form) with
  language-only matching and finite/negative-count handling; gettext-style
  lookup with the standard `\x04` msgctxt separator; minimal pure-std `.po`
  parser (`msgid`/`msgid_plural`/`msgctxt`/`msgstr[n]`, continuations,
  escapes, fuzzy skipped, `Plural-Forms` header read) with
  `POST /api/v1/i18n/catalog/po`; locale negotiation (`normalize_locale`,
  `parse_accept_language` with q-values, `negotiate_language` +
  `GET /languages/negotiate`); compact ISO 4217 table (34 major currencies)
  with `currency_info`, `format_currency`, `GET /currencies`.
- Verified reference bugs (reasons to reimplement, not copy): the gettext
  key-join order is inconsistent (`add_from_catalog` joins `id\0context`
  but `get()` joins `id\0context\0domain`), so gettext-loaded entries are
  unreachable whenever a domain is set; `msgstr()` stores the singular even
  for plural entries; the translator truncates every locale to 2 chars (loses
  region/script), silently drops English, and discards any translation equal
  to the source string (breaks no-plural languages such as Indonesian);
  plural `abs()` on signed minimums can panic; French `0` treated as plural.
- Skipped: full ISO 3166 country/language tables (conflate country with
  language; multi-language countries wrong by construction); static ISO 8601
  timezone snapshot (goes stale with DST; no `chrono-tz` added); `date.rs`
  (chrono already covers formatting); `.mo` binary parsing (would need a new
  dependency); `from_country_data` (picks an arbitrary currency for
  multi-currency countries); `find_by_symbol` (`$` is ambiguous).

### 8. IDNA / domain parsing and Geo — ADAPTED

Reference: `domain.rs` (IDNA via the `url` crate, localhost fast path,
local-suffix list, URL/email host extraction, 253-char limit), `psl.rs` /
`tld.rs` (full PSL / IANA TLD lists), `libs/maxmind.rs` (license validation +
download-URL construction only, explicit `@todo` — no DB loading),
`net/ip.rs` (bogon/CIDR helpers), `net/dns.rs` (cached resolvers, 5 s timeout,
`IpChoice`, CNAME).

- Adapted in `src/modules/geo.rs` (945 → 1,238 lines, +293): full IP
  classification (`classify_ip` / `is_bogon` / `is_public_ip` — private,
  loopback, link-local, multicast, unspecified, broadcast, documentation
  RFC 5737/3849, benchmarking RFC 2544/5180, reserved, CGNAT RFC 6598,
  unique-local, discard, deprecated site-local); IPv4-mapped IPv6
  normalization (`::ffff:8.8.8.8` → `8.8.8.8`); `Cidr::range()` /
  `Cidr::from_range()` which reject non-aligned ranges instead of widening
  them; corrected `normalize_ipv4` leading-zero handling; MaxMind coordinate
  validation; MaxMind license-key shape validation (40 chars, 3 underscore
  parts, 6-char alphanumeric prefix, `mmk` suffix).
- Adapted in `src/modules/network.rs` (895 → 1,151 lines, +256):
  `normalize_domain_input` (bare domain, trailing dot, port, full URL →
  host, IDNA to ASCII; rejects empty input, IP literals, `@` input);
  strict `domain_from_email`; localhost fast path, `is_local`, explicit
  "IP address, not a hostname" issue; DNS verifier gains `IpChoice`,
  `lookup_cname`, `lookup_ptr` (PTR via `in-addr.arpa.` / `ip6.arpa.`),
  explicit 5 s resolver timeout + 2 attempts.
- Verified reference bugs: `normalize_ip4` errors on nearly every input (it
  checks the whole-string length `> 3`); `ipv4_to_cidr_string` silently
  widens non-aligned ranges into a covering CIDR; `is_bogon` misses
  benchmarking, reserved, CGNAT, and documentation ranges.
- Skipped: full PSL / IANA TLD bundling (needs a dependency + data file +
  update mechanism; the reference fetch code has no HTTP status check, no
  persistence, fragile version parsing, and panics on cache failure; the TLD
  whois lookup does blocking TCP to port 43 with no connect timeout and an
  unbounded read); quoted-string/comment email local-part parser (allow-list
  includes CTL characters); DoH provider enum and the `reqwest::Resolve`
  impl (its port split breaks IPv6 literals).

### 9. System / process stats — ADAPTED

Reference: `/proc/stat` per-CPU deltas + `ctxt`/`btime`/`processes` counters,
`/proc/self/statm` and `/proc/self/stat` own-process parsing,
`/proc/self/status` identity/memory structs, `/proc/self/mounts` parsing with
device classification, `/proc/cpuinfo` parsing, central cached `StatsRecord`
(1 s cadence, loopback excluded), `ServerStats` aggregate shape, remote
polling TUI over a control socket, categorized `server info` rendering.

- Adapted in `src/cli/monitor.rs` (now 938 lines, ~+130): tiered sampling
  cadence (CPU/memory/processes/network every 1 s; disks every 30 s via cached
  rows; static CPU details once at startup — previously disks refreshed every
  tick); process table with state, owner UID, CPU, RSS, virtual memory,
  cumulative disk I/O, run time, sort toggle (`c` CPU / `m` memory); disk
  kind (SSD/HDD) + read/write bytes-per-second; network packets RX/TX and
  error counters; Server panel with CPU model, frequency, physical cores,
  load average, swap.
- Adapted in `src/modules/service_monitor.rs` (now 710 lines, ~+240): shared
  1 s snapshot cache (one `System` + `Disks`; previously every `/status` and
  `/info` request ran `System::new_all()`); `/status` gains global CPU %,
  swap, load average, own-process CPU % / virtual memory / state / run time;
  `/info` gains CPU frequency/vendor, swap, disk kind + cumulative I/O; new
  endpoint `GET /api/v1/service-monitor/processes?limit=&sort=`.
- Skipped: raw `/proc/stat` counters and `/proc` parsers (sysinfo 0.32 covers
  them); peak RSS / RssAnon / VmSwap (no sysinfo API); credential/signal/
  namespace structs; volume labels and readonly flags (sysinfo exposes
  neither); remote-polling TUI architecture (Unified reads the local status
  file; simpler, no control protocol); user/group lookup (panics on missing
  users); `IS_CARGO` ppid/`comm` sniffing.
- Verified reference bugs: `CpuUsage::__force_reparse` releases the wrong
  lock; `CpuLoad::set_data` mis-assigns the CPU index; `format!("{:.2} GHz",
  "N/A")` formats a `&str` with float precision (yields `"N/"`).

### 10. Hooks / middleware / routes / assets / themes — ADAPTED

Reference: typed filter-chain hooks (priority, once, removal, re-entrancy
guard) + tokio-broadcast event manager; `Middleware` trait + manager +
Actix `Transform` dispatcher; `Routes` collection with orchestrators and
method helpers; asset registry with dependency-ordered enqueue/dedupe,
attribute validation/escaping, `asset_url` helper; theme validation
(manifest, required templates, 5 MiB size cap), logical template-name →
file mapping, per-theme language files.

- Adapted in `src/core/hooks.rs` (180 → 402 lines): priorities, once
  handlers, unregister, re-entrancy guard, detailed listing — keeping the
  existing handler signature, JSON payloads, and event names. `register` now
  returns the handler id (`u64`); all existing call sites are `;`-terminated
  statements, so this is source-compatible.
- New `src/core/middleware.rs` (238 lines): Middleware trait + manager +
  Dispatcher Transform, wired into `http/server.rs`. Unified previously had
  no way for a module to add middleware at all — this closes a real gap.
  The dispatcher sits inside the existing `wrap_fn`, so pause/maintenance
  blocking and request hooks still run first.
- Adapted in `src/core/modules.rs` (217 → 267 lines): `RouteInfo`,
  `Module::route_info()`, `Module::register_middleware`, `route_catalog()`,
  `register_all_middleware`. The reference's route registry itself was
  skipped (Unified's `ServiceConfig` approach is more flexible); only the
  missing metadata piece was ported. Unmatched routes now return the standard
  `{"error": {...}}` 404 envelope via `default_service` instead of Actix's
  plain-text 404.
- New `src/themes/assets.rs` (680 lines): AssetRegistry/AssetQueue rewritten
  from the reference's ideas, fixing its flaws — deterministic `Vec` ordering
  (the reference iterates `HashMap`s, so tag order varied per request),
  topological dependency resolution with cycle detection, dedupe, attribute
  validation + escaping, SHA-256 content fingerprinting (`?v=<hash>`) with
  immutable cache headers (which the reference lacks entirely).
- Adapted in `src/themes/engine.rs` (560 → 677 lines): template-name mapping
  via optional `template_files` in the manifest (backward-compatible),
  5 MiB template/partial size cap, `assets_dir`, `asset_url` helper.
  `src/http/server.rs` (now 438 lines) serves `/assets/` and
  `/theme/{id}/assets/` — the built-in dashboard CSS/JS previously had no
  serving route at all (404).
- Skipped: typed `Arc<T>` filter-chain model (would break every handler and
  the JSON decoupling); tokio broadcast manager (no async subscribers); the
  orchestrator/group/method-helper layer (duplication); `Package` bundles
  (no caller); per-theme `.mo` files (the i18n module owns translations);
  multi-theme container (Unified's single-active-theme + atomic swap is
  stricter); CWD-relative manifest lookup; screenshot-as-URL.

### 11. JSON helpers and database query builder — MOSTLY SKIPPED, ONE ADAPTATION

Reference: `JsonResponse { debug }` building `JsonSuccess { data }` or
`JsonError { error_code, message, trace }` (different shape when not in
debug: `{"code","error"}`); `serde_json::Error::classify()` mapping;
1095-line hand-rolled `ErrorType` taxonomy; hand-rolled SQL string builder
on SQLx (`Expr` enum, `Comparison` with only `=`,`<>`,`<`,`<=`,`>`,`>=`,
`AndX`/`OrX`, `Func` with `AVG`/`COUNT` only, raw-string joins/order/group),
`Entity` trait, multi-driver adapter.

- The envelope was NOT adapted: the reference's debug vs non-debug shapes
  differ, so clients cannot rely on one shape. Unified is strictly better:
  success exactly `{"data":…}`, error exactly
  `{"error":{"message","code","metadata"}}` + `trace` in debug only.
- The query builder was NOT adapted and must never replace Diesel: it builds
  SQL by string interpolation, not bind parameters (values rendered into SQL
  text; UPDATE placeholders never actually bound — `sqlx::query(&sql)` runs
  with no arguments). It has no INSERT, OFFSET, DISTINCT, subqueries, or
  transactions; `Composite` auto-wrapping is heuristic string sniffing;
  `AndX` additions via `add_raw`/`add_and`/`add_or` are silently dropped from
  output (they mutate `inner` but `Display` renders `parts`);
  `record_clean_state()` returns `New` (copy-paste bug); `dynamic_entity.rs`
  is an empty stub.
- Adapted in `src/core/error.rs` (116 → 170 lines, +54) — the only change in
  this area: `From<serde_json::Error>` now classifies the error (`Io` → 500,
  `Syntax`/`Data`/`Eof` → 400; previously everything was 400) with the
  category recorded as `metadata.kind`; new `From<diesel::result::Error>`
  (`NotFound` → 404, `UniqueViolation` → 409, FK/check/not-null → 422, else
  500) with generic client messages and the full error only in debug `trace`.
  The envelope is unchanged. `src/core/json.rs` was deliberately untouched.
- Sweep (all skipped with reasons): `size_to_bytes` / `format_size` /
  `format_duration` (humantime already a dependency); `lc_first`/`uc_first`
  one-liners and `clean_unique_path` (path-clean already a dependency);
  hand-rolled hash helpers (MD5 must not be introduced; `sha1`/`sha2`/`hmac`
  already used inline); base64/base32 wrappers (already dependencies);
  random/uuid/semver generators (already direct dependencies); manual
  calendar arithmetic (chrono covers it); `ToJson` trait (serde covers it);
  log-level parsing (tracing `EnvFilter` covers it); `file_info.rs` 870
  lines (std::fs + nix cover it); OS user lookup (no caller); hand-rolled
  HTTP status enum (Actix `StatusCode` used everywhere); `net/ip.rs` bogon
  helpers (geo.rs already has its own); i18n date/country/currency tables
  (chrono + the i18n module cover them); runtime schema introspection
  (Diesel migrations + `table!` cover it); `libs/` (acl, image, lz4, tar,
  zip, multipart, redis, maxmind, download saver, mime — Unified already has
  equivalents in use).

## Files added / changed in this round

| File | Change |
|---|---|
| `src/core/cache.rs` | New (790 lines) — cache facade + filesystem/Redis adapters |
| `src/modules/cache.rs` | New (195 lines) — cache HTTP module (35th module) |
| `src/core/middleware.rs` | New (238 lines) — middleware trait/manager/dispatcher |
| `src/themes/assets.rs` | New (680 lines) — asset registry/queue + fingerprinting |
| `src/core/error.rs` | 116 → 170 lines — error classification + Diesel conversion |
| `src/core/hooks.rs` | 180 → 402 lines — priorities, once, unregister, re-entrancy guard |
| `src/core/modules.rs` | 217 → 267 lines — route info, middleware registration, catalog |
| `src/core/mod.rs` | 15 → 17 lines — `cache`, `middleware` modules |
| `src/core/config.rs` | → 607 lines — `CacheSection`, `redis.enabled` |
| `src/core/context.rs` | → 137 lines — `pub cache: Arc<Cache>` |
| `src/modules/mod.rs` | → 96 lines — cache module registered |
| `src/modules/downloader.rs` | Rewritten, now 1,201 lines |
| `src/modules/i18n.rs` | 558 → 1,349 lines |
| `src/modules/geo.rs` | 945 → 1,238 lines |
| `src/modules/network.rs` | 895 → 1,151 lines |
| `src/cli/monitor.rs` | → 938 lines — tiered cadence, process/disk/network panels |
| `src/modules/service_monitor.rs` | → 710 lines — snapshot cache, new `/processes` endpoint |
| `src/http/tls.rs` | → 508 lines — separate auto-created map, `remove()` fix |
| `src/http/acme.rs` | → 540 lines — multi-provider, EAB, failure handling, SAN renewal |
| `src/http/server.rs` | → 438 lines — dispatcher, JSON 404, asset routes |
| `src/themes/engine.rs` | 560 → 677 lines — template mapping, size cap, `asset_url` |
| `src/themes/mod.rs` | → 6 lines — `assets` module |
| `src/modules/theme.rs` | → 723 lines — `route_info()` |
| `src/modules/licensing.rs`, `company.rs`, `users.rs` | +1 line each — `cache` field in `stash_ctx` |
| `templates/default/layout.hbs` | → 34 lines — assets via `{{asset_url ...}}` |
| `Cargo.toml` | → 103 lines — added `redis = "1.0"` (`tokio-comp`) |
| `config.example.yaml` | → 149 lines — `cache:` section, `redis.enabled` |

Totals: 90 files (was 86), 70 Rust files (was 66), 41,195 lines (was 36,068).

## Verification (coordinator, independent of builders)

- Forbidden reference-project name in the whole tree: **0 hits**.
- Indonesian prose in repo files: **0 hits** (only the pre-existing native
  language name "Bahasa Indonesia" in the i18n catalogue).
- `todo!` / `unimplemented!` macros: **0**.
- JSON envelope intact by reading: success exactly `{"data": ...}`,
  error `{"error":{"message","code","metadata"}}` + optional debug `trace`.
  Pagination, hooks (now with priorities/once), CLI (`-t`, `--monitor`),
  and config test path (`config.rs::test`) still present in code.
- `cargo check` / `cargo build` did **NOT** run — no cargo/rustc in this VM
  and crates.io is blocked by the egress proxy. Compilation is unverified.

## Known risks and follow-ups

- Riskiest unverified API surfaces: `Dispatcher` Transform machinery in
  `src/core/middleware.rs` (generic `Transform`/`Service` bounds, `BoxBody`
  conversions, `unsafe get_unchecked_mut` adapter); Handlebars 6 `HelperDef`
  closure bound for `asset_url`; `AuthorizationHandle::deactivate()` /
  `ChallengeHandle.status` / `.error` / `ExternalAccountKey::new()` under
  instant-acme 0.8; `From<diesel::result::Error>` match ergonomics on
  `DatabaseErrorKind`; redis 1.0 (`ConnectionInfo` / `MultiplexedConnection` /
  `scan_match` stream / `set_ex` return types); reqwest 0.12 redirect and
  error-classification APIs; sysinfo 0.32 (`process.user_id()` → `Uid:
  Display`, `DiskKind::Unknown(_)` tuple variant, `Disk::usage()` /
  `Process::disk_usage()`, network packet/error getters); hickory-proto
  `RData::PTR` tuple struct + `Display`; `ResolverOpts { timeout, attempts }`.
- `ThemeEngine` still has no live caller in the binary (pre-existing); the
  asset serving routes are live, the registry/queue are library-ready.
- Redis adapter has no reconnect logic: if the multiplexed connection drops,
  later operations fail until restart. Cancel during downloader backoff sleep
  can take up to 30 s to be noticed. First `/status` request after startup
  reports 0 for CPU % (two-sample requirement).
- `redis = "1.0"` must resolve in the lockfile; if the offline mirror lacks
  it, pin `redis = "0.32"` (same API surface used here) as a fallback.
- Follow-ups noted during review (not implemented): corrected absolute
  max-age check for sessions; cookie-lifetime margin over the renew window;
  flash survival across token rotation; cache stampede protection
  (`is_regenerating`); reconnect-on-error wrapper for Redis.
