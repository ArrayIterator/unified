// Routes module - admin API for customizable page routes.
//
// HTTP API under `/api/v1/routes`:
//
//   GET    /                    list named routes (paginated, filterable)
//   GET    /{name}              one named route
//   PUT    /{name}              set a settings override
//                                {"path": "/app"}
//   DELETE /{name}?source=...   clear an override (default source=settings)
//   GET    /redirects           list active old-path redirects (paginated)
//
// Every handler requires an authenticated caller (`X-User-Id` +
// `X-Role` headers): `routes.view` for the GETs, `routes.manage`
// for PUT/DELETE. Override changes are audited with the
// authenticated caller as the actor, never with a body-supplied
// name.
//
// Every override change is announced on the hook bus as
// `route.changed` with `{"route", "old_path", "new_path", "source",
// "actor", "changed_at"}`. The audit module records the event once
// `"route.changed"` is added to its audited event list (see the
// snippet in the coordinator report); the payload already carries
// `actor`, `route`, `old_path`, and `new_path`, which is exactly what
// the audit recorder reads. No credential is involved in any route
// override, and none is ever logged or returned.
//
// The registry itself is the store: overrides live in memory in the
// `RouteRegistry` (same durability as the settings module's store).
// There is exactly one registry: the shared instance on `AppContext`
// (`ctx.routes`), seeded at startup from the `routes:` config
// section (`RoutesSection`) with the legacy `app.dashboard_path`
// config value as a deprecated fallback for `user_dashboard`
// (`routes.user_dashboard` is canonical; the legacy value only
// applies when the canonical one is unset - see
// `RouteRegistry::apply_legacy_dashboard_path`). `init` attaches
// that shared instance to this module, so the admin API, the server
// redirect handler, and the theme engine all read and write the
// same overrides.

mod views;
mod requests;
mod handlers;
pub mod helpers;
pub mod module;

// Public API (unchanged): re-exported from the split files.
// From helpers.rs:
pub use self::helpers::redirect_response;
pub use self::helpers::resolve_redirect;
pub use self::helpers::route_url;
// From module.rs:
pub use self::module::RoutesModule;
pub use self::module::register;
