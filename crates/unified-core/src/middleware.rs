// Middleware manager: a priority-ordered chain of HTTP middleware.
//
// Modules contribute middleware through `Module::register_middleware`;
// the server builds one `Dispatcher` from the manager and wraps the
// Actix application with it, so every request flows through the chain
// before reaching the route handlers. A middleware either calls `next`
// to continue the chain or short-circuits by returning its own
// response (an authentication redirect, for example).
//
// Priorities decide the order: higher priorities run first, and
// middleware with the same priority runs in registration order. The
// built-in request lifecycle (pause / maintenance blocking, request
// hooks, logging) lives in `the server HTTP layer` outside this chain,
// so it always runs first regardless of module priorities.

use crate::context::AppContext;
use crate::error::UnifiedError;
use crate::json;
use actix_service::boxed::BoxFuture;
use actix_web::body::{BoxBody, MessageBody};
use actix_web::dev::{Service, ServiceRequest, ServiceResponse, Transform, forward_ready};
use actix_web::http::header::{HeaderName, HeaderValue, HOST};
use actix_web::{HttpMessage, HttpRequest, HttpResponse};
use serde::Serialize;
use serde_json::Value;
use std::fmt::Debug;
use std::str::FromStr;
use std::future::{Ready, ready};
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::task::{Context, Poll};

/// The boxed future a middleware (or the rest of the chain) returns.
pub type MiddlewareResult = BoxFuture<Result<ServiceResponse<BoxBody>, actix_web::Error>>;

/// Calls the rest of the middleware chain with the request.
pub type NextFn = Box<dyn FnOnce(ServiceRequest) -> MiddlewareResult>;

/// One middleware in the chain.
///
/// `handle` receives the request and a `next` function for the rest of
/// the chain. Call `next(req).await` to continue; return a response
/// directly to short-circuit the chain.
pub trait Middleware: Send + Sync + Debug + 'static {
    /// Execution priority; higher runs first. Defaults to 0.
    fn priority(&self) -> isize {
        0
    }

    fn handle(&self, req: ServiceRequest, next: NextFn) -> MiddlewareResult;
}

/// The Actix `Transform` built from a `MiddlewareManager`. One
/// dispatcher is created per worker thread.
#[derive(Debug, Clone)]
pub struct Dispatcher(Rc<Vec<Arc<dyn Middleware>>>);

impl Dispatcher {
    pub fn new(middlewares: Vec<Arc<dyn Middleware>>) -> Self {
        Dispatcher(Rc::new(middlewares))
    }
}

/// The service the dispatcher wraps around the inner application.
#[derive(Debug, Clone)]
pub struct DispatcherMiddleware<S> {
    middlewares: Rc<Vec<Arc<dyn Middleware>>>,
    inner: Rc<S>,
}

/// Future returned by the dispatcher service. It converts the inner
/// service's response body into a boxed body.
pub struct DispatcherFuture<F> {
    fut: F,
}

impl<F, B> Future for DispatcherFuture<F>
where
    F: Future<Output = Result<ServiceResponse<B>, actix_web::Error>>,
    B: MessageBody + 'static,
{
    type Output = Result<ServiceResponse, actix_web::Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = unsafe { self.get_unchecked_mut() };
        match unsafe { Pin::new_unchecked(&mut this.fut) }.poll(cx) {
            Poll::Ready(Ok(res)) => Poll::Ready(Ok(res.map_into_boxed_body())),
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<S, B> Service<ServiceRequest> for DispatcherMiddleware<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = actix_web::Error> + 'static,
    B: MessageBody + 'static,
{
    type Response = ServiceResponse;
    type Error = actix_web::Error;
    type Future = DispatcherFuture<MiddlewareResult>;
    forward_ready!(inner);

    fn call(&self, req: ServiceRequest) -> Self::Future {
        DispatcherFuture {
            fut: Self::do_dispatch(self.middlewares.clone(), self.inner.clone(), req, 0),
        }
    }
}

impl<S, B> DispatcherMiddleware<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = actix_web::Error> + 'static,
    B: MessageBody + 'static,
{
    fn do_dispatch(
        middlewares: Rc<Vec<Arc<dyn Middleware>>>,
        inner: Rc<S>,
        req: ServiceRequest,
        index: usize,
    ) -> MiddlewareResult {
        if index >= middlewares.len() {
            let fut = inner.call(req);
            return Box::pin(async move { fut.await.map(|res| res.map_into_boxed_body()) });
        }
        let middleware = middlewares[index].clone();
        let middlewares_clone = middlewares.clone();
        let inner_clone = inner.clone();
        let next: NextFn = Box::new(move |request| {
            Self::do_dispatch(middlewares_clone, inner_clone, request, index + 1)
        });
        middleware.handle(req, next)
    }
}

impl<S, B> Transform<S, ServiceRequest> for Dispatcher
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = actix_web::Error> + 'static,
    B: MessageBody + 'static,
{
    type Response = ServiceResponse;
    type Error = actix_web::Error;
    type Transform = DispatcherMiddleware<S>;
    type InitError = ();
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        // Sort once per worker: higher priority first, registration
        // order kept within the same priority.
        let mut middlewares = (*self.0).clone();
        middlewares.sort_by(|a, b| b.priority().cmp(&a.priority()));
        ready(Ok(DispatcherMiddleware {
            middlewares: Rc::new(middlewares),
            inner: Rc::new(service),
        }))
    }
}

/// Wraps a plain function as middleware, for `register_function`.
#[derive(Clone)]
struct MiddlewareFunctionWrapper {
    func: Arc<dyn Fn(ServiceRequest, NextFn) -> MiddlewareResult + Send + Sync + 'static>,
}

impl Middleware for MiddlewareFunctionWrapper {
    fn handle(&self, req: ServiceRequest, next: NextFn) -> MiddlewareResult {
        (self.func)(req, next)
    }
}

impl Debug for MiddlewareFunctionWrapper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MiddlewareFunctionWrapper")
    }
}

/// Manages the ordered collection of middleware in the pipeline.
///
/// Middleware can be appended in registration order; the dispatcher
/// sorts by priority when the application is built.
#[derive(Debug, Clone, Default)]
pub struct MiddlewareManager {
    middlewares: Vec<Arc<dyn Middleware>>,
}

impl MiddlewareManager {
    /// Creates a new empty middleware manager.
    pub fn new() -> Self {
        Self {
            middlewares: Vec::new(),
        }
    }

    /// Append a middleware to the pipeline.
    pub fn register<M: Middleware>(&mut self, middleware: M) -> &mut Self {
        self.middlewares.push(Arc::new(middleware));
        self
    }

    /// Append a `Default`-constructible middleware to the pipeline.
    pub fn register_by_type<M: Middleware + Default>(&mut self) -> &mut Self {
        self.register(M::default())
    }

    /// Append a plain function as middleware.
    pub fn register_function<F>(&mut self, func: F) -> &mut Self
    where
        F: Fn(ServiceRequest, NextFn) -> MiddlewareResult + Send + Sync + 'static,
    {
        self.middlewares.push(Arc::new(MiddlewareFunctionWrapper {
            func: Arc::new(func),
        }));
        self
    }

    /// Number of registered middleware.
    pub fn len(&self) -> usize {
        self.middlewares.len()
    }

    /// True when no middleware is registered.
    pub fn is_empty(&self) -> bool {
        self.middlewares.is_empty()
    }

    /// Remove every registered middleware.
    pub fn clear(&mut self) {
        self.middlewares.clear();
    }

    /// Build the Actix dispatcher for this pipeline. The dispatcher
    /// sorts by priority when it is installed on a worker.
    pub fn create_dispatcher(&self) -> Dispatcher {
        Dispatcher::new(self.middlewares.clone())
    }
}

impl IntoIterator for MiddlewareManager {
    type Item = Arc<dyn Middleware>;
    type IntoIter = std::vec::IntoIter<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        self.middlewares.into_iter()
    }
}

// Tenant resolution (multi-tenant SaaS, batch 8)
//
// The main site is the control plane (admin + user dashboard live
// there). A tenant host (provisioned subdomain or verified custom
// domain) resolves into a tenant context attached to the request; a
// tenant host serves services only, never control-plane pages.
//
// Hook contract (answered by the domains module; this middleware only
// codes against the contract, never against that module directly):
//
//   - Request: the middleware emits `tenant.resolve_requested` with
//     payload `{"host": "<Host header value, may include :port>"}` and
//     reads the FIRST result. The result is either absent (control
//     plane) or an object:
//       `{"tenant_id": ..., "company_id": ..., "site_id": ...,
//         "kind": "subdomain" | "custom_domain", "verified": true,
//         "host": ...}`
//     or a blocked object:
//       `{"kind": "blocked", "verified": false,
//         "reason": "unverified_domain" | "suspended"}`.
//   - A blocked result NEVER serves tenant data, tenant pages, or
//     tenant APIs for that host, and a suspended tenant stops serving
//     even previously-verified custom domains.
//   - Malformed or absent results and hook errors are treated as
//     "unresolved" and fall through to the configured fallback policy
//     (404 by default - fail closed), never as a tenant.
//   - Audit: when a control-plane path is blocked on a resolved
//     tenant host, the middleware emits `tenant.control_plane_blocked`
//     (fire-and-forget; the request does not fail when no handler
//     answers).
//
// Configuration (nothing in this section is hard-coded behaviour: every
// list, value, and fallback below is a default in
// `TenantResolutionConfig` that can be overridden without a code
// change, through the builders, or through environment variables that
// map 1:1 to the `saas.*` config keys; the typed `[saas]` section in
// the config file is still open - see `TenantResolutionConfig`):
//
//   - `saas.main_host` (`UNIFIED_SAAS_MAIN_HOST`): the main-site host.
//     Requests whose host equals it skip the hook call entirely, so
//     the control-plane path stays cheap. Unset by default.
//   - `saas.blocked_control_plane_paths`
//     (`UNIFIED_SAAS_BLOCKED_PATHS`, comma-separated): control-plane
//     path prefixes blocked on tenant hosts. Defaults:
//     `/admin`, `/dashboard`, `/api/v1/admin`, plus the user-dashboard
//     verification settings pages `/dashboard/settings/domains`,
//     `/dashboard/domains`, `/dashboard/verification` (listed
//     explicitly so they stay blocked even if `/dashboard` itself is
//     removed from the list).
//   - `saas.exempt_paths` (`UNIFIED_SAAS_EXEMPT_PATHS`,
//     comma-separated): paths that bypass resolution entirely.
//     Defaults: `/health` and `/.well-known/acme-challenge/` (a health
//     probe or a certificate challenge must answer on any host).
//   - `saas.tenant_header` (`UNIFIED_SAAS_TENANT_HEADER`): response
//     header carrying the tenant id for observability. Default:
//     `X-Unified-Tenant`. It never carries the full tenant context.
//   - `saas.unknown_host_policy` (`UNIFIED_SAAS_UNKNOWN_HOST_POLICY`)
//     and `saas.missing_host_policy`
//     (`UNIFIED_SAAS_MISSING_HOST_POLICY`): fallback for a host the
//     hook does not resolve, and for a request with no Host header.
//     Values: `not_found` (default: fail closed, answer 404 in the
//     standard error envelope) or `control_plane` (explicit operator
//     opt-in only: serve the request as the control plane; an
//     operator chooses it knowingly, it is never the default).
//     Neither policy ever serves tenant data for an unresolved host.
//
// Path matching is prefix-based with a segment boundary: a prefix
// matches a path exactly or followed by `/`, so `/admin` never
// matches `/administrator`.

/// Hook event emitted to resolve a request host to a tenant. Answered
/// by the domains module; see the section header above for the payload
/// and result contract.
pub const EVENT_TENANT_RESOLVE_REQUESTED: &str = "tenant.resolve_requested";

/// Hook event emitted (fire-and-forget) when a control-plane path is
/// blocked on a resolved tenant host.
pub const EVENT_TENANT_CONTROL_PLANE_BLOCKED: &str = "tenant.control_plane_blocked";

/// Default response header carrying the resolved tenant id. Overridable
/// with config key `saas.tenant_header`.
pub const DEFAULT_TENANT_HEADER: &str = "X-Unified-Tenant";

/// Default control-plane path prefixes blocked on tenant hosts.
/// Overridable with config key `saas.blocked_control_plane_paths`.
pub const DEFAULT_BLOCKED_PATHS: &[&str] = &[
    "/admin",
    "/dashboard",
    "/api/v1/admin",
    "/dashboard/settings/domains",
    "/dashboard/domains",
    "/dashboard/verification",
];

/// Default paths that bypass tenant resolution entirely. Overridable
/// with config key `saas.exempt_paths`.
pub const DEFAULT_EXEMPT_PATHS: &[&str] = &["/health", "/.well-known/acme-challenge/"];

/// Normalize a Host header value (or a configured main host) for
/// comparison: trim, drop a `:port` suffix, lower-case, drop a trailing
/// dot. Returns `None` for an empty, over-long, or malformed host
/// (whitespace, control characters, anything but letters, digits,
/// `-` and `.`, empty or over-long labels, labels starting or ending
/// with `-`, or an IP literal). A `None` host never resolves to a
/// tenant.
pub fn normalize_host(raw: &str) -> Option<String> {
    let host = raw.trim();
    if host.is_empty() || host.len() > 253 {
        return None;
    }
    if host.contains(|c: char| c.is_whitespace() || c.is_control()) {
        return None;
    }
    if host.starts_with('[') {
        return None;
    }
    let without_port = match host.rfind(':') {
        Some(index)
            if !host[index + 1..].is_empty()
                && host[index + 1..].chars().all(|c| c.is_ascii_digit()) =>
        {
            &host[..index]
        }
        _ => host,
    };
    let normalized = without_port.trim_end_matches('.').to_ascii_lowercase();
    if normalized.is_empty() || normalized.len() > 253 || normalized.contains("..") {
        return None;
    }
    if !normalized
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
    {
        return None;
    }
    for label in normalized.split('.') {
        if label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
        {
            return None;
        }
    }
    Some(normalized)
}

/// Normalize a configured path-prefix list: trim, force a leading
/// `/`, drop a trailing `/`, drop empties, de-duplicate (first wins).
/// A list that normalizes to empty disables that list's matching.
pub fn normalize_path_list(paths: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in paths {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let prefixed = if trimmed.starts_with('/') {
            trimmed.to_string()
        } else {
            format!("/{trimmed}")
        };
        let path = prefixed.trim_end_matches('/').to_string();
        let path = if path.is_empty() { "/".to_string() } else { path };
        if !out.contains(&path) {
            out.push(path);
        }
    }
    out
}

/// True when `path` equals a configured prefix or sits under it
/// (segment boundary: the prefix followed by `/`).
fn path_matches_any(path: &str, prefixes: &[String]) -> bool {
    let path = path.trim_end_matches('/');
    let path = if path.is_empty() { "/" } else { path };
    prefixes
        .iter()
        .any(|prefix| path == prefix || path.starts_with(&format!("{prefix}/")))
}

/// Fallback policy for a host that cannot be resolved to a tenant:
/// what happens when the hook gives no (usable) answer for a host, or
/// when the request carries no Host header at all. Config keys
/// `saas.unknown_host_policy` and `saas.missing_host_policy`.
///
/// Fail closed by default: `NotFound` is the default policy, so an
/// unresolved host is answered 404 and never served as the control
/// plane. `ControlPlane` is an explicit operator opt-in only (config
/// key / environment variable set to `control_plane`): an operator
/// chooses it knowingly, for example while the resolver is being
/// wired up, and the pass-through is logged as a warning every time
/// it happens (see `unresolved()`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnresolvedHostPolicy {
    /// Serve the request as the control plane. Explicit operator
    /// opt-in only - never the default (see the enum docs).
    ControlPlane,
    /// Answer 404 in the standard error envelope. The fail-closed
    /// default.
    #[default]
    NotFound,
}

impl UnresolvedHostPolicy {
    /// Parse a policy value from config: `control_plane` (also
    /// `control-plane`, `controlplane`, `control`) or `not_found`
    /// (also `not-found`, `404`, `reject`). Anything else is `None`
    /// and the caller keeps its current value.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "control_plane" | "control-plane" | "controlplane" | "control" => {
                Some(Self::ControlPlane)
            }
            "not_found" | "not-found" | "404" | "reject" => Some(Self::NotFound),
            _ => None,
        }
    }
}

/// How a resolved tenant host was reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TenantKind {
    Subdomain,
    CustomDomain,
}

/// The tenant a request resolved to. Attached to the request
/// extensions by `TenantResolutionMiddleware` so handlers can read it
/// without a second lookup; only the tenant id is ever exposed on the
/// wire (response header, see `TenantResolutionConfig`).
///
/// Product scope: a resolution that came from a per-product
/// subdomain (saas_domains answers it with `scope: "product"`)
/// also carries the product / package / product-subdomain ids, so
/// handlers can tell "this host serves one product" from "this host
/// serves the whole tenant". All four product fields are `None` for
/// ordinary tenant-subdomain and custom-domain resolutions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TenantContext {
    pub tenant_id: String,
    pub company_id: String,
    pub site_id: String,
    pub kind: TenantKind,
    /// Normalized host the request arrived on.
    pub host: String,
    /// `"product"` when this host serves one product subdomain,
    /// `None` otherwise.
    pub scope: Option<String>,
    /// Product id for a product-subdomain resolution, else `None`.
    pub product_id: Option<String>,
    /// Package id for a product-subdomain resolution, else `None`.
    pub package_id: Option<String>,
    /// Product-subdomain id for a product-subdomain resolution,
    /// else `None`.
    pub product_subdomain_id: Option<String>,
}

impl TenantContext {
    /// Read the tenant context the middleware attached to a handler
    /// request, if this request resolved to a tenant.
    pub fn from_request(req: &HttpRequest) -> Option<TenantContext> {
        req.extensions().get::<TenantContext>().cloned()
    }

    /// Same as `from_request`, for a `ServiceRequest` inside middleware.
    pub fn from_service_request(req: &ServiceRequest) -> Option<TenantContext> {
        req.extensions().get::<TenantContext>().cloned()
    }

    /// True when the request resolved to a tenant host.
    pub fn is_tenant_request(req: &HttpRequest) -> bool {
        Self::from_request(req).is_some()
    }
}

/// Configuration for `TenantResolutionMiddleware`. Every field is a
/// default that an operator overrides without a code change: through
/// the builders, through `from_environment` (environment variables
/// mapping 1:1 to the `saas.*` config keys), or - once the integrator
/// adds the typed `[saas]` section to `AppConfig` (open item; the
/// section does not exist yet, so the server currently builds this
/// config with `from_environment` and the single construction site in
/// `src/http/server.rs` is where the typed section plugs in) - from
/// the config file.
#[derive(Debug, Clone)]
pub struct TenantResolutionConfig {
    /// Main-site host (config key `saas.main_host`). Requests on this
    /// host skip the hook call entirely. `None` means no host is
    /// treated as the main host and every host goes through the hook.
    pub main_host: Option<String>,
    /// Control-plane path prefixes blocked on tenant hosts (config
    /// key `saas.blocked_control_plane_paths`).
    pub blocked_paths: Vec<String>,
    /// Paths that bypass resolution entirely (config key
    /// `saas.exempt_paths`).
    pub exempt_paths: Vec<String>,
    /// Response header carrying the tenant id (config key
    /// `saas.tenant_header`).
    pub tenant_header_name: String,
    /// Fallback for a host the hook does not resolve (config key
    /// `saas.unknown_host_policy`). Default `NotFound` (fail closed);
    /// `ControlPlane` is an explicit operator opt-in only.
    pub unknown_host_policy: UnresolvedHostPolicy,
    /// Fallback for a request with no Host header (config key
    /// `saas.missing_host_policy`). Default `NotFound` (fail closed);
    /// `ControlPlane` is an explicit operator opt-in only.
    pub missing_host_policy: UnresolvedHostPolicy,
}

impl Default for TenantResolutionConfig {
    fn default() -> Self {
        Self {
            main_host: None,
            blocked_paths: DEFAULT_BLOCKED_PATHS
                .iter()
                .map(|path| path.to_string())
                .collect(),
            exempt_paths: DEFAULT_EXEMPT_PATHS
                .iter()
                .map(|path| path.to_string())
                .collect(),
            tenant_header_name: DEFAULT_TENANT_HEADER.to_string(),
            // Fail closed by default (C4): an unresolved host is
            // answered 404. `ControlPlane` is an explicit operator
            // opt-in via the `saas.*_host_policy` config keys /
            // `UNIFIED_SAAS_*_HOST_POLICY` variables, never the
            // default.
            unknown_host_policy: UnresolvedHostPolicy::NotFound,
            missing_host_policy: UnresolvedHostPolicy::NotFound,
        }
    }
}

impl TenantResolutionConfig {
    /// Normalize every field in place (host, path lists, header
    /// name). An invalid main host normalizes to `None` (unconfigured)
    /// rather than failing startup; an empty header name falls back
    /// to the default.
    pub fn normalized(mut self) -> Self {
        self.main_host = self.main_host.as_deref().and_then(normalize_host);
        self.blocked_paths = normalize_path_list(&self.blocked_paths);
        self.exempt_paths = normalize_path_list(&self.exempt_paths);
        let header = self.tenant_header_name.trim();
        self.tenant_header_name = if header.is_empty() {
            DEFAULT_TENANT_HEADER.to_string()
        } else {
            header.to_string()
        };
        self
    }

    /// Set the main-site host (config key `saas.main_host`).
    pub fn with_main_host(mut self, host: impl Into<String>) -> Self {
        self.main_host = Some(host.into());
        self
    }

    /// Replace the blocked control-plane path list (config key
    /// `saas.blocked_control_plane_paths`).
    pub fn with_blocked_paths(mut self, paths: Vec<String>) -> Self {
        self.blocked_paths = paths;
        self
    }

    /// Replace the exempt path list (config key `saas.exempt_paths`).
    pub fn with_exempt_paths(mut self, paths: Vec<String>) -> Self {
        self.exempt_paths = paths;
        self
    }

    /// Replace the tenant response header name (config key
    /// `saas.tenant_header`).
    pub fn with_tenant_header_name(mut self, name: impl Into<String>) -> Self {
        self.tenant_header_name = name.into();
        self
    }

    /// Set the fallback for hosts the hook does not resolve (config
    /// key `saas.unknown_host_policy`). The default is `NotFound`
    /// (fail closed); pass `ControlPlane` only as an explicit
    /// operator opt-in.
    pub fn with_unknown_host_policy(mut self, policy: UnresolvedHostPolicy) -> Self {
        self.unknown_host_policy = policy;
        self
    }

    /// Set the fallback for requests with no Host header (config key
    /// `saas.missing_host_policy`). The default is `NotFound` (fail
    /// closed); pass `ControlPlane` only as an explicit operator
    /// opt-in.
    pub fn with_missing_host_policy(mut self, policy: UnresolvedHostPolicy) -> Self {
        self.missing_host_policy = policy;
        self
    }

    /// Build the config from the environment, over the defaults. Each
    /// variable maps 1:1 to a `saas.*` config key and is read only
    /// when set and non-empty (an empty or unparseable value keeps the
    /// default, so a typo can never switch the fallback policy or
    /// clear the block list by accident):
    ///
    /// - `UNIFIED_SAAS_MAIN_HOST` -> `saas.main_host`
    /// - `UNIFIED_SAAS_BLOCKED_PATHS` (comma-separated) ->
    ///   `saas.blocked_control_plane_paths`
    /// - `UNIFIED_SAAS_EXEMPT_PATHS` (comma-separated) ->
    ///   `saas.exempt_paths`
    /// - `UNIFIED_SAAS_TENANT_HEADER` -> `saas.tenant_header`
    /// - `UNIFIED_SAAS_UNKNOWN_HOST_POLICY` ->
    ///   `saas.unknown_host_policy`
    /// - `UNIFIED_SAAS_MISSING_HOST_POLICY` ->
    ///   `saas.missing_host_policy`
    pub fn from_environment() -> Self {
        let mut config = Self::default();
        if let Ok(value) = std::env::var("UNIFIED_SAAS_MAIN_HOST") {
            if !value.trim().is_empty() {
                config.main_host = Some(value);
            }
        }
        if let Ok(value) = std::env::var("UNIFIED_SAAS_BLOCKED_PATHS") {
            let paths = split_env_list(&value);
            if !paths.is_empty() {
                config.blocked_paths = paths;
            }
        }
        if let Ok(value) = std::env::var("UNIFIED_SAAS_EXEMPT_PATHS") {
            let paths = split_env_list(&value);
            if !paths.is_empty() {
                config.exempt_paths = paths;
            }
        }
        if let Ok(value) = std::env::var("UNIFIED_SAAS_TENANT_HEADER") {
            if !value.trim().is_empty() {
                config.tenant_header_name = value;
            }
        }
        if let Ok(value) = std::env::var("UNIFIED_SAAS_UNKNOWN_HOST_POLICY") {
            if let Some(policy) = UnresolvedHostPolicy::parse(&value) {
                config.unknown_host_policy = policy;
            }
        }
        if let Ok(value) = std::env::var("UNIFIED_SAAS_MISSING_HOST_POLICY") {
            if let Some(policy) = UnresolvedHostPolicy::parse(&value) {
                config.missing_host_policy = policy;
            }
        }
        config.normalized()
    }
}

/// Split a comma-separated environment list into trimmed, non-empty items.
fn split_env_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}

/// Why a host is blocked from serving tenant data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockedReason {
    UnverifiedDomain,
    Suspended,
    /// A per-product subdomain that was released: the product is no
    /// longer served on this host. Answered with a distinct message
    /// (and a `location_hint` pointing at the main host) so the
    /// product flow can serve an export notice instead of the
    /// product. The full export-notice page itself is a follow-up
    /// for the product/release flow - the middleware only signals
    /// the state, it carries no export URL in the contract.
    Released,
}

impl BlockedReason {
    fn as_str(&self) -> &'static str {
        match self {
            Self::UnverifiedDomain => "unverified_domain",
            Self::Suspended => "suspended",
            Self::Released => "released",
        }
    }

    /// The hook payload names the reason; anything other than
    /// `suspended` or `released` is treated as `unverified_domain`
    /// (fail closed: an unknown reason never serves tenant data).
    fn from_reason(raw: Option<&str>) -> Self {
        match raw {
            Some("suspended") => Self::Suspended,
            Some("released") => Self::Released,
            _ => Self::UnverifiedDomain,
        }
    }
}

/// Read an optional id from a hook result: absent means `None`,
/// present means it must pass `is_valid_id`, else the whole answer
/// is unusable (`None` here signals that). Used for the
/// product-scope ids, which only exist on product-subdomain
/// resolutions.
fn optional_id(map: &serde_json::Map<String, Value>, key: &str) -> Option<Option<String>> {
    match map.get(key).and_then(Value::as_str) {
        None => Some(None),
        Some(raw) if is_valid_id(raw) => Some(Some(raw.to_string())),
        Some(_) => None,
    }
}

/// Outcome of resolving one request host.
#[derive(Debug, Clone)]
enum TenantResolution {
    /// Serve as the control plane (main host, or the fallback policy
    /// said so).
    ControlPlane,
    /// Serve tenant services with this context attached.
    Tenant(TenantContext),
    /// Never serve tenant data for this host (unverified domain or
    /// suspended tenant). Carries the normalized host for the error
    /// metadata.
    Blocked { reason: BlockedReason, host: String },
    /// The hook gave no usable answer for this host; the caller applies
    /// the configured fallback policy.
    Unresolved,
}

/// True when an id from a hook result is safe to trust and to send
/// back as a header value: non-empty, at most 128 characters,
/// letters, digits, `-` and `_` only.
fn is_valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Tenant-resolution middleware: resolves the Host header through the
/// `tenant.resolve_requested` hook, attaches the tenant context to
/// resolved tenant requests, blocks control-plane paths on tenant
/// hosts, and gates unverified / suspended hosts. Runs at priority
/// 950: after the security-headers middleware (1000) so its error
/// responses still carry the security headers, and before the IP
/// filter (900), rate limiting (800), request-size guard (700), and
/// all module middleware, so tenant routing is decided before any
/// route dispatch or module logic runs.
#[derive(Clone)]
pub struct TenantResolutionMiddleware {
    ctx: Arc<AppContext>,
    config: TenantResolutionConfig,
    header_name: HeaderName,
}

impl std::fmt::Debug for TenantResolutionMiddleware {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TenantResolutionMiddleware")
    }
}

impl TenantResolutionMiddleware {
    /// Build the middleware with an explicit config (see
    /// `TenantResolutionConfig` for the config keys). The config is
    /// normalized here, so builders may pass unnormalized values.
    pub fn new(ctx: Arc<AppContext>, config: TenantResolutionConfig) -> Self {
        let config = config.normalized();
        let header_name = HeaderName::from_str(&config.tenant_header_name)
            .unwrap_or_else(|_| HeaderName::from_static("x-unified-tenant"));
        Self {
            ctx,
            config,
            header_name,
        }
    }

    /// Build the middleware with the documented defaults.
    pub fn with_defaults(ctx: Arc<AppContext>) -> Self {
        Self::new(ctx, TenantResolutionConfig::default())
    }

    /// The effective (normalized) config this middleware runs with.
    pub fn config(&self) -> &TenantResolutionConfig {
        &self.config
    }

    fn is_exempt_path(&self, path: &str) -> bool {
        path_matches_any(path, &self.config.exempt_paths)
    }

    fn is_blocked_path(&self, path: &str) -> bool {
        path_matches_any(path, &self.config.blocked_paths)
    }

    fn tenant_header_value(&self, tenant: &TenantContext) -> Option<HeaderValue> {
        HeaderValue::from_str(&tenant.tenant_id).ok()
    }

    /// Resolve one host through the hook contract. `raw_host` is the
    /// Host header value as received (it may include `:port`);
    /// `normalized_host` is the same host after `normalize_host`.
    /// Defensive by design: no result, a non-object result, a result
    /// missing its tenant ids, a tenant-kind result that is not
    /// `verified: true`, or a result naming a different host all come
    /// back `Unresolved` - never as a tenant.
    fn resolve(&self, raw_host: &str, normalized_host: &str) -> TenantResolution {
        let context = self.ctx.hooks.emit(
            EVENT_TENANT_RESOLVE_REQUESTED,
            serde_json::json!({ "host": raw_host }),
        );
        let Some(result) = context.results.first() else {
            return TenantResolution::Unresolved;
        };
        let Value::Object(map) = result else {
            return TenantResolution::Unresolved;
        };
        let kind = map.get("kind").and_then(Value::as_str).unwrap_or("");
        if kind == "blocked" {
            let reason = BlockedReason::from_reason(map.get("reason").and_then(Value::as_str));
            return TenantResolution::Blocked {
                reason,
                host: normalized_host.to_string(),
            };
        }
        let tenant_kind = match kind {
            "subdomain" => TenantKind::Subdomain,
            "custom_domain" => TenantKind::CustomDomain,
            _ => return TenantResolution::Unresolved,
        };
        // A tenant-kind answer that is not verified is not a tenant:
        // fail closed as a blocked (unverified) host instead of
        // serving tenant data on an unverified claim.
        if map.get("verified").and_then(Value::as_bool) != Some(true) {
            return TenantResolution::Blocked {
                reason: BlockedReason::UnverifiedDomain,
                host: normalized_host.to_string(),
            };
        }
        let tenant_id = map
            .get("tenant_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        let company_id = map
            .get("company_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        let site_id = map.get("site_id").and_then(Value::as_str).unwrap_or("");
        if !is_valid_id(tenant_id) || !is_valid_id(company_id) || !is_valid_id(site_id) {
            return TenantResolution::Unresolved;
        }
        // An answer naming a different host is not an answer for this
        // request.
        if let Some(answer_host) = map.get("host").and_then(Value::as_str) {
            if normalize_host(answer_host).as_deref() != Some(normalized_host) {
                return TenantResolution::Unresolved;
            }
        }
        // Product scope: a product-subdomain resolution carries
        // `scope: "product"` plus the product / package /
        // product-subdomain ids. Only the literal `"product"`
        // scope is kept (anything else is not a product scope,
        // and its product keys are ignored); when the scope IS
        // `"product"`, a present-but-invalid product id drops
        // the whole answer to `Unresolved`, never a
        // half-populated context.
        let scope = match map.get("scope").and_then(Value::as_str) {
            Some("product") => Some("product".to_string()),
            _ => None,
        };
        let (product_id, package_id, product_subdomain_id) = if scope.is_some() {
            let Some(product_id) = optional_id(map, "product_id") else {
                return TenantResolution::Unresolved;
            };
            let Some(package_id) = optional_id(map, "package_id") else {
                return TenantResolution::Unresolved;
            };
            let Some(product_subdomain_id) = optional_id(map, "product_subdomain_id") else {
                return TenantResolution::Unresolved;
            };
            (product_id, package_id, product_subdomain_id)
        } else {
            (None, None, None)
        };
        TenantResolution::Tenant(TenantContext {
            tenant_id: tenant_id.to_string(),
            company_id: company_id.to_string(),
            site_id: site_id.to_string(),
            kind: tenant_kind,
            host: normalized_host.to_string(),
            scope,
            product_id,
            package_id,
            product_subdomain_id,
        })
    }

    /// Build a short-circuit error response in the standard envelope.
    /// `tenant` attaches the configured tenant header when a tenant
    /// was resolved (observability); blocked hosts never carry it.
    fn short_circuit(
        &self,
        req: ServiceRequest,
        error: UnifiedError,
        debug: bool,
        tenant: Option<&TenantContext>,
    ) -> MiddlewareResult {
        let mut response = HttpResponse::build(
            actix_web::http::StatusCode::from_u16(error.code)
                .unwrap_or(actix_web::http::StatusCode::FORBIDDEN),
        )
        .json(json::error_json(&error, debug));
        if let Some(tenant) = tenant {
            if let Some(value) = self.tenant_header_value(tenant) {
                response.headers_mut().insert(self.header_name.clone(), value);
            }
        }
        Box::pin(async move { Ok(req.into_response(response)) })
    }

    /// Answer an unresolved host under the configured policy: pass
    /// through as the control plane (explicit operator opt-in only -
    /// logged as a warning every time, so a fail-open deployment is
    /// always visible), or 404 in the standard envelope (the
    /// fail-closed default). `host` is the normalized host when one
    /// exists; a missing or malformed host is never echoed back (its
    /// raw value is untrusted input), only its `reason` is reported.
    fn unresolved(
        &self,
        req: ServiceRequest,
        next: NextFn,
        policy: UnresolvedHostPolicy,
        host: Option<String>,
        reason: &'static str,
        debug: bool,
    ) -> MiddlewareResult {
        match policy {
            UnresolvedHostPolicy::ControlPlane => {
                // Explicit operator opt-in: the request is served as
                // the control plane even though its host did not
                // resolve. Log it - a fail-open pass-through must
                // never happen silently.
                log::warn!(
                    "Unresolved host ({reason}) served as control plane: explicit ControlPlane opt-in is active"
                );
                Box::pin(async move { next(req).await })
            }
            UnresolvedHostPolicy::NotFound => {
                let (message, host_value) = match &host {
                    Some(host) => (
                        format!("Unknown host \"{host}\""),
                        Value::String(host.clone()),
                    ),
                    None => (
                        "Request host could not be resolved".to_string(),
                        Value::Null,
                    ),
                };
                let error = UnifiedError::not_found(message)
                    .with_metadata("reason", Value::String(reason.to_string()))
                    .with_metadata("host", host_value);
                self.short_circuit(req, error, debug, None)
            }
        }
    }
}

impl Middleware for TenantResolutionMiddleware {
    fn priority(&self) -> isize {
        950
    }

    fn handle(&self, req: ServiceRequest, next: NextFn) -> MiddlewareResult {
        let path = req.path().to_string();
        let debug = self.ctx.is_debug();
        // Exempt paths bypass resolution entirely: a health probe or
        // an ACME challenge must answer on any host, resolved or not.
        if self.is_exempt_path(&path) {
            return Box::pin(async move { next(req).await });
        }
        let raw_host = req
            .headers()
            .get(HOST)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|host| !host.is_empty())
            .map(str::to_string);
        let Some(raw_host) = raw_host else {
            // Missing Host header: never panic, never guess a tenant.
            // The fallback is the configured policy, not a hard-coded
            // branch.
            return self.unresolved(
                req,
                next,
                self.config.missing_host_policy,
                None,
                "missing_host",
                debug,
            );
        };
        let Some(normalized_host) = normalize_host(&raw_host) else {
            // Malformed Host header: same policy as an unknown host.
            return self.unresolved(
                req,
                next,
                self.config.unknown_host_policy,
                None,
                "invalid_host",
                debug,
            );
        };
        // Cheap control-plane path: when the host equals the
        // configured main host, skip the hook call entirely.
        if let Some(main_host) = &self.config.main_host {
            if normalized_host.as_str() == main_host.as_str() {
                return Box::pin(async move { next(req).await });
            }
        }
        match self.resolve(&raw_host, &normalized_host) {
            TenantResolution::ControlPlane => Box::pin(async move { next(req).await }),
            TenantResolution::Unresolved => self.unresolved(
                req,
                next,
                self.config.unknown_host_policy,
                Some(normalized_host),
                "unknown_host",
                debug,
            ),
            TenantResolution::Blocked { reason, host } => {
                log::warn!("Tenant host {host} blocked ({})", reason.as_str());
                let message = match reason {
                    BlockedReason::Suspended => "Tenant is suspended",
                    BlockedReason::UnverifiedDomain => "Domain ownership is not verified",
                    BlockedReason::Released => {
                        "This product subdomain was released; the product is no longer served on this host"
                    }
                };
                let mut error = UnifiedError::forbidden(message)
                    .with_metadata("reason", Value::String(reason.as_str().to_string()))
                    .with_metadata("host", Value::String(host));
                if reason == BlockedReason::Released {
                    // Mirror the control-plane-blocked pattern: tell
                    // the caller where to go instead. The full
                    // export-notice page is a follow-up for the
                    // product/release flow; the middleware only
                    // signals the state.
                    let location_hint = self
                        .config
                        .main_host
                        .clone()
                        .map(Value::String)
                        .unwrap_or(Value::Null);
                    error = error.with_metadata("location_hint", location_hint);
                }
                self.short_circuit(req, error, debug, None)
            }
            TenantResolution::Tenant(tenant) => {
                if self.is_blocked_path(&path) {
                    // Tenant hosts serve services only: control-plane
                    // paths are blocked, the block is audit-logged via
                    // the hook (fire-and-forget: the request does not
                    // fail when no handler answers), and the caller is
                    // told where administration lives.
                    let location_hint = self
                        .config
                        .main_host
                        .clone()
                        .map(Value::String)
                        .unwrap_or(Value::Null);
                    self.ctx.hooks.emit(
                        EVENT_TENANT_CONTROL_PLANE_BLOCKED,
                        serde_json::json!({
                            "host": tenant.host,
                            "path": path,
                            "tenant_id": tenant.tenant_id,
                            "company_id": tenant.company_id,
                            "site_id": tenant.site_id,
                            "location_hint": location_hint,
                        }),
                    );
                    log::warn!(
                        "Control-plane path {path} blocked on tenant host {}",
                        tenant.host
                    );
                    let mut error = UnifiedError::forbidden(
                        "Administration lives on the main site; tenant hosts serve services only",
                    )
                    .with_metadata("reason", Value::String("control_plane_blocked".to_string()))
                    .with_metadata("path", Value::String(path));
                    if let Some(main_host) = &self.config.main_host {
                        error = error.with_metadata(
                            "location_hint",
                            Value::String(main_host.clone()),
                        );
                    }
                    return self.short_circuit(req, error, debug, Some(&tenant));
                }
                let header_name = self.header_name.clone();
                let header_value = self.tenant_header_value(&tenant);
                let mut req = req;
                req.extensions_mut().insert(tenant);
                Box::pin(async move {
                    let mut res = next(req).await?;
                    if let Some(value) = header_value {
                        res.headers_mut().insert(header_name, value);
                    }
                    Ok(res)
                })
            }
        }
    }
}
