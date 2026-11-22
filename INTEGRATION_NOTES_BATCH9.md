# INTEGRATION NOTES — BATCH 9 (Whitelabel + Module Presets / Setup Wizard)

Coordinator-verified by read-back. Nothing pushed. Remote head still `7cf6853c`.

## 1. What batch 9 delivered

| Item | Files | Lines | Routes | Migration | Tables |
|---|---|---|---|---|---|
| Whitelabel | `src/modules/whitelabel.rs` (new) | 2,345 | 15 | `2026-11-22-000071_whitelabel` | 5 |
| Presets + Setup Wizard | `src/modules/presets.rs` (new) | 2,747 | 17 | `2026-11-22-000070_presets` | 4 |
| **Totals** | **2 new module files + 2 migrations** | **5,092** | **32** | — | **9** |

Tree at batch-9 verification time: 154 `.rs` files, ~311,715 Rust lines, 73 migration dirs.

Coordinator verification (read-back, not compilation): whitelabel.rs 2,345 lines /
presets.rs 2,747 lines (exact match to builder reports); RouteInfo lines 17 in
whitelabel.rs (= 15 `RouteInfo {` literals + import + fn signature) and 19 in
presets.rs (= 17 literals + import + fn signature); `todo!`/`unimplemented!` 0 in
both files; forbidden reference-project name 0 in both files + both migrations;
Indonesian words (word-boundary grep, broad list) 0 in both files; business-type
match arms in presets.rs 0; `require_permission` 15 hits in whitelabel.rs (14
non-public handlers + 1 definition); `require_capability` 18 hits in presets.rs
(1 definition + 17 handler calls); all 9 preset seed module ids confirmed present
in `src/modules/mod.rs` (`crm`, `billing`, `notifications`, `projects`, `tasks`,
`tickets`, `store`, `licensing`, `api_manager`); all 8 dropped ids confirmed
ABSENT from `mod.rs` (`properties`, `web3`, `timetracker`, `todos`, `portals`,
`workflows`, `network_tools`, `invoices` — no `invoices` module exists at all);
seed parity confirmed: 4 preset rows + 13 `preset_modules` rows in both the Rust
seed table and the migration `INSERT`s (same UUIDs
`10000000-0000-4000-8000-000000000001` … `...0004`); CREATE TABLE == DROP TABLE
in both migrations (whitelabel 5/5, presets 4/4 — the presets down.sql grep shows
5 only because a comment line contains the words "DROP TABLE"; the actual DROP
statements are 4).

## 2. MIGRATION NUMBERING — COLLISION, ACTION REQUIRED

- **`000070` EXISTS TWICE on disk**: `2026-11-22-000070_presets` (this batch) and
  `2026-11-22-000070_saas_domains` (batch 8). The presets builder's claim that
  "000070 was free" is WRONG — it checked before batch 8 landed its directory
  (or misread the listing). Do NOT run both under the same number: the
  integrator must renumber ONE of them (suggested: renumber `000070_presets` to
  the next free number at integration time — 000073+ — and update every in-file
  reference: the directory name, the header comments in up.sql/down.sql, and any
  `migration 000070` mentions in `src/modules/presets.rs`).
- Pre-existing collisions NOT from this batch (flagged before, still open):
  `000056` (`000056_community` batch 4 vs `000056_storage_product_byo`),
  `000068` (`000068_licensing_saas` vs `000068_network_tools`, both batch 7).
- `000100_calculator` (batch 3) is still out of sequence.
- Batch 9's other number is clean: `000071_whitelabel` is unique. Batch 8's
  `000072_domain_verification` is unique.

## 3. `src/modules/mod.rs` — add (integrator)

```rust
pub mod whitelabel;
pub mod presets;
```

In the register function, alongside the other `::register(registry);` calls:

```rust
whitelabel::register(registry);
presets::register(registry);
```

Module ids: `whitelabel` (requires: none declared beyond the core; see §6),
`presets` (`requires: ["extensions", "licensing", "audit"]` — all three are
registered in `mod.rs`, verified). `whitelabel` declares
`available_as_product() -> true`, `product_category() -> Some("infrastructure")`
in the exact `meetings.rs` / `storage_manager.rs` trait form. `presets` does not
declare itself sellable (it is admin infrastructure).

## 4. `src/db/schema.rs` — `table!` blocks (from the up.sql files)

```rust
table! {
    whitelabel_global_settings (id) {
        id -> Varchar,
        allow_whitelabel -> Bool,
        updated_by -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

table! {
    whitelabel_company_settings (company_id) {
        company_id -> Varchar,
        mode -> Varchar,
        updated_by -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

table! {
    whitelabel_global_defaults (id) {
        id -> Varchar,
        display_name -> Varchar,
        logo_storage_target -> Nullable<Varchar>,
        logo_storage_key -> Nullable<Varchar>,
        favicon_storage_target -> Nullable<Varchar>,
        favicon_storage_key -> Nullable<Varchar>,
        primary_color -> Varchar,
        accent_color -> Varchar,
        background_color -> Varchar,
        text_color -> Varchar,
        sender_name -> Varchar,
        sender_email -> Varchar,
        login_tagline -> Varchar,
        login_background_storage_target -> Nullable<Varchar>,
        login_background_storage_key -> Nullable<Varchar>,
        powered_by_text -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

table! {
    whitelabel_profiles (id) {
        id -> Uuid,
        company_id -> Varchar,
        tenant_id -> Nullable<Varchar>,
        display_name -> Nullable<Varchar>,
        logo_storage_target -> Nullable<Varchar>,
        logo_storage_key -> Nullable<Varchar>,
        favicon_storage_target -> Nullable<Varchar>,
        favicon_storage_key -> Nullable<Varchar>,
        primary_color -> Nullable<Varchar>,
        accent_color -> Nullable<Varchar>,
        background_color -> Nullable<Varchar>,
        text_color -> Nullable<Varchar>,
        custom_domain -> Nullable<Varchar>,
        domain_status -> Varchar,
        hide_powered_by -> Bool,
        sender_name -> Nullable<Varchar>,
        sender_email -> Nullable<Varchar>,
        login_logo_storage_target -> Nullable<Varchar>,
        login_logo_storage_key -> Nullable<Varchar>,
        login_background_storage_target -> Nullable<Varchar>,
        login_background_storage_key -> Nullable<Varchar>,
        login_tagline -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

table! {
    whitelabel_history (id) {
        id -> Uuid,
        company_id -> Varchar,
        tenant_id -> Nullable<Varchar>,
        actor -> Varchar,
        action -> Varchar,
        field -> Varchar,
        old_value -> Nullable<Text>,
        new_value -> Nullable<Text>,
        allowed -> Bool,
        created_at -> Timestamptz,
    }
}

table! {
    presets (id) {
        id -> Uuid,
        slug -> Text,
        name -> Text,
        description -> Text,
        business_type -> Text,
        built_in -> Bool,
        archived -> Bool,
        settings -> Jsonb,
        steps -> Jsonb,
        created_by -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

table! {
    preset_modules (id) {
        id -> Uuid,
        preset_id -> Uuid,
        module_id -> Text,
        required -> Bool,
        sort_order -> Int4,
        created_at -> Timestamptz,
    }
}

table! {
    preset_wizard_states (id) {
        id -> Uuid,
        company_id -> Uuid,
        actor_id -> Text,
        preset_id -> Nullable<Uuid>,
        steps -> Jsonb,
        current_step -> Text,
        status -> Text,
        skipped_steps -> Jsonb,
        disabled_optional_modules -> Jsonb,
        settings -> Jsonb,
        application_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

table! {
    preset_applications (id) {
        id -> Uuid,
        preset_id -> Nullable<Uuid>,
        preset_slug -> Text,
        company_id -> Uuid,
        actor_id -> Text,
        status -> Text,
        outcomes -> Jsonb,
        settings_applied -> Jsonb,
        created_at -> Timestamptz,
        completed_at -> Nullable<Timestamptz>,
    }
}
```

Add all nine table names to `allow_tables_to_appear_in_same_query!`. Follow the
existing `schema.rs` conventions for type spellings (Varchar vs Text, Uuid,
Timestamptz, Jsonb) — the SQL types above are the source of truth.

## 5. `src/db/models.rs` — structs (one Queryable + one New* Insertable per table)

- `WhitelabelGlobalSetting` / `NewWhitelabelGlobalSetting`
  (`id`, `allow_whitelabel`, `updated_by`, `created_at`, `updated_at`).
- `WhitelabelCompanySetting` / `NewWhitelabelCompanySetting`
  (`company_id`, `mode`, `updated_by`, `created_at`, `updated_at`).
- `WhitelabelGlobalDefaults` / `NewWhitelabelGlobalDefaults` (all columns of
  `whitelabel_global_defaults` above).
- `WhitelabelProfile` / `NewWhitelabelProfile` (all columns of
  `whitelabel_profiles` above; `id: Uuid`, `tenant_id: Option<String>`).
- `WhitelabelHistory` / `NewWhitelabelHistory` (all columns of
  `whitelabel_history` above).
- `Preset` / `NewPreset` (`id`, `slug`, `name`, `description`,
  `business_type`, `built_in`, `archived`, `settings: serde_json::Value`,
  `steps: serde_json::Value`, `created_by`, `created_at`, `updated_at`).
- `PresetModule` / `NewPresetModule` (`id`, `preset_id`, `module_id`,
  `required`, `sort_order`, `created_at`).
- `PresetWizardState` / `NewPresetWizardState` (`id`, `company_id`,
  `actor_id`, `preset_id: Option<Uuid>`, `steps`, `current_step`, `status`,
  `skipped_steps`, `disabled_optional_modules`, `settings`,
  `application_id: Option<Uuid>`, `created_at`, `updated_at`).
- `PresetApplication` / `NewPresetApplication` (`id`, `preset_id:
  Option<Uuid>`, `preset_slug`, `company_id`, `actor_id`, `status`,
  `outcomes`, `settings_applied`, `created_at`, `completed_at:
  Option<DateTime<Utc>>`).

## 6. Roles / capabilities (add to the roles module)

- Whitelabel: `whitelabel.manage` (admin), `whitelabel.view` (tenant read of
  own allowance/branding), `whitelabel.branding.manage` (tenant branding
  writes). Until these exist, every whitelabel capability check fails closed.
- Presets: `presets:view` (viewer/auditor view-only), `presets:manage` (admin
  CRUD/clone/archive), `presets:apply` (operator/developer + admin; apply +
  wizard).
- The whitelabel public route `GET /api/v1/whitelabel/public/branding/{company_id}`
  is intentionally exempt from capability checks and must be gated by an
  `ApiAccessRule` row for `/api/v1/whitelabel/public/*` (default deny) — same
  pattern as the portals public route.

## 7. Hook wiring the integrator must still do

### Whitelabel — events emitted (all in `whitelabel.*`)
`whitelabel.allowance.changed`, `whitelabel.defaults.changed`,
`whitelabel.branding.changed`, `whitelabel.branding.denied` (disallowed
attempt, 403), `whitelabel.branding.reset`, `whitelabel.domain.requested`,
`whitelabel.domain.verified`, `whitelabel.domain.rejected`,
`whitelabel.sender.requested`.

### Whitelabel — hooks / listeners
- `whitelabel.branding.resolve` (`{"company_id", "tenant_id"?}` → resolved
  branding record). CONSUMERS NOT YET WIRED: theme, portals, mail, and login
  rendering must call this hook per request/tenant. Until they do, branding
  resolution exists but nothing renders it.
- `whitelabel.allowance.check` (`{"company_id"}` → `{"allowed"}`).
- Whitelabel LISTENS to `domain.verify` (moves a profile's `domain_status`
  requested → verified/rejected). The domains module has NO listener yet for
  `whitelabel.domain.requested`; the mail module has NO listener yet for
  `whitelabel.sender.requested` — add them or those handoffs go nowhere.
- Add the `whitelabel.*` event names to `AUDITED_EVENTS` in `audit.rs`
  (no `audit.record_requested` event exists in the codebase, so the audit
  trail currently flows via the `whitelabel.*` events + the `whitelabel_history`
  table only).

### Presets — events emitted
`preset.created/updated/cloned/archived`, `preset.module.enabled`,
`preset.settings.applied`, `preset.applied`, `presets.wizard.started`,
`presets.wizard.step_saved`, `presets.wizard.skipped`,
`presets.wizard.resumed`, `presets.wizard.restarted`,
`presets.wizard.completed`, `presets.wizard.settings.completed`,
`module.enabled` (per module enabled by an apply — the audit module already
subscribes to `module.enabled`, so enablements are audited).

### Presets — listeners / gaps
- Add the `preset.*` / `presets.wizard.*` event names to `AUDITED_EVENTS`
  (audit does not subscribe to them yet).
- NO subscribers yet for `preset.settings.applied` /
  `presets.wizard.settings.completed` (settings / company / branding /
  domains modules) — the wizard stores the company-name / branding / domain
  answers and hands them off via these events; the owning modules must listen.
- No auto-start on tenant creation yet — a `company.created` hook
  subscription that starts a wizard is a follow-up.
- Enabled-state visibility gap: presets tracks what *it* enabled per tenant
  in its own map; modules enabled via the Extensions Manager are invisible to
  it, so re-applying a preset may report a module as `enabled` that the
  Extensions Manager already enabled. Follow-up fix: a shared `EnabledState`
  owned by extensions.

## 8. Seeded presets (data — verify against §1; do NOT hard-code these anywhere else)

| Preset (slug) | Modules (required / optional) |
|---|---|
| Property Seller (`property-seller`) | crm (req), billing (req), notifications (opt) |
| Freelancer (`freelancer`) | projects (req), billing (req), tasks (opt) |
| Agency (`agency`) | projects (req), crm (req), tickets (opt) |
| SaaS Seller (`saas-seller`) | store (req), licensing (req), billing (req), api_manager (opt) |

Deliberately NOT seeded (verified against `mod.rs`): `properties`, `web3`
(Property Seller); `timetracker`, `todos` (Freelancer); `portals`,
`workflows` (Agency); `network_tools` (SaaS Seller — the registered `network`
module is a different module and was NOT substituted); `invoices` does not
exist (billing is the real billing/invoice module). Once those modules are
registered, an admin can add them to any preset via the API with no code
change. Seed UUIDs: `10000000-0000-4000-8000-000000000001` … `...0004`
(identical in Rust seeds and SQL INSERTs).

## 9. README.md / TODO.md lines (integrator)

- README: module count +2 (whitelabel, presets), migrations +2, tables +9;
  add list entries "Whitelabel" (server-side branding resolution, allow white
  labelling global + per-company, sellable infrastructure add-on) and "Module
  Presets + Setup Wizard" (data-driven presets, resumable wizard, entitlement-
  aware apply).
- TODO: add batch-9 section with the OPEN list from §11 and the §2 renumbering
  job. Note: README counts are already stale from parallel rounds — recount
  the whole tree at integration time.

## 10. Modularity-rule compliance (standing user rule, MUTLAK)

Checked by the coordinator against both builders' work:
- Presets are DATA: definitions live in the `presets` / `preset_modules`
  tables (seeded by identical Rust + SQL rows), admin-editable, cloneable;
  built-ins can only be edited via clone (PATCH returns 409 with the clone
  hint). Runtime logic reads the store only. Verified: 0 business-type match
  arms in `presets.rs`; wizard steps come from stored preset/wizard data.
- Branding defaults are CONFIGURABLE: they live in the admin-editable
  `whitelabel_global_defaults` record (`GET/PUT /admin/defaults`); the stored
  record is the source of truth once saved. No hard-coded colour / tenant /
  brand-field lists; the only closed sets are state machines (allowance mode,
  domain status).
- Disclosed hard-coded spots (per the rule — record, don't hide):
  1. `GlobalDefaults::seed()` values (e.g. `#2563EB`, `"Unified"`,
     `"Powered by Unified"`) — initial contents of the defaults record on a
     fresh install only; never consulted after an admin saves defaults.
  2. `builtin_preset_seeds` in `presets.rs` + the matching SQL `INSERT`s —
     the only preset content baked into code; technically required because
     the in-memory store is authoritative and has no DB read path yet.
  3. `WIZARD_STEP_NAMES` closed step vocabulary (same closed-set pattern as
     `ProductCategory`); `DEFAULT_WIZARD_STEPS` is marked as the configurable
     default, used only when a preset stores no step list.

## 11. OPEN list

1. **`cargo check` / `cargo build` NEVER ran anywhere** (no toolchain in this
   VM, crates.io blocked). Compilation is unverified for both modules.
   Riskiest unverified surfaces: the `RouteInfo` / `route_version_support`
   shapes as used, the hook payload shapes, and the `check_module_entitlement`
   call in presets.rs.
2. **Migrations never executed** against live PostgreSQL; in-memory DashMap
   stores stay authoritative everywhere (same pattern as other rounds).
3. **§2 renumbering**: `000070` exists twice (presets vs saas_domains) —
   renumber one before running migrations. Pre-existing: `000056` twice,
   `000068` twice, `000100_calculator` out of sequence.
4. **Registration**: `pub mod whitelabel;` / `pub mod presets;` + register
   calls missing (integrator, §3); schema/models wiring missing (§4–§5).
5. **Roles capabilities missing** (§6) — all capability checks fail closed
   until the roles module defines them.
6. **Consumers not wired** (§7): theme/portals/mail/login don't call
   `whitelabel.branding.resolve` yet; domains/mail don't listen to the
   whitelabel handoff events; nobody listens to the preset settings handoff
   events; audit doesn't subscribe to `preset.*` / `presets.wizard.*` yet.
7. **Public whitelabel route needs an `ApiAccessRule` row** (§6) or the API
   Manager denies it.
8. **Known gaps, same as other rounds**: `viewer_id`/`viewer_role` arrive as
   trusted query/body parameters until the auth extractor lands (every handler
   still enforces its capability check); whitelabel renderers must only use
   `custom_domain` when `domain_status == "verified"` (the resolver reports
   the status but does not hide a `requested` domain string); a whitelabel
   storage-pair update must resend both target and key in the same request;
   presets' enabled-state visibility gap (§7).
9. Nothing pushed. No tracker, MEMORY.md, sync-record, or LICENSE edits were
   made by this round.
