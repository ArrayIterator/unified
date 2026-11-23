# external-iot

IoT device manager external module for the Unified server: per-tenant
device registry with per-device API keys, HTTP telemetry ingestion
with rate limits and aggregates, device twins, command queue, alert
rules, and fleet health. In-memory store.

Implements `unified_core::modules::Module` (id `"iot"`,
`is_core() == false`) in the exact shape of
`crates/example-hello-module`: one struct, `register(registry)`, and
a `module()` accessor. All routes live under `/api/v1/iot/` and all
responses use the standard success envelope (`{"data": ...}`) via
`unified_core::json`; errors use the standard error shape only.

## Honest limits

- **Ingestion is HTTP only.** No MQTT broker is embedded and no
  broker connection is opened. The MQTT topic mapping a broker
  deployment would use is documented below; a broker would translate
  those topics into calls to these same HTTP endpoints.
- **Storage is in-memory.** Restarting the server loses devices,
  telemetry, twins, commands, rules, and alerts. Persistence (Diesel
  migrations owned by this crate, run from `init()`) is a future step;
  `IotState` is the single place that would change.
- **No server-side auth is added here.** Admin endpoints take the
  tenant from the `X-Unified-Tenant` header (or an explicit
  `tenant_id` field) and rely on the server's own auth; device
  endpoints authenticate with the per-device API key described below.

## Tenancy

Every record carries a `tenant_id`. Writes require one, and every
read filters by it — there is no global view, and a device belonging
to another tenant reports 404 exactly like a missing device, so
tenant membership is never leaked through an error.

## Devices and per-device API keys

`POST /api/v1/iot/devices` provisions a device: `device_key` (unique
per tenant, 1–64 chars, letters/digits/`-_.`), `name`, `device_type`
(must be in the configured list — see below), `firmware_version`,
`location`, `metadata` (free-form JSON object, max 8 KiB).

The response returns the device **plus a generated per-device API key
(`device_key_plain`), shown exactly once**. Only its SHA-256 hash is
stored; the plaintext is never stored, never returned again, and
never logged. Lose it and the device must be re-provisioned (there is
deliberately no "show key again" endpoint).

Device endpoints authenticate with that key in the `X-Device-Key`
header (or `Authorization: Bearer <key>`), compared against the
stored hash in constant time.

Lifecycle: `provisioned → active → suspended → decommissioned`.
`decommissioned` is terminal — no transition out of it exists, and
decommissioned devices reject telemetry, commands, and twin writes.
Only `active` devices may send telemetry. `DELETE /devices/{id}`
removes the device and its telemetry, twin, and commands.

Device types are data, not an enum: the list lives in the module
state, defaults to `actuator, camera, controller, gateway, meter,
sensor, tracker, other`, is listed by `GET /api/v1/iot/device-types`,
and can be extended with `POST /api/v1/iot/device-types`.

## Telemetry

`POST /api/v1/iot/devices/{id}/telemetry` (device-key auth) accepts
`{metric, value, unit, timestamp?}`. The metric name, unit, and
finite numeric value are validated; `timestamp` (RFC 3339, defaults
to ingest time) is rejected when more than 5 minutes in the future.
Each device is limited to 60 ingests per minute (in-memory fixed
window; the 61st gets 429). Points are kept in a bounded ring buffer:
the last 500 points per metric per device.

`GET /api/v1/iot/devices/{id}/telemetry?metric=&from=&to=` returns
the stored points (oldest first) plus aggregates computed honestly
from exactly those points: `min`, `max`, `avg`, `count` (`min`/`max`/
`avg` are null when no points matched). `from`/`to` accept epoch
seconds or RFC 3339.

## Device twin

`GET /api/v1/iot/devices/{id}/twin` returns
`{desired, reported, in_sync}`. `PUT .../twin` (admin side) updates
the desired state and marks the twin out-of-sync when it differs.
`POST .../twin/reported` (device-key auth) updates the reported
state from the device and recomputes `in_sync` (deep JSON equality).
Both states are JSON objects, max 8 KiB each.

## Commands

`POST /api/v1/iot/devices/{id}/commands` queues a command
(`{name, params?, ttl_secs?}` — params max 4 KiB, TTL 1–86400 s,
default 3600 s). Devices poll with
`GET /api/v1/iot/devices/{id}/commands` (device-key auth; returns
queued commands by default, `?status=all` returns all with their
effective status). `POST .../commands/{cmd}/ack` records
`{status: "acked"|"failed", result?}` and moves the command to a
terminal state. A queued command past its TTL is reported as
`expired` on read (and persisted as expired on first observation);
expired or already-acknowledged commands reject further acks.

## Alert rules and alerts

`POST /api/v1/iot/alert-rules` creates a rule: a `device_id`, a
`device_type`, or both (at least one is required; a named device must
exist in the tenant), plus `metric`, `operator` (`>`, `<`, `>=`,
`<=`, `==`), `threshold`, `cooldown_secs` (0–86400), and `severity`
(`info`/`warning`/`critical`, default `warning`).

Evaluation runs on every telemetry ingest: when a rule matches the
device and metric, the condition holds, and the cooldown has passed
since the rule last fired, an alert is stored and the
`iot.alert.triggered` hook event is emitted. `GET /api/v1/iot/alerts`
lists alerts, filterable by `device_id` and `severity`.

## Fleet health

`GET /api/v1/iot/health?silent_threshold_secs=` (default 900 s, range
60–86400) returns, for the tenant: device counts by status, devices
silent longer than the threshold (never-seen devices count from
creation time), the out-of-sync twin count, and the open alert count.

## Routes

| Method | Path | Auth |
| --- | --- | --- |
| GET | `/api/v1/iot/device-types` | tenant |
| POST | `/api/v1/iot/device-types` | tenant |
| GET | `/api/v1/iot/health` | tenant |
| GET | `/api/v1/iot/alert-rules` | tenant |
| POST | `/api/v1/iot/alert-rules` | tenant |
| DELETE | `/api/v1/iot/alert-rules/{id}` | tenant |
| GET | `/api/v1/iot/alerts` | tenant |
| GET | `/api/v1/iot/devices` | tenant |
| POST | `/api/v1/iot/devices` | tenant |
| GET | `/api/v1/iot/devices/{id}` | tenant |
| PUT | `/api/v1/iot/devices/{id}` | tenant |
| DELETE | `/api/v1/iot/devices/{id}` | tenant |
| POST | `/api/v1/iot/devices/{id}/telemetry` | device key |
| GET | `/api/v1/iot/devices/{id}/telemetry` | tenant |
| GET | `/api/v1/iot/devices/{id}/twin` | tenant |
| PUT | `/api/v1/iot/devices/{id}/twin` | tenant |
| POST | `/api/v1/iot/devices/{id}/twin/reported` | device key |
| POST | `/api/v1/iot/devices/{id}/commands` | tenant |
| GET | `/api/v1/iot/devices/{id}/commands` | device key |
| POST | `/api/v1/iot/devices/{id}/commands/{cmd}/ack` | device key |

"Tenant" auth means the `X-Unified-Tenant` header (or an explicit
`tenant_id` field). List endpoints paginate with `page`/`per_page`
(max 100).

## Hook events emitted

| Event | When | Payload |
| --- | --- | --- |
| `iot.device.provisioned` | device created | `device_id`, `tenant_id`, `device_key`, `device_type` |
| `iot.device.decommissioned` | status reaches `decommissioned`, or device deleted | `device_id`, `tenant_id`, `device_key` (+ `deleted: true` on delete) |
| `iot.alert.triggered` | an alert rule fires on ingest | `alert_id`, `rule_id`, `tenant_id`, `device_id`, `metric`, `value`, `threshold`, `operator`, `severity` |

The module listens to nothing external: no well-known core event in
`unified_core::hooks::events` covers device lifecycle or telemetry,
so `register_hooks` is intentionally empty.

## MQTT topic mapping (for a broker deployment)

No broker is embedded. A deployment that fronts this module with an
MQTT broker would use these topics, translating each message into
the matching HTTP call with the same payload shape:

| Direction | Topic | Maps to |
| --- | --- | --- |
| device → server | `unified/iot/{tenant}/{device_key}/telemetry` | `POST .../devices/{id}/telemetry` (device key from the topic; the broker resolves `device_key` to the device id) |
| device → server | `unified/iot/{tenant}/{device_key}/twin/reported` | `POST .../devices/{id}/twin/reported` |
| device → server | `unified/iot/{tenant}/{device_key}/commands/ack` | `POST .../devices/{id}/commands/{cmd}/ack` |
| server → device | `unified/iot/{tenant}/{device_key}/commands` | queued commands, pushed or polled via `GET .../devices/{id}/commands` |
| server → device | `unified/iot/{tenant}/{device_key}/twin/desired` | desired twin state, as returned by `GET .../devices/{id}/twin` |

## Layout

- `src/lib.rs` — crate root: contract documentation, re-exports, and
  the unit tests.
- `src/types.rs` — domain types (device, telemetry point, twin,
  command, alert rule/alert) and the pure helpers
  (`allowed_transition`, `eval_operator`, `compute_aggregates`).
- `src/validate.rs` — input validation and device-key hashing helpers.
- `src/state.rs` — the in-memory `IotState` store: all CRUD, ingest,
  twin, command, rule-evaluation, and fleet-health logic.
- `src/handlers.rs` — Actix handlers: tenant resolution, device-key
  authentication, and the standard-envelope responses.
- `src/module.rs` — the `IotModule` `Module` implementation,
  `register()`, and `module()`.

## Wiring into the server

1. Remove the standalone `[workspace]` table from this crate's
   `Cargo.toml` (it exists only so the crate can be checked on its
   own; a crate cannot be both a member and its own workspace root).
2. Add `crates/external-iot` to the root workspace `members` and
   `external-iot = { path = "crates/external-iot" }` to the root
   `[dependencies]`.
3. Add one line in the external-modules section of
   `src/modules/mod.rs::register_all`:
   `external_iot::register(registry);`
4. Rebuild and restart. The module's `init()` attaches the hook bus
   from `AppContext`; routes and state need no other setup.

## Verify

From this crate's directory (standalone workspace root):

```sh
cargo check -p external-iot
cargo check -p external-iot --tests
cargo test -p external-iot
```

Unit tests cover device-key hashing/auth, per-tenant key
uniqueness, tenant isolation on reads, the status lifecycle
(including the terminal decommissioned state), telemetry rejection
for non-active devices and future timestamps, aggregates computed
from stored points, the 60/minute rate limit, twin sync logic,
alert-rule evaluation plus cooldown, device-type rule matching, and
command expiry/acknowledgement.
