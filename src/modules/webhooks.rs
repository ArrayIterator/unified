// Webhooks Manager — outgoing webhook endpoints and deliveries.
//
// Routes under `/api/v1/webhooks`:
//
//   GET    /events              catalog of subscribable event names
//   GET    /endpoints           list endpoints (paginated, filterable)
//   POST   /endpoints           create an endpoint subscription
//   GET    /endpoints/{id}      get one endpoint
//   PUT    /endpoints/{id}      update an endpoint
//   DELETE /endpoints/{id}      delete an endpoint
//   POST   /endpoints/{id}/pause    pause deliveries to an endpoint
//   POST   /endpoints/{id}/resume   resume deliveries to an endpoint
//   POST   /endpoints/{id}/test     send a `webhook.ping` test delivery
//   GET    /deliveries          list deliveries (paginated, filterable)
//   GET    /deliveries/{id}     get one delivery
//   POST   /deliveries/{id}/redeliver  retry a delivery with its stored payload
//
// Delivery model: the module registers a hook handler for every known
// event name. When an event fires, every active, unpaused endpoint
// subscribed to it gets a delivery record, and a background task posts
// the payload. `notification.webhook` (emitted by the notifications
// module when the webhook channel is selected) has its own handler:
// it fans out the same way, and when no endpoint subscribes to it the
// miss is recorded as a `webhook.failed` event instead of being
// silently dropped. Each delivery is signed with HMAC-SHA256 over
// `{unix_timestamp}.{body}`; the signature travels in
// `X-Unified-Signature: sha256=<hex>` with `X-Unified-Timestamp`.
// Transient failures (connection errors, timeouts, HTTP 408 / 429 /
// 5xx) are retried with exponential backoff up to `max_attempts`;
// other 4xx responses fail the delivery immediately. Redirects are
// never followed, so a redirect cannot smuggle a delivery to an
// internal address.
//
// URL safety: only https is accepted, except http to a loopback host
// for local development. URLs with embedded credentials are rejected.
// IP-literal hosts that are private, link-local, or unspecified are
// rejected. At delivery time the host is resolved once and every
// resolved address is validated; the request is then pinned to those
// validated addresses through a DNS override, keeping the original
// host in the URL for the Host header and TLS SNI. The client never
// performs a second lookup, so a DNS change between validation and
// connect cannot steer the delivery at an internal address
// (DNS-rebinding TOCTOU). The endpoint secret is stored but never
// serialized: API responses only report `secret_configured`.
//
// Every route requires the caller identity (`X-User-Id` + `X-Role`
// headers) and a capability checked through the `role.check` hook,
// failing closed when no handler answers: `webhooks.view` for reads,
// `webhooks.manage` for writes.
//
// Hooks emitted: `webhook.delivered`, `webhook.failed`,
// `webhook.endpoint.created` (plus `webhook.endpoint.updated`,
// `webhook.endpoint.deleted`, `webhook.endpoint.paused`).

// Split into one file per concern under `src/modules/webhooks/`.
// This root only declares the submodules and re-exports the
// public API at its original paths.

pub mod constants;
pub mod store;
pub mod types;
pub mod signing;
pub mod validation;
pub mod delivery;
pub mod requests;
pub mod handlers;
pub mod helpers;
pub mod module;

pub use self::constants::{webhook_events, KNOWN_EVENTS};
pub(crate) use self::constants::{HmacSha256, DEFAULT_MAX_ATTEMPTS, MAX_MAX_ATTEMPTS, DEFAULT_BACKOFF_BASE_SECS, MAX_BACKOFF_BASE_SECS, MAX_BACKOFF_SECS, DEFAULT_TIMEOUT_SECS, MAX_TIMEOUT_SECS, USER_AGENT, PERM_WEBHOOKS_VIEW, PERM_WEBHOOKS_MANAGE};
pub(crate) use self::store::{HOOK_BUS, ENDPOINTS, DELIVERIES, endpoints_store, deliveries_store, emit};
pub use self::types::{WebhookEndpoint, WebhookEndpointView, DeliveryStatus, WebhookDelivery};
pub use self::signing::{sign_payload, verify_signature};
pub(crate) use self::signing::{payload_hash};
pub use self::validation::{validate_webhook_url};
pub(crate) use self::validation::{is_loopback, is_forbidden_ip, resolve_validated_addresses, validate_event_names};
pub use self::delivery::{dispatch_event};
pub(crate) use self::delivery::{build_client, build_client_resolved, backoff_delay, is_retryable_status, run_delivery, set_delivery_attempt, set_delivery_retrying, finish_delivery, mark_failed, enqueue_delivery};
pub(crate) use self::helpers::{
    Caller, caller, caller_company, check_capability, require_caller_company, require_permission,
    require_tenant,
};
pub(crate) use self::requests::{CreateEndpointRequest, default_true, default_max_attempts, default_backoff_base_secs, default_timeout_secs, UpdateEndpointRequest, paginate, find_endpoint, validate_retry_policy};
pub(crate) use self::handlers::{list_events, create_endpoint, list_endpoints, get_endpoint, update_endpoint, delete_endpoint, set_paused, pause_endpoint, resume_endpoint, test_endpoint, list_deliveries, get_delivery, redeliver};
pub use self::module::{WebhooksModule, register};
