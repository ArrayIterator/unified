# Integration Notes — Batch 16 (Calendar + Global Holiday Sync)

Coordinator-verified by read-back. Nothing pushed. Remote head untouched
(`7cf6853c`). Do NOT edit these files in place — apply the snippets below.

## 1. What landed

| Item | File | Lines | Routes | Migration | Tables |
|---|---|---|---|---|---|
| Calendar (aggregation view) | `src/modules/calendar.rs` (new) | 2,667 | 13 | `2026-11-22-000095_calendar` | 3 |
| Holidays (global sync) | `src/modules/holidays.rs` (new) | 4,780 | 27 | `2026-11-22-000098_holidays` | 6 |

Totals: **7,447 new Rust lines, 40 new routes, 9 new tables, 2 migrations.**
Tree at verification: `src/modules/` = 137 `.rs` files, 408,801 lines;
106 migration dirs (batch 18's `000099_cards` landed during this round).

Coordinator verification (read-back, not trusted reports):
- calendar.rs 2,657 lines as built, 2,667 after the coordinator fix (§9);
  `.route(` = 13, `RouteInfo {` entries = 13. 0 stub macros, 0
  forbidden-name hits, 0 Indonesian word-boundary hits.
- holidays.rs 4,780 lines; `.route(` = 27 (a naive grep says 28 — line 225
  is a comment containing the literal `.route(`), `RouteInfo {` = 27.
  0 stub macros, 0 forbidden-name hits, 0 Indonesian word-boundary hits.
- Both migrations: CREATE TABLE == DROP TABLE (3 and 6).
- Calendar's AI registration matches batch 14's contract exactly:
  `register_ai_capabilities("calendar", CALENDAR_AI_CAPABILITIES)` and the
  `AiCapability` struct fields (`id, module_id, name, description,
  input_fields, allows_pii, use_case`) match `src/modules/ai.rs:4203`.

## 2. Module registration (`src/modules/mod.rs`)

```rust
pub mod calendar;
pub mod holidays;
```

In `register_all_modules` (same area as the other batch-16-era modules):

```rust
    calendar::register(registry);
    holidays::register(registry);
```

Both modules expose `pub fn register(registry: &ModuleRegistry)`.
Declarations: id `"calendar"` / `"holidays"`, `available_as_product = true`,
`product_category = Some("productivity")`, `is_core = false`.
`holidays` declares `requires = ["users", "roles", "notifications",
"scheduler"]`; `calendar` has no hard module requirements (it degrades
gracefully when no module answers its hooks).

## 3. Schema / models (`src/db/schema.rs`, `src/db/models.rs`)

Tables are persistence targets only — both modules keep the in-memory
DashMap store authoritative (todos/library precedent). Mirror the tables
below; no FKs to other modules' tables anywhere (enforced by module code).

### 000095_calendar

```sql
calendar_feeds (
    id UUID PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    company_id TEXT,
    user_id TEXT NOT NULL,
    token_hash TEXT NOT NULL,          -- SHA-256 of the issued token, never the raw token
    modules JSONB NOT NULL DEFAULT '[]'::jsonb,
    label TEXT,
    expires_at TIMESTAMPTZ NOT NULL,
    revoked BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at TIMESTAMPTZ
);
-- indexes: (tenant_id, user_id), (token_hash), (expires_at)

calendar_settings (
    id UUID PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    scope TEXT NOT NULL CHECK (scope IN ('user', 'company')),
    owner_id TEXT NOT NULL,
    weekend_days JSONB NOT NULL DEFAULT '[6, 7]'::jsonb,  -- ISO 1=Mon..7=Sun
    working_hours_start TEXT NOT NULL DEFAULT '09:00',
    working_hours_end TEXT NOT NULL DEFAULT '17:00',
    default_view TEXT NOT NULL DEFAULT 'month'
        CHECK (default_view IN ('month', 'week', 'day', 'agenda')),
    timezone TEXT NOT NULL DEFAULT 'UTC',
    holiday_country TEXT NOT NULL DEFAULT '',
    create_link_templates JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, scope, owner_id)
);
-- index: (tenant_id)

calendar_sources (
    id UUID PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    module TEXT NOT NULL,
    visible BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, user_id, module)
);
-- index: (tenant_id, user_id)
```

### 000098_holidays

```sql
holiday_providers (  -- platform reference data, NO tenant id
    id TEXT PRIMARY KEY CHECK (char_length(id) BETWEEN 1 AND 64),
    name TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('nager_date', 'calendarific', 'abstract_api', 'google_ical', 'generic_ical')),
    base_url TEXT NOT NULL,
    allowed_hosts JSONB NOT NULL DEFAULT '[]'::jsonb,
    api_key_required BOOLEAN NOT NULL DEFAULT FALSE,
    api_key_encrypted TEXT CHECK (api_key_encrypted IS NULL OR api_key_encrypted LIKE 'v1.%'),
    key_fingerprint TEXT CHECK (key_fingerprint IS NULL OR char_length(key_fingerprint) = 12),
    min_interval_secs BIGINT NOT NULL DEFAULT 1,
    max_requests_per_minute BIGINT NOT NULL DEFAULT 60,
    calendar_ids JSONB NOT NULL DEFAULT '{}'::jsonb,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    builtin BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

holiday_countries (  -- platform reference data, ISO 3166-1 alpha-2, 249 seeded
    code TEXT PRIMARY KEY CHECK (char_length(code) = 2),
    name TEXT NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

holiday_sync_configs (  -- tenant business data
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL,
    company_id TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    fallback_provider_ids JSONB NOT NULL DEFAULT '[]'::jsonb,
    countries JSONB NOT NULL DEFAULT '[]'::jsonb,
    years JSONB NOT NULL DEFAULT '[]'::jsonb,
    schedule TEXT NOT NULL,
    feed_url TEXT,                      -- only for generic_ical configs
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
-- indexes: (tenant_id), (provider_id)

holiday_sync_runs (  -- tenant business data, one row per (provider, country, year) sync
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL,
    company_id TEXT NOT NULL,
    config_id UUID NOT NULL,
    provider_id TEXT NOT NULL,
    country TEXT NOT NULL CHECK (char_length(country) = 2),
    year BIGINT NOT NULL CHECK (year BETWEEN 2000 AND 2100),
    status TEXT NOT NULL CHECK (status IN ('success', 'failed')),
    trigger TEXT NOT NULL CHECK (trigger IN ('manual', 'scheduled', 'sync_all')),
    holidays_found BIGINT NOT NULL DEFAULT 0,
    holidays_stored BIGINT NOT NULL DEFAULT 0,
    error TEXT,
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    fetched_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
-- indexes: (tenant_id), (config_id), (provider_id, country, year)

holidays (  -- shared reference data, keyed by country+year, NO tenant id
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    country TEXT NOT NULL CHECK (char_length(country) = 2),
    date DATE NOT NULL,
    name TEXT NOT NULL,
    local_name TEXT,
    provider_id TEXT NOT NULL,
    source TEXT NOT NULL DEFAULT 'synced' CHECK (source = 'synced'),
    global BOOLEAN NOT NULL DEFAULT TRUE,
    types JSONB NOT NULL DEFAULT '[]'::jsonb,
    counties JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT uq_holidays_country_date_name UNIQUE (country, date, name)
);
-- indexes: (country, date), (provider_id)

holiday_overrides (  -- tenant business data; at most one per (tenant, country, date)
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL,
    company_id TEXT NOT NULL,
    country TEXT NOT NULL CHECK (char_length(country) = 2),
    date DATE NOT NULL,
    name TEXT NOT NULL,
    override_kind TEXT NOT NULL CHECK (override_kind IN ('day_off', 'working_day')),
    note TEXT,
    created_by TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT uq_holiday_overrides_tenant_country_date UNIQUE (tenant_id, country, date)
);
-- indexes: (tenant_id), (country, date)
```

Per-tenant placement: the holidays migration SQL is schema-agnostic; create
these tables inside the tenant schema via `tenancy::run_tenant_migrations`
(batch 17, on disk). The calendar migration tables also carry `tenant_id`
and move into the per-tenant schema the same way (see §8).

## 4. Routes (all in `route_info()` already — register the modules, no extra work)

### Calendar — `GET/POST /api/v1/calendar*` (13)

| Method | Path |
|---|---|
| GET | `/api/v1/calendar` (view=month\|week\|day\|agenda, from/to, modules, timezone) |
| GET | `/api/v1/calendar/items` (paginated, filtered) |
| POST | `/api/v1/calendar/create-intent` |
| GET / PUT | `/api/v1/calendar/settings` |
| GET / PUT | `/api/v1/calendar/sources` |
| POST / GET | `/api/v1/calendar/feeds` |
| DELETE | `/api/v1/calendar/feeds/{id}` |
| GET | `/api/v1/calendar/feed/{token}` (public iCal; `.ics` suffix accepted) |
| POST | `/api/v1/calendar/ai/summarize-day`, `/api/v1/calendar/ai/whats-next` |

### Holidays — `/api/v1/holidays*` (27)

Providers: `GET/POST /providers`, `GET/PATCH/DELETE /providers/{id}`,
`POST /providers/{id}/test`. Countries: `GET/POST /countries`,
`PATCH/DELETE /countries/{code}`. Configs: `GET/POST /configs`,
`GET/PATCH/DELETE /configs/{id}`, `POST /configs/{id}/sync`,
`POST /sync-all`. Runs: `GET /sync-runs`, `GET /sync-runs/{id}`.
Holidays: `GET /` (list), `GET /is-holiday`, `GET /non-working-days`.
Overrides: `GET/POST /overrides`, `GET/PATCH/DELETE /overrides/{id}`.

## 5. Hook contracts (exact — do not rename)

### Calendar emits — owning modules must answer

**`calendar.items_requested`** — request:
`{"tenant_id", "company_id", "user_id", "start", "end", "modules"?}`
(RFC 3339 UTC bounds). Each answering module contributes ONE result: a JSON
array of items, or `{"items": [...]}`. Shared item shape:
`{id, module, title, start, end?, all_day, color?, label?, deep_link?,
visibility ("private"|"shared"|"public"), owner_user_id?, company_id?,
tenant_id?}`. Calendar merges, sorts by start, dedupes by `(module, id)`,
and enforces visibility server-side (private items only to their owner;
foreign-company / foreign-tenant items silently excluded). No answerer →
empty list, not an error. **Nobody answers this yet** — todos, appointments,
events, meetings, webinars, academic, projects, etc. must implement it;
until they do the calendar returns empty views.

**`calendar.create_requested`** — request `{"tenant_id", "module", "start",
"end", "title?", "user_id", "company_id"}`; answer `{"url": "..."}`.
Fallback: configurable deep-link templates (`{start}`/`{end}`/`{title}`,
admin-editable via settings; built-in defaults for todos/appointments/
events/meetings, clearly marked). Neither → 503. Never creates a record.

**`calendar.ai.draft_requested`** — request `{"tenant_id", "user_id",
"role", "kind" ("summarize_day"|"whats_next"), "date", "item_ids",
"context_text", "requested_at"}`; answer `{"draft": "..."}` (or
`{"text": ...}`), optional `model`/`provider`. No answer → 503, never
fabricated. Drafts only. Capabilities `summarize_day` / `whats_next` are
registered via `register_ai_capabilities` (verified against batch 14's
`AiCapability` registry in `ai.rs` — field-for-field match).

### Calendar consumes

**`holidays.is_holiday`** — see §5 holidays. The calendar passes
`{"date", "country", "tenant_id"}` (tenant id REQUIRED — the holidays
handler answers nothing without it; this was FIXED by the coordinator,
see §9). No handler registered → weekends only, never an error.

### Holidays answers

**`holidays.is_holiday`** — request `{"date": "2026-12-25", "country":
"ID", "tenant_id": "<uuidv7>"}` (`tenant_id` REQUIRED; absent/invalid → no
answer, fail closed). Answer, exactly one result: `{"date", "country",
"is_holiday": bool, "name": string|null, "source": "synced"|"manual"|
"not_configured", "provider": "<provider-id>"|"manual"|null}`. Manual
overrides win; a `working_day` override cancels a synced holiday
(`is_holiday: false, source: "manual"`).

**`holidays.non_working_days`** — request `{"start", "end", "country",
"tenant_id"}` (tenant_id REQUIRED; range clamped to 366 days). Answer,
exactly one result: `{"start", "end", "country", "in_scope": bool,
"dates": [{"date", "name", "source", "provider"}]}`; country not selected by
any enabled config of the tenant → `{"dates": [], "in_scope": false}`.

Consumers (via events only, never direct calls): calendar (non-working
days), HR (leave/attendance), academic (class schedules), appointments
(slot generation skips holidays), projects (working-day calculations).

### Holidays emits

`holidays.provider/config/override.created|updated|deleted`,
`holidays.sync.started|finished|failed` (payloads carry ids only, never
keys). Sync failures also emit `notification.requested` once per config
(aggregated); previously synced holidays stay cached.

### Scheduler listeners (registered in both modules)

- `calendar.feeds.prune_requested` — deletes expired tokens + revoked
  tokens older than 30 days.
- `holidays.sync_requested` (`{"config_id","tenant_id"}`) /
  `holidays.sync_all_requested` (`{"tenant_id"}`) — run in spawned tasks.

## 6. Scheduler (`src/core/scheduler.rs` — KNOWN_KINDS + dispatch arms)

Add these kinds (none are in `KNOWN_KINDS` yet — both modules decline
today with `scheduler_status: "declined"` recorded honestly):

```rust
"calendar.feeds.prune"   // -> emit calendar.feeds.prune_requested
"holidays.sync"          // -> emit holidays.sync_requested
"holidays.sync_all"      // -> emit holidays.sync_all_requested
```

## 7. Capability seeds (roles)

- `calendar:read`, `calendar:manage` (feeds/settings), `ai:use` on the two
  AI endpoints. Public feed (`GET /api/v1/calendar/feed/{token}`) uses
  tenant resolution + HMAC token verification instead of a capability.
- `holidays:read`, `holidays:manage`, `holidays:sync`.
- Cross-tenant reads answer 404, never 403 (fail-closed ownership rule,
  todos.rs precedent). No tenant context → 403 on authenticated handlers.

## 8. Tenant scoping (standing rule)

- **Holidays — CONVERGED.** Batch 17's plumbing landed mid-task as
  `src/core/tenancy.rs` (`schema_name()`, `schema_name_for_str()`,
  `TenantContext::from_http_request`, `with_tenant_connection`,
  `run_tenant_migrations`). `resolve_tenant` returns
  `tenancy::TenantContext` (401 missing context / 403 non-UUID id, fail
  closed); `Caller` carries the `schema_name` derived only by
  `schema_name()` (marked `#[allow(dead_code)]` until the Diesel layer
  consumes it); all 4 hook handlers reject non-UUID `tenant_id` payloads
  via `schema_name_for_str()`. Name-collision note: `tenancy::TenantContext`
  vs `middleware::TenantContext` — imported aliased as `TenancyContext`.
  Still open: only the Diesel persistence wiring — the in-memory store is
  authoritative and issues no SQL, so there is no `with_tenant_connection`
  call site yet. When wired, it takes the tenant Uuid from `resolve_tenant`
  into `tenancy::with_tenant_connection(pool, tenant_uuid, …)`; migration
  tables are created inside the tenant schema by
  `tenancy::run_tenant_migrations` (SQL is schema-agnostic).
- **Calendar — convergence point documented, not yet swapped.** Batch-17
  plumbing was NOT on disk when the calendar builder finished (verified:
  no `schema_name` helper anywhere in `src/` at that time). Per
  instructions it did NOT invent its own schema-name derivation. The
  single swap point is `resolve_tenant` in `calendar.rs` (marked with a
  `CONVERGENCE POINT` comment in the file header): when batch 17 lands,
  the integrator extends that one function to also return the schema name
  / search_path handle and re-points the three migration tables (already
  carrying `tenant_id`) at the per-tenant schema. No handler, record shape,
  or hook contract changes. Caveat: fail-closed tenant resolution means
  control-plane (main-host) requests with no resolved tenant get 403 from
  this module today — serving the calendar on the control plane needs an
  explicit policy decision, never a silent `public` fallback.
- **Batch-17 items the integrator must fix first** (from batch 17's own
  report, recorded here because both modules touch the same seam): (1)
  batch 17 B2's seam does not match B1's real tenancy API (assumed
  `TenantContext::new` / `context.schema()` / `create_tenant_schema` /
  `run_tenant_migrations` — none exist); (2) batch 17 migration CHECK
  rejects real schema names (up.sql line 91: `^tenant_[0-9a-f_]{36}$` vs
  actual `tenant_` + 32 hex — must become `^tenant_[0-9a-f]{32}$`).

## 9. Coordinator fix applied (already in the files — do not re-apply)

B2 found that `calendar.rs` emitted `holidays.is_holiday` WITHOUT
`tenant_id`, so the holidays handler would answer nothing and every
calendar view would silently fall back to weekends only. Fixed by the
coordinator in `calendar.rs` (+10 lines, 2,657 → 2,667):
`non_working_days` now takes `tenant_id: &str`, the emit payload includes
`"tenant_id": tenant_id`, and the single call site passes
`&caller.tenant_id`. The file-header doc example was also corrected:
`source` is `"synced"`/`"manual"` with the provider id in a separate
`"provider"` field (the old example showed `"source": "nager-date"`).
Re-verified after the edit: 0 stub macros, single call site, tenant id
present in the emit.

## 10. Migration numbering

- `000095_calendar` — SINGLE (batch 14 renumbered `saas_packages` to
  000096 during this round; the earlier 000095 collision self-resolved —
  verified by fresh listing).
- `000098_holidays` — SINGLE. 000091 library, 000092 entitlements, 000093
  product_subdomains, 000094 tenant_isolation, 000096 saas_packages,
  000097 pricing, 000099 cards (batch 18) all taken by parallel rounds.
- Full duplicate list for the integrator (verified across batch 12/13/14
  listings): **000056, 000070, 000075, 000077, 000080, 000090** (×2 each)
  + **000100_calculator** out of sequence. 000068/000074/000081 verified
  SINGLE. Renumber into a gapless sequence before any migration run.

## 11. Provider registry (holidays — seeded data, all admin-editable)

Definition: `{id, name, kind, base_url, allowed_hosts[], api_key_required,
key_configured, key_fingerprint, min_interval_secs, max_requests_per_minute,
calendar_ids{}, enabled, builtin}`. Kinds: `nager_date`, `calendarific`,
`abstract_api`, `google_ical`, `generic_ical`. Seeded (`builtin: true`,
editable/disableable, not deletable):
- **nager-date** (default) — `https://date.nager.at`, no key, 1s / 60rpm.
- **calendarific** — keyed, 1s / 60rpm.
- **abstract-api** — `https://holidays.abstractapi.com/v1`, keyed, 1s / 60rpm.
- **google-ical** — no key, 2s / 30rpm, 12 seeded per-country calendar ids
  (DATA, not code).
- **generic-ical** — no base URL; feed URL lives on the config, its host
  becomes the fetch allowlist.
Countries: full ISO 3166-1 alpha-2 list seeded as data — 249 entries,
249 unique, 0 duplicates. Keys are write-only, encrypted with the ai.rs
`v1.` HMAC-keystream format (same secret source `app.secret_key`, same dev
fallback string), never in any GET response, log, or error. SSRF follows
downloader.rs exactly, stricter (https only, allowlist host, safe DNS,
every redirect re-validated, max 10). Rate limits enforced per provider;
429 honours Retry-After (cap 60s); 3 attempts max with exponential
backoff. Sync dedupes by (country, date, name), upserts in place, never
duplicates; runs recorded per (provider, country, year). iCal parsing is
hand-rolled (unfold + VEVENT DTSTART/SUMMARY). Provider response shapes
are documented in the file header as **from provider documentation, not
live-verified**.

## 12. ApiAccessRule rows (api_manager — allow-deny, deny by default)

Add one row per route in §4 (method + path + module `calendar` /
`holidays`), default deny, scopes `calendar:read` / `calendar:manage` /
`holidays:read` / `holidays:manage` / `holidays:sync` as applicable.
The public feed `GET /api/v1/calendar/feed/{token}` is token-gated, not
key-gated — do not put it behind an ApiAccessRule that requires an API key.

## 13. AUDITED_EVENTS additions

`calendar.view.requested`, `calendar.feed.created`, `calendar.feed.revoked`,
`calendar.ai.draft_requested`, `holidays.sync.started`,
`holidays.sync.finished`, `holidays.sync.failed`,
`holidays.override.created`, `holidays.override.updated`,
`holidays.override.deleted`, `holidays.provider.created`,
`holidays.config.created`.

## 14. README / TODO lines

README (modules table): add `calendar` — "Integrated aggregation calendar
(month/week/day/agenda, per-module filters, revocable iCal subscriptions,
AI day summary)" and `holidays` — "Global public-holiday sync (Nager.Date
default, Calendarific, Abstract API, Google/iCal feeds, manual overrides)".
TODO: record the OPEN list (§15) and the `calendar.items_requested` /
`calendar.create_requested` adoption checklist (todos, appointments,
events, meetings, webinars, academic, projects — each module answers with
its dated items; until then the calendar shows empty views).

## 15. OPEN list (explicit — integrator / later rounds)

1. `cargo check` / `cargo build` / `cargo test` NEVER ran anywhere —
   compilation unverified for both modules. Highest-risk compile points
   (builders' own notes): calendar `DashMap::retain` closure signature,
   `RefMut` deref-clone in `verify_feed_token`, `web::Query<PaginationParams>`
   deref coercion in `list_feeds`; holidays Diesel `QueryableByName`-
   style shapes are NOT used (no SQL issued) — risk is the reqwest /
   HMAC-keystream code paths.
2. No module answers `calendar.items_requested` or
   `calendar.create_requested` yet — calendar returns empty views /
   template fallback until owning modules implement the contracts.
3. Scheduler kinds `calendar.feeds.prune`, `holidays.sync`,
   `holidays.sync_all` are NOT in `KNOWN_KINDS` (§6) — the listeners exist
   but nothing fires them today.
4. No live fetch ever ran for any holiday provider — all provider shapes,
   Google calendar ids, and the 12 seeded `calendar_ids` are from
   documentation, not live-verified; first live `POST
   /providers/{id}/test` per provider is still open.
5. Encryption is HMAC keystream + tag, not AEAD (same as ai.rs; no AEAD
   crate in Cargo.toml) — replace `encrypt/decrypt_api_key` and bump the
   `v1.` prefix when one is added. SSRF TOCTOU rebinding window remains
   (same as downloader.rs).
6. Sync runs in the HTTP handler (awaited, bounded per config) — a
   production deployment should move it to a background job via the
   scheduler kinds in §6.
7. `chrono-tz` is NOT in Cargo.toml: calendar rejects IANA timezone names
   with an honest 422; `timezone` accepts `"UTC"`/`"Z"` or a fixed offset
   (`"+07:00"`). All storage/comparison is UTC.
8. Calendar `resolve_tenant` still needs the batch-17 swap (§8); holidays
   Diesel persistence wiring is still open (in-memory authoritative).
9. Migrations never executed against a live PostgreSQL database.
10. **Academic calendar reconciliation (from batch 13's OPEN list):** the
    academic module has its own calendar concept (class schedules, exam
    periods); the integrator must reconcile it with the global calendar
    aggregation — academic should answer `calendar.items_requested` with
    its schedules rather than duplicating a calendar view.
11. Exams vs curriculum 3-domain assessment overlap (batch 13/14 OPEN) is
    unrelated to this batch but shares the same integrator pass.
12. `pub mod privacy;` is MISSING from `mod.rs` (batch 14 compile-blocker:
    ai.rs + import_export.rs both use `crate::modules::privacy`) — the
    integrator must add it; it blocks compilation of the whole tree, not
    just this batch.
