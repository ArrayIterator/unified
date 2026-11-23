# external-fleet

Fleet and vehicle management external module for the Unified server.
Vehicles, drivers, trips, GPS positions, fuel logs, maintenance
schedules, and cost rollups — all per-tenant, in an in-memory store.

## What it does

| Area | Behaviour |
| --- | --- |
| Vehicles | CRUD under `/api/v1/fleet/vehicles`. Plate number validated (3–12 chars, letters/digits/spaces/hyphens, at least one letter and one digit) and unique per tenant; VIN validated as 17 characters (I, O, Q excluded) and unique per tenant; make/model/year, fuel type, acquisition date/cost, odometer. Status lifecycle `active → maintenance → retired`, with `retired` terminal. The odometer never moves backwards. |
| Vehicle types | Data-driven, not a closed enum: the type is a string validated against a per-tenant configurable list (`GET`/`PUT /api/v1/fleet/vehicle-types`). Defaults: `truck`, `van`, `car`, `motorcycle`, `other`. |
| Drivers | CRUD under `/api/v1/fleet/drivers`. Licence number unique per tenant; status `active`/`suspended`. Reads compute `days_until_license_expiry`, `license_expiring_soon` (within 30 days), and `license_expired`. |
| Trips | `POST /api/v1/fleet/trips` assigns a vehicle + driver, both of which must exist, be active, and have no overlapping live trip (overlap checked by time window; touching endpoints do not overlap). `PATCH /api/v1/fleet/trips/{id}/status` moves `planned → in_progress → completed \| cancelled`. Completing requires `end_odometer_km >= start_odometer_km` and records `distance_km`, `duration_minutes`, and the vehicle's new odometer. |
| GPS positions | `POST /api/v1/fleet/vehicles/{id}/positions` ingests lat (-90..90), lon (-180..180), speed (>= 0), timestamp (at most 5 minutes in the future). History is bounded to the last 1000 fixes per vehicle; each fix updates the vehicle's last-known position. `GET` returns the track. |
| Fuel logs | `POST /api/v1/fleet/fuel` (litres > 0, odometer >= last recorded odometer). `GET /api/v1/fleet/fuel?vehicle_id=…` returns the logs, per-interval efficiency, and overall km/litre — honestly `null` when fewer than two fill-ups exist. |
| Maintenance | `POST /api/v1/fleet/maintenance` (due by odometer and/or date). `GET /api/v1/fleet/maintenance/due` lists items due now: odometer reached OR date passed, computed from live vehicle data. `PATCH /api/v1/fleet/maintenance/{id}/complete` records the cost and, when next due values are given, creates the follow-up pending item. |
| Costs | `GET /api/v1/fleet/vehicles/{id}/costs` totals fuel + completed maintenance + completed trips, with a per-category breakdown. |

## Tenant isolation

Every record carries a `tenant_id`. The tenant is resolved per
request from the `X-Unified-Tenant` header (the same header the core
tenant middleware uses), else the body's `tenant_id` field, else the
`?tenant_id=` query parameter. A request with no tenant anywhere is
rejected with 400 — handlers never fall back to a global view. A
record owned by another tenant is reported as not found, so one
tenant can never probe another's ids.

## Hooks

Emitted (no external subscriptions — see below):

- `fleet.maintenance.due` — a maintenance item is due now (emitted
  when the due list is read and when an odometer change makes an
  item due).
- `fleet.trip.completed` — a trip completed, with distance and
  duration.
- `fleet.vehicle.retired` — a vehicle's status moved to `retired`.

The module subscribes to no external events. `logistics.*` event
names do not exist in `unified_core::hooks::events` (verified: the
only well-known events are the 26 core ones), so there is nothing
real to subscribe to; the module listens only to `app.start` (to
report itself) and emits its own `fleet.*` events.

## Routes

All routes live under `/api/v1/fleet/` and return the standard
envelope (`{"data": ...}` on success, `{"error": ...}` on failure).
See `route_info()` in `src/lib.rs` for the full list (25 routes).

## Wiring (server-side, not done by this crate)

1. Root `Cargo.toml`: add `external-fleet = { path =
   "crates/external-fleet" }` under `[dependencies]` and add
   `"crates/external-fleet"` to the `[workspace] members` list.
2. `src/modules/mod.rs::register_all`: add one line in the
   external-modules section: `external_fleet::register(registry);`
3. Rebuild and restart.

## Verification

```sh
cd crates/external-fleet
cargo check -p external-fleet
cargo check -p external-fleet --tests
```

The crate is intentionally a standalone workspace root (empty
`[workspace]` table in `Cargo.toml`) until step 1 above lands, so
it checks without editing the root `Cargo.toml`. When it becomes a
workspace member, remove that table and switch the versioned
dependencies to `dep.workspace = true`.

## Status

In-memory store only: all records are lost on restart. Persistence
(a Diesel migration in this crate, run from `init()`) is a
follow-up that must keep the same tenant filter on every query.
