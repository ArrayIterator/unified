// Storage module - HTTP access to the storage targets.
//
// Routes under `/api/v1/storage`:
//
//   GET    /                              storage status (kinds, target count)
//   GET    /targets                       list targets (paginated, secrets masked)
//   POST   /targets                       add a target (kind + config, secrets write-only)
//   GET    /targets/{name}                one target (secrets masked)
//   DELETE /targets/{name}                delete a target
//   POST   /targets/{name}/test           test the target connection
//   POST   /targets/{name}/objects?key=   upload object bytes (raw body)
//   GET    /targets/{name}/objects?key=   download object bytes (raw body)
//   DELETE /targets/{name}/objects?key=   delete an object
//   POST   /targets/{name}/signed-url     {"key", "expires_secs"?} -> {"url", ...}
//
// Targets added here are registered in the shared `StorageRegistry`
// on `AppContext` and live in memory; durable targets are declared in
// the `storage` configuration section and registered at startup.
// A target's configuration is never returned with secrets: secret
// fields are masked (see `StorageTargetConfig::masked_config`).
// Object downloads return raw bytes, not the JSON envelope, because
// the envelope cannot carry a binary body.
//
// Capability checks: every handler requires the caller identity
// (`X-User-Id` + `X-Role` headers) and one capability, checked
// through the `role.check` hook (fails closed): `storage.read` for
// reads, `storage.write` for object uploads / deletes / signed URLs,
// `storage.manage_targets` for target create / delete / test.
//
// Integrity: every upload records the SHA-256 of the bytes (returned
// in the response and in the `storage.object.uploaded` event). A
// download with `?expected_sha256=` verifies the bytes against that
// digest and refuses corrupt bytes instead of serving them; local
// and SFTP providers additionally verify against their durable
// sidecar on every plain download.
//
// Quotas: this is the raw provider API - it has no owner or tenant
// scope, so the hub quotas (storage-manager) cannot apply here.
// Targets created through this API may carry an optional
// `quota_bytes` / `max_objects` in their config, enforced here on
// every upload (409 on exceed, overwrite counts only the size
// delta). Quota-sensitive uploads with an owner scope must go
// through the storage-manager hub instead. Targets declared in
// configuration carry no target-level quota here.

mod handlers;
mod helpers;
pub mod module;
mod store;
mod types;

pub use self::module::*;
