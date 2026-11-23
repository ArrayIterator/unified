// Notifications module.
//
// In-app notifications with read tracking and per-user channel,
// category and digest preferences. Notification templates render
// per-event titles and bodies with `{{placeholders}}` and carry the
// channel selection (email via an email-provider connection, in-app,
// webhook) on the template itself. Other modules request a
// notification through the `notification.requested` hook; email
// delivery goes through the Email Providers module
// (`email.send_requested`, falling back to the Mail Manager's
// `mail.send_requested` when no provider handler answers). The centre
// also listens directly to the failure and lifecycle events of the
// other modules (orders, payments, backups, clusters, tickets,
// licenses, incidents, webhooks, security) so alerts land here.
// Interconnection happens only through the hook bus.
//
// Audience rules (enforced server-side, see `resolve_recipients`):
// a notification is stored once per resolved recipient. Broadcast
// audiences (`all`, `users`) are stored once with `user_id` "all" and
// are visible to every user in list / unread-count queries. Role,
// department and segment audiences resolve ONLY through the audience
// registry (`POST /api/v1/notifications/audiences`); an unregistered
// audience is never delivered to anyone -- the request fails with an
// `audience unresolved` result and a `notification.audience.unresolved`
// event instead of silently dropping or leaking the notification.
// There is no user-directory hook in this codebase (the Users module
// exposes no audience-resolution event), so membership must be
// registered explicitly; that registry is the source of truth here.
//
// Delivery tracking: every channel dispatch records a
// `DeliveryAttempt` (queued / sent / delivered / failed, with the
// provider job id when one is returned). The providers report the
// outcome back through `email.sent` / `email.failed` (Email Providers)
// and `mail.sent` / `mail.failed` (Mail Manager); those listeners
// update the attempt. A failed delivery is never silent: the attempt
// keeps the error, `notification.delivery.failed` is emitted, and the
// attempt can be retried (see `RETRY_DELAYS_SECS` for the policy).
//
// Routes (all under `/api/v1/notifications`):
//   GET    /                              - paginated notifications
//   POST   /                              - create a notification
//   GET    /preferences                   - per-user preferences
//   PUT    /preferences                   - update per-user preferences
//   GET    /unread-count                  - unread/total counts for a user
//   POST   /read-all                      - mark all of a user's notifications read
//   POST   /digest/send                   - send a digest email for a user
//   GET    /audiences                     - registered audiences (paginated)
//   POST   /audiences                     - register an audience
//   GET    /audiences/{audience}          - one registered audience
//   DELETE /audiences/{audience}          - delete a registered audience
//   GET    /deliveries                    - delivery attempts (paginated)
//   GET    /{id}/deliveries               - delivery attempts for one notification
//   POST   /{id}/deliveries/{attempt_id}/retry - retry a failed delivery
//   GET    /templates                     - paginated notification templates
//   POST   /templates                     - create a notification template
//   GET    /templates/{id}                - one notification template
//   PUT    /templates/{id}                - update a notification template
//   DELETE /templates/{id}                - delete a notification template
//   POST   /templates/{id}/preview        - render a template with variables
//   POST   /templates/{id}/test           - render and dispatch a template once
//   GET    /{id}                          - one notification
//   DELETE /{id}                          - delete a notification
//   POST   /{id}/read                     - mark one notification read

pub mod ai_capabilities;
pub mod types;
pub mod state;
mod helpers;
mod handlers;
mod dispatch;
mod events;
pub mod module;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::*;
pub use self::state::*;
pub use self::helpers::*;
pub use self::handlers::*;
pub use self::dispatch::*;
pub use self::events::*;
pub use self::module::*;
