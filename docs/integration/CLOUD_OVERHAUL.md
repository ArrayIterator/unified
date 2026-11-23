# Cloud Overhaul (task 377)

Status: IMPLEMENTED (see §13 for the file list). Design first, then code:
this document is the contract the implementation follows.

## 0. User directives (in order)

1. "module hosting kita rombak total!" — total overhaul of the hosting
   module.
2. "modules providers itu salah! Modules provider yang isinya openstack
   redshift itu masuk provider cloud" — the providers module is wrong:
   the cloud providers (OpenStack, Redshift, Docker, Kubernetes, EC2,
   Proxmox, KVM, Xen, …) belong under **cloud providers**.
3. Cloud module gets a PACKAGE feature — a sellable PRODUCT (site-builder
   pattern: `is_core=false`, `available_as_product=true`; tenant without
   the package gets 403/404). The package contains PLANS, and with a plan
   the tenant can create APPS (deploy applications).
4. Connectors are NOT baked into core cloud — they go into EXTERNAL
   MODULES that hook into the cloud module later: virtualization/panel
   connectors (SolusVM NEW, Virtualizor, cPanel, DirectAdmin, Plesk,
   CyberPanel, WHMCS) and SSL providers (certificates module: Let's
   Encrypt, Google, ZeroSSL, Cloudflare, custom ACME). The cloud module
   DEFINES the hook contracts they plug into.
5. "cloud panel" is the grouping name for those external panel/provider
   connector modules — each self-contained ("self dependensi"), hooking
   into cloud via the hook contracts in §8.
6. QEMU manager lives in the virtualization submodule via Proxmox
   (existing KvmAdapter). A direct libvirt/QMP driver is a FUTURE
   external-module candidate, not core.
7. ARCHITECTURE RULE (user, 2026-11-23 ~23:02): Unified (the ERP) has
   NO VPS manager. The VPS manager is a SEPARATE EXTERNAL PROJECT
   (task 378 was cancelled for this reason). Unified has a CLOUD
   CONNECTOR: it CONNECTS to external platforms — it does NOT
   build/reimplement the VPS/VM panel itself. The cloud overhaul stays
   a connector layer: provider adapters + package/plans/apps deployed
   THROUGH those connectors. No built-in VPS/VM panel, VM provisioning
   engine, or hypervisor management in the cloud module.
8. ARCHITECTURE PRINCIPLE (user, 2026-11-23 ~23:04): the provider
   module is a TRANSLATION layer — each adapter translates an external
   provider's API (Hetzner, OpenStack, Docker, …) into the ERP's
   uniform interface (the `CloudProvider` trait: `kind`,
   `test_connection`, `list_servers`, `server_status` + optional
   `create`/`scale`/`delete`/`start`/`stop`). The cloud module stays a
   connector layer on this principle; it never reimplements the
   platforms.

## 1. Current state (verified from source, 2026-11-23)

- `hosting` module (id `hosting`, "Hosting Manager", ~2,900 lines,
  `src/modules/hosting/`): HostingPlan (disk/CPU/RAM/bandwidth, price,
  billing cycle monthly/yearly, optional store product link) and
  HostingAccount (plan, domain, company, status
  pending/active/suspended/cancelled, ssl_state, disk/bandwidth used,
  store order link). Routes `/api/v1/hosting/plans|accounts` + lifecycle
  (`requirements`, `activate`, `suspend`, `unsuspend`, `cancel`).
  Requires `store`, `domains`, `network`. Hooks: `store.product.*`,
  `store.order.completed` / `order.paid`, `domain.verify`,
  `ssl.renewed` / `ssl.swapped` / `ssl.swap`. In-memory DashMap stores
  are authoritative; Diesel tables exist in migration 000007
  (`hosting_plans`, `hosting_accounts`, `hosting_order_claims`) but are
  not mapped in `schema.rs`. Is core (default `is_core=true`); routes
  are NOT package-gated today.
- `providers` module (id `providers`, "Cloud Providers",
  `src/modules/providers/`): one hookable adapter registry
  (`ProviderRegistry::register_factory`, enum-keyed by `ProviderKind`)
  plus the `CloudProvider` trait (required: `test_connection`,
  `list_servers`, `server_status`; default-unsupported: `create_server`,
  `scale_server`, `delete_server`, `start_server`, `stop_server`).
  Connections + inventory cache are in-memory DashMaps. Capabilities
  come from the registry seed JSON
  (`crates/unified-core/provider_registry_seed.json`, family `"cloud"`,
  single source of truth at runtime) and are mirrored in DB rows
  (`provider_registry_definitions`, migration 000127, `ON CONFLICT DO
  NOTHING`). Routes `/api/v1/providers` (kinds, connections CRUD + test,
  servers list/create/status/delete/scale/start/stop, inventory, sync).
  Is core; routes are NOT package-gated today.
- Adapter inventory (capabilities from the seed):
  Hetzner (create/scale/delete), OVH (create/scale/delete), Tencent
  (read-only), Huawei (read-only), AWS EC2 (read-only), Google Compute
  (read-only), Virtualizor (create/delete/start/stop),
  **Docker (delete/start/stop; NO create)**, **Kubernetes (read-only)**,
  Redshift (read-only), DigitalOcean (create/scale/delete),
  Linode (create/scale/delete), OpenStack (create/delete/start/stop),
  Proxmox VE (delete/start/stop; no create), KVM via Proxmox
  (delete/start/stop), XenServer/XCP-ng (delete/start/stop), Custom
  (config-driven).
- `clusters` module ("Multi-Cluster") is node registration + mutual
  sync — NOT cloud infrastructure; untouched by this overhaul.
- `panels` module (WHMCS, cPanel/WHM, DirectAdmin, Plesk, CyberPanel,
  generic; own `panel_registry`) and `certificates` module (ACME modes
  `letsencrypt`, `google`, `zerossl`, `cloudflare`, `custom`) stay
  as-is in this overhaul.
- No module declares `requires()` on `hosting` or `providers`. No React
  page and no demo page calls `/api/v1/hosting` or `/api/v1/providers`.
- `ProductCategory::ALL` = productivity, communication, commerce,
  infrastructure, ai. Cloud uses `infrastructure`.
- SaaS package seeds: exam `20000000-0000-4000-8000-000000000001`,
  site-builder `...0002`. Cloud seed uses `...0003`.
- Flat Color Icons has NO cloud icon (329 SVGs checked) — the cloud
  module icon is custom-drawn in the same flat style (see §11).

## 2. New structure

One new module id, `cloud`, with Rust submodules. The old ids stay
registered as thin, deprecated compatibility shims (see §10).

```
src/modules/cloud.rs            # root: pub mod hosting/providers/...
src/modules/cloud/
  module.rs                     # CloudModule (id "cloud")
  hosting/                      # MOVED from src/modules/hosting/* (verbatim, + gate)
    types.rs requests.rs views.rs helpers.rs
    handlers_plans.rs handlers_accounts.rs hooks.rs ai_capabilities.rs
  providers/                    # MOVED from src/modules/providers/* (verbatim, + caps)
    types.rs validation.rs store.rs http.rs
    adapters.rs adapters_cloud.rs adapters_custom.rs
    adapters_vps.rs adapters_infra.rs handlers.rs
  containers.rs                 # NEW: CloudApp types/state + app deploy handlers
  virtualization.rs             # NEW: virtualization grouping endpoint (view only)
  handlers.rs                   # NEW: cloud-level handlers (panels, ssl-providers, virtualization)
  hooks.rs                      # NEW: hook contracts + collectors (§8)
  events.rs                     # NEW: cloud.app.* event names
  helpers.rs                    # NEW: require_cloud_entitlement + PERM_CLOUD_*
  ai_capabilities.rs            # NEW: AI capabilities for "cloud"
src/modules/hosting.rs          # THIN SHIM: re-export + compat HostingModule
src/modules/providers.rs        # THIN SHIM: re-export + compat ProvidersModule
src/modules/mod.rs              # pub mod cloud; (before cloudflare); cloud::register
                                # in the infrastructure group, before hosting
```

Module declaration (`CloudModule`):

- id `cloud`, name "Cloud", description: cloud hosting, provider
  connectors, containers and app deployment through those connectors.
- `is_core() = false`, `available_as_product() = true`,
  `product_category() = "infrastructure"` (site-builder pattern).
- `requires() = ["store", "domains", "network"]` (inherited from
  hosting: plans are sold via Store, accounts/apps are bound to
  Domains, DNS/TLS probing via Network).
- `requires_libraries()` — same set as hosting plus `reqwest`,
  `async-trait` (adapters).

### Submodules

| Submodule | Contents |
|---|---|
| `cloud::hosting` | HostingPlan / HostingAccount, plan + account handlers, lifecycle, requirements check, store/domain/SSL hooks. Unchanged logic; see §3. |
| `cloud::providers` | Provider connections, adapter registry, all 17 provider adapters, inventory cache, sync. Unchanged logic except the capability flips in §5. |
| `cloud::containers` | NEW: `CloudApp` records + app deploy/list/get/delete/start/stop (§6). |
| `cloud::virtualization` | NEW: one grouping endpoint (§7). Connector view only — NOT a VM panel. |
| `cloud::hooks` | Hook contracts + collectors (§8). |
| `cloud::packages` | No separate submodule: the package is the module itself (seed package `cloud-saas`, §9) plus the entitlement gate in `helpers.rs`. Plans are the existing `HostingPlan` (§9). |

## 3. Hosting overhaul (what actually changes)

- **Move only.** All hosting files move verbatim under
  `cloud::hosting`. No handler logic, type, hook, or event name changes.
  Hook events stay `hosting.plan.*`, `hosting.account.*`,
  `hosting.requirements.checked` (existing subscribers keep working).
- **Shared state.** `HostingState` becomes a process-wide singleton
  (`OnceLock<Arc<HostingState>>` accessor in `cloud::hosting::types`),
  used by BOTH the compat `HostingModule` and the `CloudModule`, so a
  plan created under `/api/v1/cloud/hosting/plans` is visible under
  `/api/v1/hosting/plans` and vice versa. (Providers connections /
  inventory are already `OnceLock` statics — no change needed there.)
- **Hooks registered once.** The compat `hosting` and `providers`
  modules keep their existing `register_hooks` (each subscription is
  registered exactly once, since each module is registered once); the
  `cloud` module adds NO duplicate subscriptions for them. The hook
  closures capture the shared `HostingState`, so observations land in
  the same store both scopes read. `init` (AI capabilities) still
  runs on both: `hosting` keeps its existing AI capabilities, `cloud`
  registers its own (§11).
- **INTENDED BEHAVIOR CHANGE — package gate.** Hosting moves from
  core/ungated to package-gated: every hosting handler (plans AND
  accounts, both scopes) calls `require_cloud_entitlement` after the
  existing permission check (site-builder `authorize` pattern:
  permission first, then entitlement). A tenant without the cloud
  package gets 403 on the routes; the control plane (no company scope)
  is never gated. This is the point of the sellable package (§9) —
  the billing/product LOGIC is intact, its AVAILABILITY becomes
  package-gated.
- **Plans are the plan tiers.** No new plan type is introduced: the
  cloud package's PLANS are the existing `HostingPlan` resource bundles
  (disk/CPU/RAM/bandwidth) — they already are sellable tiers linked to
  store products, and their quotas bound app deployments (§6).

## 4. Providers become cloud providers

- **Move only.** All providers files move verbatim under
  `cloud::providers`. No adapter logic, validation, auth, or endpoint
  changes. `ProviderKind`, `CloudProvider`, `ProviderRegistry`,
  `ProviderConnection`, the seed JSON shape, and the `provider.*` hook
  events are unchanged.
- The providers compat module keeps id `providers` and registers ONLY
  the legacy `/api/v1/providers` scope (no hooks, no init changes).
- The cloud module registers the canonical
  `/api/v1/cloud/providers/*` scope using the same handlers.
- The providers handlers gain the same cloud entitlement gate as the
  hosting handlers (permission first, then entitlement), so the legacy
  scope cannot bypass the package.
- Virtualizor stays in the providers code as-is (works, untouched).
  It is a LATER extraction candidate to a "cloud panel" external
  module (§8), NOT moved in this overhaul.

## 5. Capability flips (Docker create, Kubernetes write)

Seed JSON (`crates/unified-core/provider_registry_seed.json`, the
runtime source of truth) plus migration 000135 `UPDATE`s on the stored
rows (`provider_registry_definitions`, `ON CONFLICT DO NOTHING` means
existing installs keep old rows without the update):

- `docker`: `create: false → true` (delete/start/stop/containers
  unchanged). `scale` stays false (containers are not scaled; create
  more instead).
- `kubernetes`: `create/scale/delete: false → true`
  (`containers` stays true, `start_stop` stays false — pods are not
  started/stopped; they are deleted (restart) or scaled to 0).

No other kind changes capabilities.

## 6. Containers + app deploy (NEW)

### 6.1 Request shapes (backward-compatible extensions)

`CreateServerRequest` gains optional fields (all `#[serde(default)]`,
existing callers unaffected): `namespace`, `replicas`, `env`
(`HashMap<String, String>`), `ports` (`[{container_port, host_port?,
protocol?}]`), `command` (`[String]`), `memory_mb`, `cpu_cores`.
`ScaleServerRequest` gains `replicas: Option<u32>`.

### 6.2 Docker `create_server`

`POST /v1.44/containers/create?name={name}` with
`{Image, Cmd, Env: ["K=V"], ExposedPorts, HostConfig: {PortBindings,
Memory, NanoCpus}}`. Resource limits come from the request (which the
app flow fills from the plan: `cpu_cores → NanoCpus`,
`ram_mb → Memory` bytes). 201 returns `{Id}`; the adapter then calls
`server_status(id)`. Image pull is the engine's job (`Image` must exist
on the host or be pullable by name; no image management in this round —
same line as the existing "no image management" scope note).

### 6.3 Kubernetes write ops

- `create_server` → `POST /apis/apps/v1/namespaces/{ns}/deployments`
  (Deployment manifest: image, env, ports, replicas; namespace from
  the request, default `default`). Returns the created deployment
  mapped via `map_deployment` (re-fetched for status fields).
- `scale_server` → `PATCH .../deployments/{name}/scale`
  (`{"spec": {"replicas": n}}`, merge-patch).
- `delete_server` → `DELETE` deployment or pod by id
  (`deployment/<ns>/<name>` or `pod/<ns>/<name>`). Deleting a pod IS
  the restart mechanism (its controller recreates it).
- New `KubernetesAdapter::request_json(method, path, body)` helper
  (POST/PATCH/DELETE with JSON body, empty-body tolerant like Docker's
  start/stop). The existing `get()` stays for reads.

### 6.4 Apps — deploy applications through the connectors

`CloudApp` record (in-memory DashMap store, same pattern as hosting /
providers; table `cloud_apps` in migration 000135 for future
persistence — see §12; `plan_id` / `connection_id` are plain UUIDs
with NO foreign keys, because plans and connections are
in-memory-authoritative and an FK would reject valid references):

```
id, name, plan_id (HostingPlan), connection_id, kind (docker|kubernetes),
resource_id (container id | deployment/<ns>/<name>), image,
status (creating|running|stopped|failed), url (best-effort, from a
mapped host port), env (json), created_at, updated_at
```

Flow (`POST /api/v1/cloud/apps`, `cloud.manage` + package gate +
tenant/company scope):
plan exists and is active → connection exists, enabled, kind is
docker/kubernetes → build `CreateServerRequest` from the app + plan
quotas → `adapter.create_server` → Docker: `start_server` after
create (K8s deployments start pods by themselves) → store the record
→ emit `cloud.app.deployed` → 201.

Other endpoints: `GET /api/v1/cloud/apps` (paginated),
`GET /api/v1/cloud/apps/{id}` (live status refresh via
`adapter.server_status`, best effort — stored status on probe
failure), `DELETE /api/v1/cloud/apps/{id}` (adapter delete + remove
record + `cloud.app.deleted`), `POST .../start`, `POST .../stop`
(Docker only; Kubernetes returns the honest unsupported error).

## 7. Virtualization grouping (view only — NOT a VM panel)

- `GET /api/v1/cloud/virtualization`: connections whose kind is one
  of `proxmox`, `kvm`, `xen`, `virtualizor`, with cached inventory
  counts. QEMU is covered here via the Proxmox `KvmAdapter`
  (QEMU guests as `node:qemu:vmid`).
- This is a grouping/view over the CONNECTOR adapters. It does NOT
  provision VMs, does NOT manage templates/images, has NO console/VNC,
  and does NOT talk to hypervisors directly. Per the architecture rule
  (§0.7), that is the external VPS manager project's job, not
  Unified's. A direct libvirt/QMP driver (QMP is JSON over a Unix
  socket; libvirt remote is SSH/TLS — neither is HTTP) is a FUTURE
  external-module candidate, not built here.

## 8. Hook contracts ("cloud panel" external modules)

Following the `widgets.widget_types` / `email_provider.definitions`
collect pattern: emit with `{}`, each answer is one definition object
or an array; malformed answers are skipped; built-ins win on id
collision; the catalogue is deterministic.

| Hook event | Answers (definition shape) | Collector |
|---|---|---|
| `cloud.provider.definitions` | `{family: "cloud", id (kebab), display_name, capabilities: {create, scale, delete, containers, start_stop}, allowed_hosts?, needs_endpoint?}` | `collect_cloud_provider_definitions(ctx)`: seed catalog built-ins + hook answers. |
| `cloud.panel.definitions` | `{id (kebab), display_name, kinds?: [...]}` — panel connectors (cPanel, DirectAdmin, Plesk, CyberPanel, WHMCS, SolusVM, Virtualizor) | `collect_cloud_panel_definitions(ctx)`: hook answers (+ the in-tree `panels` module answers with its registry contents — see below). |
| `cloud.ssl_provider.definitions` | `{id (kebab, e.g. "letsencrypt"), display_name, acme_directory_url?}` | `collect_cloud_ssl_provider_definitions(ctx)`: hook answers (+ the in-tree `certificates` module answers with its ACME modes — see below). |

Exposed read endpoints: `GET /api/v1/cloud/panels` and
`GET /api/v1/cloud/ssl-providers` (collected definitions);
`GET /api/v1/cloud/providers` (the providers `list_kinds` handler)
merges hook-contributed provider definitions into the catalog the
same way `email_provider.definitions` does.

"Cloud panel" is the CATEGORY name for the external connector modules:
SolusVM, Virtualizor, cPanel, DirectAdmin, Plesk, CyberPanel, WHMCS,
and the SSL providers. Each is self-contained ("self dependensi":
its own dependencies, its own module/crate) and plugs into cloud
through these hooks. It is a category, not a code module.

What is NOT built here:
- SolusVM is NOT built into core. Only the hook contract it would
  use is defined (`cloud.panel.definitions` + the factory API below).
- Virtualizor, `panels`, `certificates` are NOT moved or extracted.
  They keep working as-is. Extraction to external modules is LATER
  work; to make the contracts live now, the in-tree `panels` module
  answers `cloud.panel.definitions` from its `panel_registry`
  (registered kinds + display names) and the in-tree `certificates`
  module answers `cloud.ssl_provider.definitions` from the ACME
  provider list in `crate::http::acme` (letsencrypt, google, zerossl,
  buypass, custom — with production directory URLs — plus the
  Cloudflare origin mode, which is not an ACME provider and carries
  no directory URL). Both are read-only hook answers, no behavior
  change. DONE (both registered in the modules' `register_hooks`).
  Note: the answer comes from the ACME provider list, not from the
  certificate-mode registry — the mode registry has only three modes
  (letsencrypt/custom/cloudflare) and no directory URLs, so it is
  not the SSL-provider catalogue.
- KNOWN GAP (documented, not fixed here): a contributed provider
  definition still needs a registered adapter factory
  (`ProviderRegistry::register_factory`, today keyed by the
  `ProviderKind` enum), exactly like the email-providers caveat. A
  string-keyed factory registry so external modules can add kinds
  without touching the enum is a future step.

## 9. Package → plans → apps (site-builder pattern)

- `CloudModule`: `is_core() = false`,
  `available_as_product() = true`,
  `product_category() = "infrastructure"`.
- Gate: `cloud::helpers::require_cloud_entitlement` — copied from
  `site_builder::helpers::require_site_builder_entitlement` with
  module id `"cloud"`: control plane (no company scope) never gated;
  tenant/company mismatch → 403; tenant without the package →
  `check_module_entitlement` verdict (403 on routes, 404 on the public
  render path where applicable). Applied in every cloud/hosting/
  providers handler after the permission check (site-builder
  `authorize` order: permission first, then entitlement). New cloud
  permissions: `cloud.view`, `cloud.manage` (free strings, same as
  `hosting.view` / `providers.manage`).
- Seed package (migration 000135 + `saas_packages/seed.rs` mirror,
  same pattern as site-builder):
  - id `20000000-0000-4000-8000-000000000003`, slug `cloud-saas`,
    name "Cloud SaaS", status published, `product_id` NULL (no
    pricing invented; pricing lives on the store product if attached).
  - modules: `cloud` (required, 0), `domains` (required, 1) — apps
    get domains/SSL through the domains module.
  - `saas_packages::init` picks the seed up automatically (add
    `seed_cloud_package()` to the seed loop); an existing install
    picks it up without disturbing stored packages.

## 10. Backward compatibility (hard constraint)

- Old routes keep working: `/api/v1/hosting/*` (compat `HostingModule`)
  and `/api/v1/providers/*` (compat `ProvidersModule`) register the
  SAME handler functions as the new canonical scopes. The only behavior
  change is the package gate (§3, intended).
- Old module ids stay registered: `hosting`, `providers` (marked
  DEPRECATED in code comments — canonical home is `cloud`). This keeps
  entitlements, AI capabilities registered under `hosting`, hook
  subscribers (`hosting.*`, `provider.*` events), and the admin module
  list working.
- Old hook event names are unchanged (`hosting.*`, `provider.*`).
  New events are additive: `cloud.app.deployed`,
  `cloud.app.deleted`, `cloud.app.failed`.
- `CreateServerRequest` / `ScaleServerRequest` extensions are all
  optional (`#[serde(default)]`) — old request bodies still parse.
- No DB table is dropped or renamed. `hosting_plans`,
  `hosting_accounts`, `hosting_order_claims`,
  `provider_registry_definitions` keep their names and rows.
- No feature is deleted: every old handler, adapter, capability, and
  validation stays.
- Hook registration (correction to the first draft of this doc, which
  implied cloud would re-register hosting/providers hooks): the
  compat shims keep their existing `register_hooks` and cloud adds
  NONE for hosting/providers — each subscription is registered
  exactly once, by exactly one module. The new `cloud.*.definitions`
  hooks are answered by the `panels` and `certificates` modules, not
  by cloud itself.

## 11. Icons, docs, AI

- New module icon: `assets/icons/modules/cloud.svg`, custom-drawn in
  the Flat Color Icons flat style (the set has no cloud icon —
  verified: 0 of 329 SVGs match "cloud"). DONE: sprite rebuilt
  (`assets/icons/modules-sprite.svg`, 157 symbols), `demo/assets/sprite.js`
  regenerated (byte-identical after JSON-unescape), `assets/icons/ICONS.md`
  regenerated (157 modules, cloud row present) and the custom draw
  noted in `assets/icons/README.md` (14 custom-drawn now).
  NOTE: ICONS.md as it stands renders at 32px, not the 64px this
  doc first specified — the 372 regeneration rebuilt it at 32px; the
  size discrepancy is pre-existing, not introduced here.
  `hosting` and `providers` icons stay.
- AI capabilities: `cloud::ai_capabilities.rs` registers
  `AI_CAPABILITIES` for module id `cloud` (plan/app drafting +
  deployment troubleshooting, all non-PII, same shape as hosting's).
  `hosting` keeps its existing capabilities.
- Docs: this file; `MODULES.md` gains a Cloud entry (count drift is
  separate open audit work — the entry is added, the catalogue total
  is not re-audited here).

## 12. Migration plan (000135)

New dir `crates/unified-core/migrations/2026-11-23-000135_cloud_package_apps/`
(picked up automatically by the embedded `diesel_migrations`):

- `up.sql`:
  - `CREATE TABLE IF NOT EXISTS cloud_apps (...)` (id UUID PK,
    name, plan_id UUID (no FK — plans are in-memory-authoritative),
    connection_id UUID (no FK — connections are in-memory),
    kind, resource_id, image, status, url, env JSONB, created_at,
    updated_at).
  - Seed `saas_packages` row `cloud-saas` (`...0003`) +
    `saas_package_modules` rows (`cloud`, `domains`), both
    `ON CONFLICT DO NOTHING`.
  - `UPDATE provider_registry_definitions SET capabilities = ...`
    for ALL 17 cloud kinds, with the full 9-key capability JSON
    copied from the updated seed JSON — not just docker/kubernetes:
    the WHMCS-alignment flags (`suspend`, `renew`,
    `change_password`, `usage`) are set per kind too (the 000127
    INSERT block is generated — never hand-edit it; the 000135 UPDATE
    is the correct place for a capability change on stored rows).
    The UPDATEs were generated from the seed JSON (old values for
    down.sql taken from the pre-overhaul seed).
- `down.sql`: drop `cloud_apps`; delete the seed rows; restore the
  old capability JSONs for all 17 cloud kinds.
- Rust seed mirror: `SEED_CLOUD_PACKAGE_ID` + `seed_cloud_package()`
  in `src/modules/saas_packages/seed.rs`, added to the init loop in
  `src/modules/saas_packages/module.rs`. DONE (both written).

## 16. Provider abstraction — WHMCS alignment

Reference standard (user, 2026-11-23 ~23:05; verified from
`developers.whmcs.com/provisioning-modules/supported-functions` and
`core-module-functions`): the WHMCS provisioning/server module.
Our `CloudProvider` trait was INCOMPLETE against it. The trait is
extended toward the standard in this overhaul; the translation-layer
principle (§0.8) is unchanged — each adapter still translates its
platform's API into the uniform trait, the trait just declares more
of the uniform surface.

### 16.1 Mapping table

| WHMCS function | Our trait (before) | Our trait (this overhaul) |
|---|---|---|
| CreateAccount | `create_server` (optional) | unchanged |
| SuspendAccount | — (no trait method) | **`suspend_server`** (optional, default unsupported) |
| UnsuspendAccount | — | **`unsuspend_server`** (optional, default unsupported) |
| TerminateAccount | `delete_server` (optional) | unchanged (Terminate ≡ delete) |
| Renew | — | **`renew_server`** (optional, default unsupported; NO adapter implements it — see §16.3) |
| ChangePassword | — | **`change_password`** (optional, default unsupported) |
| ChangePackage | `scale_server` (optional) | unchanged (ChangePackage ≡ scale) |
| MetaData (DisplayName, APIVersion, RequiresServer) | — | **`metadata()`** (default: display name from the kind, `api_version: "unknown"`, `requires_server: true`); adapters override with their real API version |
| ConfigOptions (up to 24 product settings) | — | ALREADY EXISTS as data: the seed catalog's `credentials_shape` per kind (name/label/type/required/secret/description). No trait duplication; the trait's `metadata()` covers the MetaData half |
| UsageUpdate (daily disk+bandwidth import, feeds overage billing) | — | **`usage_metrics`** (optional, default unsupported) returning `UsageMetrics { cpu_percent?, memory_mb?, disk_gb?, bandwidth_in_mb?, bandwidth_out_mb?, sampled_at }`; the daily import/cron is NOT built (see §16.3) |
| MetricProvider (metered billing) | — | same `usage_metrics` — the metric source; billing consumption is a later module |
| ClientArea / AdminLink / LoginLink / ClientAreaCustomButtonArray / ClientAreaAllowedFunctions / AdminCustomButtonArray / AdminServicesTabFields(+Save) / CustomActions | — | NOT BUILT (see §16.3) |

### 16.2 What gets implemented (per adapter)

Capability flags added to `ProviderCapabilities` + seed JSON +
migration 000135 stored-row UPDATEs (missing keys read as `false`,
so old rows stay valid): `suspend`, `renew`, `change_password`,
`usage`. New routes (both scopes, same handlers, capability-guarded
like the existing ones): `POST .../servers/{id}/suspend`,
`.../unsuspend`, `.../renew`, `.../change-password`,
`GET .../servers/{id}/usage`. New events: `provider.server.suspended`,
`.unsuspended`, `.renewed`, `.password_changed`.

| Adapter | suspend / unsuspend | change_password | usage_metrics |
|---|---|---|---|
| Docker | pause / unpause (`POST /containers/{id}/pause`, `/unpause`) | — | one-shot `GET /containers/{id}/stats?stream=false` (CPU %, memory, network rx/tx) |
| Kubernetes | scale replicas to 0 / back to 1 (documented: unsuspend restores 1 replica; the pre-suspend count is not preserved in this round) | — | — (needs metrics-server; future) |
| OpenStack | suspend / resume server actions (`POST /servers/{id}/action {"suspend": null}` / `{"resume": null}`) | `{"changePassword": {"adminPass": ...}}` action | — (Ceilometer/Gnocchi is deployment-dependent; future) |
| Proxmox VE (incl. KVM/QEMU guests) | `qm suspend` / `qm resume` (`POST /nodes/{node}/qemu/{vmid}/status/suspend`, `/resume`) | — (needs the guest agent + a set-password flow; future) | — |
| XenServer / XCP-ng | `VM.suspend` / `VM.resume` (XML-RPC) | — | — |
| Virtualizor | suspend / unsuspend (`act=suspend` / `act=unsuspend`) | `act=changepassword` | — |
| Hetzner | — (no suspend API; stop/start are the power actions) | — (`reset_password` generates a random password; it cannot set a chosen one) | `GET /v1/servers/{id}/metrics?type=cpu,disk,network&start=&end=` (last hour; CPU % + integrated network in/out) |
| Linode | — | `POST /linode/instances/{id}/password` (`{"root_pass": ...}`) | — |
| DigitalOcean | — | — (`password_reset` generates a random password, emailed to the account; it cannot set a chosen one) | — |
| OVH, Tencent, Huawei, AWS EC2, Google Compute, Redshift, Custom | — (no suspend/password/usage API in the adapter's scope) | — | — |

`renew_server` stays default-unsupported on EVERY adapter: renewal
is a billing-layer concept (the platform bills automatically); no
provider in the registry exposes a "renew this server" API. The
method exists so the trait matches the WHMCS surface and a future
adapter (or the billing module) can implement it.

### 16.3 What stays out and why

- **Client/admin UI hooks** (ClientArea, AdminLink, LoginLink,
  buttons, AdminServicesTabFields, CustomActions): no client-area UI
  exists for providers in the React app yet. These are UI-surface
  hooks, not API — they belong to a future React admin/client round,
  consuming `metadata()` + the catalog. The API surface built here
  is what those hooks would call.
- **Automation** (create on payment, suspend on overdue via daily
  cron, unsuspend on payment, terminate on long overdue): NOT wired.
  It needs a scheduler/cron (the subscription-expire sweep is itself
  open work) plus billing state (overdue = billing, not cloud).
  Mapping for the future round: `store.order.completed` /
  `order.paid` → `create_server` (the hosting claim pattern already
  exists); a daily overdue sweep → `suspend_server`; payment →
  `unsuspend_server`; long-overdue sweep → `delete_server`.
- **UsageUpdate daily import / overage billing**: `usage_metrics`
  is the metric SOURCE; the daily import job and any overage billing
  are a billing-module concern, not cloud.
- **K8s metrics-server, OpenStack Ceilometer, Proxmox guest-agent
  password**: deployment-dependent or multi-step; documented as
  future, not faked.

### 16.4 What this does NOT change

The connector-only rule (§0.7): the trait extension adds
translation surface, not a built-in panel. No VM provisioning
engine, no templates, no console/VNC. The WHMCS comparison is
about the ABSTRACTION (what a provisioning module can express),
not about copying WHMCS's product model.

Moved (verbatim, `git mv`):
`src/modules/hosting/*` → `src/modules/cloud/hosting/*`;
`src/modules/providers/*` → `src/modules/cloud/providers/*`.
New: `src/modules/cloud.rs`, `src/modules/cloud/{module,containers,
virtualization,hooks,events,helpers,ai_capabilities}.rs`;
shims: `src/modules/hosting.rs`, `src/modules/providers.rs`
(rewritten as thin re-export + compat registration).
Edited: `src/modules/mod.rs` (mod + register), `src/modules/cloud/
hosting/types.rs` (shared `HostingState` accessor),
`src/modules/cloud/hosting/handlers_*.rs` + `cloud/providers/
handlers.rs` (entitlement gate),
`crates/unified-core/provider_registry_seed.json` (docker/k8s caps),
`src/modules/cloud/providers/adapters_cloud.rs` (Docker create,
K8s write ops + `request_json`),
`src/modules/cloud/providers/types.rs` (request struct extensions),
`src/modules/saas_packages/seed.rs` + `module.rs` (cloud seed),
`src/modules/panels/*` (one hook answer),
`src/modules/certificates/*` (one hook answer),
`assets/icons/modules/cloud.svg` + sprite + `ICONS.md`,
`MODULES.md`, this doc.

## 14. Explicitly NOT in this overhaul

- No VPS/VM panel, VM provisioning engine, hypervisor management,
  VM templates, console/VNC (external VPS manager project; task 378
  cancelled as a Unified task).
- No SolusVM adapter in core (hook contract only).
- No extraction of Virtualizor / panels / certificates to external
  modules (later; they stay working as-is).
- No direct libvirt/QMP driver (future external candidate).
- No image management / registry support for Docker (image must exist
  on the host or be pullable by name).
- No quota-pool enforcement across apps per plan (a plan is a tier,
  not a pool, in this round).
- No React UI for cloud management (separate workstream; the Untitled
  UI adoption task 374 runs in parallel).
- No change to the `clusters` (Multi-Cluster) module.

## 15. Verification plan

- `cargo fmt --check` on all touched files; `cargo check -p
  unified-core` + its registry tests (seed JSON parses; docker
  create / k8s create-scale-delete capabilities true).
- Root bin full check is NOT expected to reach exit 0 on this VM
  (OOM wall, standing caveat) — attempt the lean check once and
  report honestly; do not claim a clean root check from a truncated
  log. Rustfmt + targeted checks are the verification for the moved
  code (moves are verbatim; new code is small and self-contained).
- `npm run check` exit 0.
- Route proof by inspection: old paths present in compat modules'
  `configure_routes` + `route_info`; new paths in `CloudModule`;
  shared handlers confirmed by grep.
- Greps: no stale `crate::modules::hosting` / `crate::modules::
  providers` paths outside the shims; no Indonesian strings, no
  tnode/WordPress/TrayDigita in touched files.
- Unit tests added: seed capability values; hook-collect dedup
  (built-ins win, malformed skipped); app request → plan quota
  mapping; entitlement gate (control plane exempt).
