// Frontend module - public pages: home, sign-in, registration,
// forgot-password, reset-password, and sign-out.
//
// The dashboards module owns the authenticated `/dashboard` and
// `/admin` scopes; this module owns the public surface that leads
// into them. Every path comes from the shared named-route registry
// (`AppContext.routes`), so a config or settings override for
// `login`, `register`, `forgot_password`, `reset_password`, or
// `logout` moves the page without a code change. The server-level
// redirect in `src/http/server.rs` covers an old path until the next
// restart, exactly as it does for the dashboard routes.
//
// Pages are server-rendered Handlebars through the theme engine,
// following the dashboards `render_page_response` pattern: a content
// template rendered first, then wrapped by the `layout` template.
// The pages post to the existing JSON APIs with `fetch` (see
// `assets/js/auth.js`): sign-in posts to
// `POST /api/v1/auth-manager/jwt/issue`, registration posts to
// `POST /api/v1/users`. There is no self-service password-reset
// endpoint in the auth manager, so the forgot-password and
// reset-password pages say so plainly and point at an administrator
// instead of posting to an endpoint that does not exist.

pub mod handlers;
pub mod module;

pub use self::module::{FrontendModule, register};
