# Map & Tagging — shared geospatial module (task 358)

Status: built 2026-11-23. Migration `2026-11-23-000134_map_tagging`.
Module id: `maps` (`src/modules/maps.rs` + `src/modules/maps/`).
React: `/react/maps` (`assets/react/src/pages/MapsPage.tsx`,
`assets/react/src/components/MapView.tsx`).

## What it is

The SINGLE shared geospatial store for the whole platform. Every
other module that needs a location consumes this one — no module
keeps its own ad-hoc location table, pin store, or geofence
evaluator. It is the foundation the IoT / GPS tracker, logistics,
ISP (ODP / customer pins), customer location, branch offices,
products, and shipping features build on.

## Core module, always active

`MapsModule::is_core()` returns `true` explicitly (the trait
already defaults to true; the override makes the decision
visible) and `available_as_product()` returns `false`: the
foundation is not sold as a product and the extensions system
refuses to disable a core module (`disable_with_check` in
`src/modules/extensions/service.rs`: "Core module X cannot be
disabled"). A shared foundation that could be switched off would
break every consumer, so it cannot be.

## Dependency contract (the existing system — nothing new built)

Modules declare the dependency with the existing mechanism, in
both spellings (they are merged by `module_requires()` into
`ModuleInfo.dependencies`):

```rust
fn requires(&self) -> &[&'static str] {
    &["users", "roles", "store", "notifications", "scheduler", "assets", "maps"]
}

fn dependencies(&self) -> Vec<&'static str> {
    vec!["users", "roles", "store", "notifications", "scheduler", "assets", "maps"]
}
```

The behaviour is the extensions system's, enforced, not
documented wishful thinking (see `crates/unified-core/src/modules.rs`
and `src/modules/extensions/service.rs`):

- **Activation order + cycle detection**: `transitive_requires_ordered()`
  orders activation and refuses cycles.
- **Auto-activation**: enabling a module auto-activates its disabled
  dependencies (`will_auto_activate` / `auto_activated`). Maps is
  core, so it is already on — the declaration still matters: it is
  what makes the dependency visible and enforced.
- **Missing dependency blocks activation**: a `requires()` id that
  no registered module provides is `missing_dependency` and the
  activation is refused.
- **Disable blocking**: disabling a module is blocked while an
  enabled dependent still needs it (`dependents_of` check), unless
  forced — and for maps the core rule already forbids disabling it
  at all.

Declared consumers today (all already carry coordinates of their
own; the declaration is the contract, not a rewrite — see
"Follow-up migration work" below):

| Module | Why it consumes maps |
| --- | --- |
| `logistics` | Shipments, delivery routes, vehicles, drivers, warehouses all carry latitude/longitude; nearest-neighbour reorder + haversine live there today. |
| `properties` | Parcels, blocks, and developments are located; a pluggable geocoder provider (none / Nominatim) and 2D geometry live there today. |
| `fleet` (external crate `crates/external-fleet`, not wired into the binary yet) | Vehicles carry a last known position and trips record GPS fixes (`GpsPosition`, `last_latitude` / `last_longitude`). Declares `requires() -> &["maps"]`; the declaration takes effect when the crate is wired in. |

Do NOT add `requires() -> &["maps"]` to a module that does not
need locations: the declaration is a real dependency (it blocks
disabling and drives activation), not a label.

## Model

All tables are tenant-scoped by `tenant_id` and live in
`crates/unified-core/migrations/2026-11-23-000134_map_tagging/`;
the Diesel schema in `crates/unified-db-platform/src/schema/maps.rs`
mirrors them exactly.

| Table | Contents |
| --- | --- |
| `map_locations` | A pin: `label`, `kind`, `latitude`, `longitude`, `address`, `metadata` JSONB, optional `layer_id`. Coordinates are the truth; the address is display text only and is never geocoded by this module. |
| `map_location_links` | The generic join: `(tenant_id, entity_type, entity_id, location_id, role)`. ANY module attaches a location to its records. `entity_type` is the owning module's record kind (`shipment`, `warehouse`, `customer`, `odp`, `branch`, `product`, …); `role` is what the location is for that record (`home`, `office`, `delivery`, `asset`, …). |
| `map_layers` | Named pin layers: `name`, `description`, `visible`, `style` JSONB. A location belongs to at most one layer; deleting a layer unsets it. |
| `map_geofences` | Named zones: `name`, `description`, `geometry` (GeoJSON Polygon / MultiPolygon of `[longitude, latitude]`). Point-in-zone is evaluated by the runtime (ray casting), never by the database. |
| `map_positions` | Timestamped GPS positions for one entity (`entity_type` + `entity_id`): `latitude`, `longitude`, `accuracy_m`, `speed_kmh`, `heading_deg`, `source`, `recorded_at`. The tracking history GPS trackers and IoT devices write to. |
| `map_location_kinds` | Admin-definable location kinds (`slug`, `label`, `icon`, `color`, `enabled`). |
| `map_link_roles` | Admin-definable link roles (`slug`, `label`, `enabled`). |
| `map_providers` | Tile provider definitions (see "Provider registry" below). `tenant_id` NULL is a platform-shared definition; a tenant row overrides / extends the catalogue for that tenant only. |
| `map_settings` | One row per tenant: `active_provider`, `api_key_ciphertext` (encrypted, write-only), `default_latitude`, `default_longitude`, `default_zoom`. |

### Kinds and roles are data, not enums

The built-ins are a fallback, never the only values:

- Kinds: `pin`, `home`, `office`, `warehouse`, `store`, `customer`,
  `asset`, `vehicle`, `odp`, `branch`, `delivery-point`.
- Roles: `home`, `office`, `delivery`, `asset`, `billing`,
  `shipping`, `pickup`.

A tenant with no rows still gets the built-ins. A tenant row with
the same slug overrides the built-in (own label / icon / colour /
enabled). Creating a location with an unknown or disabled kind, or
attaching with an unknown or disabled role, is a 400 — kinds and
roles are definitions, not free text. Deleting a tenant override
reveals the built-in with the same slug again; stored locations
keep their kind string either way.

## Public contract

In-process (the stable surface other modules call — every function
takes the resolved tenant id, so a consumer can never forget the
scope; there is no unscoped entry point):

```rust
use crate::modules::maps::{
    attach_location, detach_location, locations_for_entity,
    nearby, within_geofence, geofences_containing, record_position,
};

// Attach a location to a shipment as its delivery point.
let link = attach_location(&ctx, tenant_id, "shipment", shipment_id, location_id, "delivery").await?;
// Every location attached to a record, each with its role.
let pins = locations_for_entity(&ctx, tenant_id, "customer", customer_id).await?;
// Locations within 10 km of a point, nearest first.
let close = nearby(&ctx, tenant_id, -6.2, 106.8, 10.0, None).await?;
// Point-in-zone tests.
let inside = within_geofence(&ctx, tenant_id, geofence_id, -6.2, 106.8).await?;
let zones = geofences_containing(&ctx, tenant_id, -6.2, 106.8).await?;
// Record a GPS fix (also evaluates geofence enter/exit).
let fix = record_position(&ctx, tenant_id, RecordPositionInput {
    entity_type: "vehicle".to_string(),
    entity_id: vehicle_id,
    latitude: -6.2, longitude: 106.8,
    accuracy_m: Some(5.0), speed_kmh: Some(42.0),
    heading_deg: Some(90.0), source: Some("gps".to_string()),
    recorded_at: None,
}).await?;
```

HTTP (all under `/api/v1/maps`, all authenticated, envelope
exactly `{"data": ...}` / `{"error": {"message", "code",
"metadata"}}`):

| Method | Path | Capability |
| --- | --- | --- |
| GET | `/locations[?kind&layer_id&page&per_page]` | `maps.view` |
| POST | `/locations` | `maps.manage` |
| GET/PUT/DELETE | `/locations/{id}` | `maps.view` / `maps.manage` / `maps.manage` |
| GET | `/nearby?latitude&longitude&radius_km&kind` | `maps.view` |
| POST / DELETE | `/links` | `maps.manage` |
| GET | `/entities/{entity_type}/{entity_id}/locations` | `maps.view` |
| GET/POST/PUT/DELETE | `/layers`, `/layers/{id}` | `maps.view` / `maps.manage` |
| GET/POST/PUT/DELETE | `/geofences`, `/geofences/{id}` | `maps.view` / `maps.manage` |
| POST | `/geofences/{id}/contains` | `maps.view` |
| POST | `/positions` | `maps.track` |
| GET | `/positions/{entity_type}/{entity_id}[?from&to]` | `maps.view` |
| GET/POST/PUT/DELETE | `/kinds`, `/kinds/{id}` | `maps.view` / `maps.manage` |
| GET/POST/PUT/DELETE | `/roles`, `/roles/{id}` | `maps.view` / `maps.manage` |
| GET/POST/PUT/DELETE | `/providers`, `/providers/{id}` | `maps.view` / `maps.manage` |
| GET / PUT | `/settings` | `maps.view` / `maps.manage` |

`radius_km` defaults to 10 and is capped at 500; the nearby
search prefilters by bounding box in SQL and computes the exact
haversine distance per row (same construction as the logistics
route heuristic). `maps.track` is separate from `maps.manage`
so a GPS tracker / IoT agent can be granted position writes
without full map administration.

## Hooks

Emitted fire-and-forget (other modules react without calling
this module; the event names are constants in
`unified_core::hooks::events`):

- `maps.location_created` / `maps.location_updated` /
  `maps.location_deleted` — `{tenant_id, location_id, kind, …}`.
- `maps.location_attached` / `maps.location_detached` —
  `{tenant_id, entity_type, entity_id, location_id, role}`.
- `maps.position_recorded` — `{tenant_id, entity_type,
  entity_id, latitude, longitude, recorded_at}`.
- `maps.geofence_entered` / `maps.geofence_exited` —
  `{tenant_id, entity_type, entity_id, geofence_id,
  geofence_name, latitude, longitude, recorded_at}`. Enter/exit
  is evaluated on every recorded position against the entity's
  previous fix: entering a zone fires `entered`, leaving fires
  `exited`; a first fix inside a zone fires `entered`.

## Provider registry

A provider is one tile source behind ONE interface: a tile URL
template with `{z}` / `{x}` / `{y}` placeholders (plus an
optional `{s}` subdomain placeholder and an optional `{key}`
browser-key placeholder), an attribution string, a maximum zoom,
an optional subdomain list, and a free-form `options` object.
The setting is per tenant (`map_settings.active_provider`);
the platform default is Leaflet / OpenStreetMap.

Built-in providers (seeded by migration 000134, editable /
disableable through the providers API — the runtime reads the
table and falls back to the built-ins only when the table has
no row for a slug):

| Slug | Provider | Key | Tile template |
| --- | --- | --- | --- |
| `osm` | OpenStreetMap (default) | No | `https://tile.openstreetmap.org/{z}/{x}/{y}.png` |
| `mapbox` | Mapbox | Yes (access token) | `https://api.mapbox.com/styles/v1/mapbox/streets-v12/tiles/{z}/{x}/{y}?access_token={key}` |
| `google` | Google Maps | Yes (browser key) | `https://mt1.google.com/vt/lyrs=m&x={x}&y={y}&z={z}&key={key}` |

A custom provider is the same shape as a row: an admin creates
one through `POST /api/v1/maps/providers` with `slug`,
`display_name`, `tile_url_template`, `attribution`,
`requires_api_key`, `max_zoom`, `subdomains`, `options`,
`enabled` — any open-source tile server or fully custom tile
API. `subdomains` accepts both spellings: `abc` (compact
Leaflet) or `a,b,c`. The template must be an http(s) URL
containing `{z}`, `{x}`, `{y}` and must not contain
credentials.

Rules that keep the registry safe:

- No API key lives in code, in a seed, or in a log. A key is
  supplied per tenant through `PUT /api/v1/maps/settings`
  (`api_key` is write-only: omit it to keep the stored key,
  send `""` to clear it) and stored encrypted with the same
  `v1.` HMAC-SHA256 keystream format the email providers module
  uses for OAuth secrets (`src/modules/maps/crypto.rs`), keyed
  by `app.secret_key` captured in module `init`. Storing a key
  without a configured secret is refused, fail closed.
- The key is a referrer-restricted browser key by design: it is
  only ever substituted into a tile URL served to authenticated
  tenant users with `maps.view`, and is never returned by any
  settings endpoint (only `has_api_key` / `key_configured`).
  Store only keys restricted by HTTP referrer to this deployment.
- A platform-shared provider row is read-only except for its
  `enabled` flag: one tenant must not rewrite the tile URL every
  other tenant renders from. A tenant that needs a different
  template creates its own provider row with the same slug —
  the tenant row shadows the platform one. Platform rows cannot
  be deleted (disable them instead).
- When the active provider needs a key and none is stored,
  `GET /api/v1/maps/settings` returns `tile_url: null` and the
  React map shows an honest "provider needs an API key" note
  instead of broken tiles. When the active provider is unknown
  or disabled, the settings resolve to the OpenStreetMap
  built-in rather than failing the map.

### Tiles are a runtime dependency — named honestly

Leaflet itself is BUNDLED: the `leaflet` npm dependency ships
its JS and CSS inside the React build, so the map needs no CDN
script. The TILES still come from the configured tile provider
at runtime — the default host is `tile.openstreetmap.org`.
A tenant that must not depend on it runs its own tile server
and points a custom provider row at it; nothing else changes.

## React

- `assets/react/src/components/MapView.tsx` — the Leaflet map.
  Pins are div icons (a coloured dot per kind, from
  `GET /api/v1/maps/kinds`), never Leaflet's default image
  markers, so no marker asset can 404 under the bundler.
  Clicking the map reports coordinates; clicking a pin selects
  it. When the provider changes, the tile layer URL swaps
  without a reload.
- `assets/react/src/pages/MapsPage.tsx` (`/react/maps`) — the
  admin page: map + location CRUD (click the map to fill
  coordinates), nearby search, locations-of-one-record lookup
  + attach, layers, geofences (GeoJSON input + contains test
  against the selected pin), kinds, roles, providers, and
  settings (active provider, default centre/zoom, write-only
  API key + clear). All data loads through `assets/react/src/api.ts`
  (`fetchMapLocations`, `fetchMapSettings`, …) with the standard
  envelope and CSRF flow; a failed load renders an honest error,
  never fabricated data.

## Tenant isolation (C3)

Every handler resolves the tenant FIRST (`require_tenant`),
then the verified caller, then the capability (`role.check`
hook, fail closed) — see `src/modules/maps/helpers.rs`. Writes
additionally require a resolved company scope
(`require_caller_company`); reads verify the company agreement
without requiring a company. Every query filters on the
resolved tenant id and every single-record load is tenant
scoped, so a tenant can never see or touch another tenant's
map data. There is no public-schema fallback: a request with
no resolved tenant context is rejected.

## What this module deliberately does NOT do

- **No geocoding.** The address on a location is display text.
  The properties module already has a pluggable geocoder
  (none / Nominatim); if a shared geocoder is ever needed, it
  belongs here as a provider-shaped registry, not as a second
  copy of the properties one.
- **No IP geolocation.** That is the `geo` module (MaxMind /
  CIDR overrides). Maps is pins, zones, and tracks.
- **No position retention job.** Positions accumulate; a
  cleanup / retention rule is future work, not a half-built
  scheduler entry.
- **No live migration of existing modules.** Logistics and
  properties keep their own latitude/longitude columns (see
  below); they gain the `requires()` declaration now and move
  their reads onto the maps contract as follow-up work.

## Follow-up migration work (existing ad-hoc location handling)

Verified by grep over `src/modules/` on 2026-11-23. These
modules hold location data of their own today and should
migrate onto the maps contract (attach locations through
`map_location_links`, read through `locations_for_entity` /
`nearby`, evaluate zones through `within_geofence`) — each as
its own task, with its own data migration. Nothing here was
silently rewritten: the coordinates stay where they are until
that work lands.

| Module | Ad-hoc handling today |
| --- | --- |
| `logistics` | `latitude` / `longitude` columns on shipments, delivery routes, vehicles, drivers, warehouses; haversine + nearest-neighbour reorder in `helpers.rs`. Already declares `requires() -> &["maps"]`. |
| `properties` | Full location subsystem: `location_handlers.rs`, `geometry.rs`, a pluggable geocoder provider (`providers.rs`: none / Nominatim), location search. Already declares `requires() -> &["maps"]`. |
| `fleet` (external crate) | GPS fixes (`GpsPosition`: latitude / longitude / speed) and vehicle last known position (`last_latitude` / `last_longitude`), in-memory store. Already declares `requires() -> &["maps"]`; GPS fixes should be recorded through `maps.record_position` when the crate is wired into the binary. |
| `events` | Optional venue `latitude` / `longitude` on the event (`types.rs`, `validation.rs`). Single coordinate pair, no store — lowest priority; attach a maps location when an event needs a pin. |
| `network` | Displays `latitude` / `longitude` from a `geo.lookup` result (`handlers.rs`) — display-only, no store; no migration needed. |
| `geo` | IP geolocation (MaxMind / CIDR overrides, `lookup.rs`) — a different domain (IP → location), not pins; no migration. |
| `privacy` | `redaction.rs` only lists `latitude` / `longitude` as sensitive field names for redaction — not location handling. |

There is NO `customers` module in this repo today (no such
directory under `src/modules/`). The `iot` module DOES exist
as an external crate (`crates/external-iot`) but holds no
location handling at all (device registry, telemetry ingestion,
device twins, commands, alerts — no coordinates anywhere), so
it declares no dependency on maps; when IoT devices start
reporting positions, they should write through
`maps.record_position` directly. The ISP ODP / customer-pin
consumers do not exist yet and should be built directly on the
maps contract when they arrive. The finished tasks 355
(document folders) and 356 (content tags + custom post types)
contain zero location handling (grep-verified), so nothing
there needs rework to consume maps.

## Verification

- `cargo check -p unified-core --offline` — exit 0 (233 warnings,
  same count as the pre-change baseline run).
- `cargo check -p unified-db-platform --offline` — exit 0.
- `cargo check -p external-fleet --offline` — exit 0 (covers the
  `requires() -> &["maps"]` change in that crate).
- `npm run check` — exit 0 on the second run. The first run
  failed with one real error (`MapsPage.tsx:502`: `role` does
  not exist on `MapLocation` — the entity-locations state was
  typed `MapLocation[]` instead of `EntityLocation[]`); fixed
  and re-ran clean.
- `npm run build:react` — exit 0 (Leaflet CSS/JS confirmed in
  the built bundle: `leaflet-container` in the dist CSS).
- Maps module typecheck via a throwaway scratch crate
  (`/tmp/maps-check`, real files symlinked in as a normal
  module tree, workspace `Cargo.lock` reused, shared target
  dir) — `cargo check --offline` exit 0, 0 errors, 0 warnings
  in the maps code. This check caught and fixed 2 real bugs
  after the first green-looking state: 5× E0521 (borrowed
  `&str` params captured into `'static` `spawn_blocking`
  closures in `attach_location`/`detach_location`/
  `locations_for_entity` — fixed by owning the strings first)
  and 2× E0382 (moved values reused in the hook payload in
  `detach_location` — fixed with separate closure copies),
  plus 1 dead-code warning (`is_encrypted` helper, removed).
- Maps unit tests via the same scratch crate —
  `cargo test --offline --lib` exit 0: **10 passed, 0 failed**
  (validation 6, providers 2, crypto 2). Note: the first
  `cargo test` run failed to LINK because this VM's
  `~/.cargo/config.toml` passes `-fuse-ld=mold` but mold is
  no longer installed (VM replacement wiped it); the test run
  used `RUSTFLAGS="-C link-arg=-fuse-ld=bfd"` as an override.
  Reinstalling mold (`apt install mold`) would restore the
  default path; the repo itself needs no change.
- `rustfmt --edition 2021 --check` clean on all new/edited
  Rust files; the two consumer edits
  (`logistics/module.rs`, `properties/module.rs`) were hand-wrapped
  to rustfmt style because those files already carry pre-existing
  rustfmt diffs unrelated to this task.

NOT verified (honest gaps): the full root-bin `cargo check`
OOM-kills on this VM, so `src/modules/mod.rs` registration
and the `logistics`/`properties` `requires()` edits have no
full-bin compile proof (they are one-line/array-level edits;
the identical change in `external-fleet` was verified with
`cargo check -p external-fleet`, exit 0). No live PostgreSQL
migration run; no live endpoint tests; store/handler logic
above the unit-tested pure functions is typechecked but not
executed against a database.
