// Cache module — HTTP access to the shared application cache.
//
// Routes under `/api/v1/cache`:
//
//   GET    /                              cache status (adapter, default TTL, Redis enabled, failover state)
//   GET    /{key}                         read one entry (404 on a miss or an expired entry)
//   POST   /                              store an entry: {"key", "value", "ttl_secs"?}
//   DELETE /{key}                         delete one entry
//   POST   /clear                         remove every entry
//   POST   /prune                         evict entries past the filesystem bound
//   POST   /invalidate                    {"prefix"} -> delete every entry under a key prefix
//   GET    /tenant/{tenant}/{key}         read one tenant-scoped entry
//   POST   /tenant/{tenant}               store a tenant-scoped entry: {"key", "value", "ttl_secs"?}
//   DELETE /tenant/{tenant}/{key}         delete one tenant-scoped entry
//   POST   /tenant/{tenant}/invalidate    delete every entry belonging to one tenant
//
// `value` is plain JSON on the way in and on the way out: strings,
// numbers, booleans, arrays and objects round-trip as themselves. Raw
// bytes are returned as a base64 string. `ttl_secs` omitted means the
// configured default TTL; `ttl_secs: 0` means the entry never expires.
//
// Capability checks: every handler requires the caller identity
// (`X-User-Id` + `X-Role` headers) and one capability, checked through
// the `role.check` hook (fails closed when no handler answers):
// `cache.read` for reads, `cache.write` for writes, `cache.manage`
// for clear / prune / invalidate. Tenant-scoped routes use the same
// capabilities; the tenant id comes from the URL, and the facade
// prefixes the key with `tenant:{id}:`, so a tenant entry is never
// reachable through the unscoped routes.
//
// The tenant id for a request normally comes from the middleware
// `TenantContext` (resolved from the Host header). These routes take
// it from the URL instead so an operator or a background job can
// address a tenant explicitly; a caller must hold the cache
// capability either way — the tenant id is a scope, not an
// authorization.

mod types;
mod handlers;
mod module;

pub use self::module::CacheModule;
pub use self::module::register;
