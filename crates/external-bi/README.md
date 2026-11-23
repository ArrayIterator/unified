# external-bi

BI / data-warehouse connector module for the Unified server. It lets
other modules (or the operator) declare *dataset shapes*, extract rows
for those shapes from hook providers, keep every extract as an
immutable snapshot, run small honest queries over a snapshot, and ship
snapshots to webhook or file destinations on a schedule.

## What it is not

- **Not a SQL engine.** Datasets declare a shape only; they never
  execute SQL. The query endpoint filters / groups / aggregates
  in-memory rows from one snapshot.
- **Not a live data warehouse.** All records are per-tenant and
  in-memory; there is no database table and no external warehouse
  connection.
- **No built-in cron daemon.** Schedules are evaluated only when a
  scheduler emits the `bi.schedule.tick` hook (or when `run-now` is
  called).

## Layout

| File | What lives there |
| --- | --- |
| `src/lib.rs` | Crate docs, event names, constants, re-exports, unit tests |
| `src/types.rs` | Dataset / snapshot / destination / schedule types + request bodies |
| `src/validate.rs` | Tenant, field, and row-shape validation |
| `src/export.rs` | CSV / JSONL rendering, webhook URL + file-path safety |
| `src/query.rs` | The query engine (filters, grouping, aggregations, sort, limit) |
| `src/state.rs` | In-memory per-tenant state, extracts, deliveries |
| `src/handlers.rs` | HTTP handlers (all under `/api/v1/bi/`) |
| `src/module.rs` | `BiModule`, hook registration, `register`, `module` |

## Tenancy

Every record carries a `tenant_id`. It is required on writes and
enforced as a filter on every read — there is no global view. Send it
as the `X-Unified-Tenant` header (preferred) or as a `tenant_id` field
in the JSON body; when both are present they must agree.

## Datasets

A dataset definition is **data, not code**: `name`, `source` (a hook
event name or a module id it extracts from — validated as a
non-empty string, never executed), `fields` (list of `{name, type}`
with `type` one of `string`, `number`, `boolean`, `datetime`),
`description`, `owner`. Datasets never execute SQL; they declare
shape only. Changing `fields` bumps `schema_version`, and snapshots
record the version that produced them.

| Route | Purpose |
| --- | --- |
| `POST /api/v1/bi/datasets` | Create a dataset |
| `GET /api/v1/bi/datasets` | List datasets for the tenant |
| `GET /api/v1/bi/datasets/{id}` | Get one dataset |
| `PUT /api/v1/bi/datasets/{id}` | Update a dataset |
| `DELETE /api/v1/bi/datasets/{id}` | Delete a dataset and its runs, snapshots, and schedules |

## Extracts and snapshots

`POST /api/v1/bi/datasets/{id}/extract` runs an extract now. The
module emits `bi.dataset.provide` with `{tenant_id, dataset_id,
source, fields}`; every registered provider contributes
`{"rows": [...]}` (or `{"error": "..."}`). Each row is validated
against the declared fields — a row missing a field, with a wrong
type, or with an unparseable datetime is rejected with a reason, and
extra keys are dropped. The run is recorded (`started` / `finished`,
rows accepted, rows rejected with reasons, duration), and a
successful extract stores an **immutable snapshot** (rows + schema
version + `extracted_at`). Snapshots are append-only: there are no
update or delete endpoints for them.

- `GET /api/v1/bi/datasets/{id}/extracts` — list runs.
- `GET /api/v1/bi/datasets/{id}/snapshots` — list snapshots
  (metadata only; fetch rows through query / export).
- `POST /api/v1/bi/datasets/{id}/extract` accepts an optional
  `destination_id` to deliver the fresh snapshot immediately.

### Seed provider

A provider for this module's own extract-run history is built in, so
an extract works end to end with zero external modules. Create a
dataset with `source: "bi.extract_runs"` and exactly these fields:

| Field | Type |
| --- | --- |
| `dataset_id` | string |
| `status` | string |
| `rows_accepted` | number |
| `rows_rejected` | number |
| `duration_ms` | number |
| `started_at` | datetime |

Extracting it returns one row per past extract run of the tenant
(including the current run's predecessors).

## Query

`POST /api/v1/bi/datasets/{id}/query` runs over the latest snapshot,
or a chosen `snapshot_id`. Body:

```json
{
  "filters": [{ "field": "region", "op": "contains", "value": "es" }],
  "group_by": "region",
  "aggregations": [
    { "agg": "count" },
    { "agg": "sum", "field": "amount" },
    { "agg": "avg", "field": "amount" }
  ],
  "sort": { "field": "sum_amount", "direction": "desc" },
  "limit": 100
}
```

- Filter ops: `=`, `!=`, `>`, `<`, `>=`, `<=`, and `contains`
  (strings only). Datetime filters compare RFC 3339 instants.
- Grouping: one field. Aggregations: `count`, `sum`, `avg`, `min`,
  `max` — the numeric ones require a `number` field and are
  validated as such.
- `limit` defaults to 100 and may not exceed 1000.
- The response is the computed result plus `row_count`,
  `rows_scanned`, `rows_matched`, and the `snapshot_id` /
  `schema_version` / `extracted_at` it ran against.

## Destinations

| Route | Purpose |
| --- | --- |
| `POST /api/v1/bi/destinations` | Create a destination |
| `GET /api/v1/bi/destinations` | List destinations |
| `GET /api/v1/bi/destinations/{id}` | Get one destination |
| `DELETE /api/v1/bi/destinations/{id}` | Delete a destination |
| `POST /api/v1/bi/destinations/{id}/test` | Dry-run with a 1-row sample |
| `GET /api/v1/bi/deliveries` | List delivery attempts |

- `webhook`: `{"kind": "webhook", "url": "https://..."}`. The
  module POSTs one NDJSON summary line plus one line per row
  (`content-type: application/x-ndjson`). The URL is validated at
  creation **and** at delivery: http/https only, no embedded
  credentials, and the host must resolve to public IPs only
  (loopback, private, link-local, multicast, reserved, CGNAT, and
  documentation ranges are rejected). Redirects are never followed,
  so a redirect cannot smuggle the request to a private host.
  Timeout is 10 seconds.
- `file`: `{"kind": "file", "path": "exports/orders.csv"}`. Writes
  CSV below the fixed base directory (`var/bi-exports` relative to
  the server working directory, overridable with the
  `BI_EXPORT_BASE_DIR` environment variable). Absolute paths and any
  `..` traversal are rejected. The test endpoint writes its sample to
  a sibling `<name>-test.<ext>` file and never overwrites the real
  file.

Every delivery attempt is logged with `status`
(`delivered` / `failed` / `deferred`), HTTP status code when there
is one, latency, and the error when there is one.

## Exports

`GET /api/v1/bi/datasets/{id}/export?format=csv|jsonl&snapshot_id=…`
streams the snapshot rows as CSV (proper quoting) or JSONL. This
endpoint returns raw file bytes with a content-disposition (like a
download); every other JSON response uses the standard
`{"data": ...}` envelope.

## Schedules

| Route | Purpose |
| --- | --- |
| `POST /api/v1/bi/schedules` | Create a schedule (`dataset_id`, `cron`, optional `destination_id`) |
| `GET /api/v1/bi/schedules` | List schedules |
| `GET /api/v1/bi/schedules/{id}` | Get one schedule |
| `DELETE /api/v1/bi/schedules/{id}` | Delete a schedule |
| `POST /api/v1/bi/schedules/{id}/run-now` | Run one immediately (extract + optional delivery) |

Cron expressions are 5 fields (`minute hour day-of-month month
day-of-week`) with ranges checked (`*`, lists, ranges, steps such as
`*/15`); the shape is validated at creation. Schedules are evaluated
only when the `bi.schedule.tick` hook fires — a scheduler must emit
that tick (about once a minute). There is no built-in cron daemon.
On a tick, due schedules extract and deliver: file destinations
deliver synchronously, webhook deliveries are spawned when the tick
runs inside an async runtime and otherwise recorded as `deferred`
(use `run-now` for a guaranteed delivery).

## Hooks

Emitted:

| Event | When |
| --- | --- |
| `bi.extract.completed` | An extract finished successfully (payload: run + snapshot id) |
| `bi.extract.failed` | An extract failed (payload: the run) |
| `bi.destination.failed` | A destination delivery failed (payload: the attempt) |

Listened to:

| Event | What it does |
| --- | --- |
| `bi.dataset.provide` | Dataset providers return rows; the built-in seed provider answers for `source = "bi.extract_runs"` |
| `bi.schedule.tick` | Evaluate due schedules (a scheduler must emit this) |

## Wiring (server side, not this crate)

- Root `Cargo.toml`: add `crates/external-bi` to `[workspace]
  members` and `external-bi = { path = "crates/external-bi" }` to
  `[dependencies]`.
- `src/modules/mod.rs::register_all`: add
  `external_bi::register(registry);` in the external-modules section.
- Rebuild and restart.

## Verification

`cargo check -p external-bi` (with `CARGO_BUILD_JOBS=1`,
`RUSTFLAGS="-C debuginfo=0"` on this memory-constrained VM) and
`cargo check -p external-bi --tests` / `cargo test -p external-bi`
for the unit tests (query aggregates, row-shape validation, cron
validation/matching, CSV quoting, public-IP check, export-path
safety, tenant validation, module metadata).

Known limits: no live warehouse connection and no real cron daemon
are expected — both are out of scope by design. `unified-core`
currently depends on the `unified-db-*` crates, and
`unified-db-education` has pre-existing compile errors in this tree,
so a plain in-tree `cargo check -p external-bi` fails on that
dependency before reaching this crate; the crate itself is verified
in an isolated scratch workspace against an API-compatible
`unified-core` stub (verbatim `hooks.rs` / `json.rs`) until that
upstream breakage is fixed.
