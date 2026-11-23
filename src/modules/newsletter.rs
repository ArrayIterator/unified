// Newsletter module.
//
// Lists, subscribers with double opt-in, and campaigns built from the
// existing GrapeJS email-template builder (templates are referenced by
// id; the builder itself is never re-implemented here). Campaigns send
// through the Email Providers module, one explicit provider connection
// per campaign (or the list's default connection), and report
// per-campaign stats. Open and click tracking are first-class: a
// tracking pixel and rewritten links emit `newsletter.open` and
// `newsletter.click` on the hook bus.
//
// Routes (all under `/api/v1/newsletter`):
//   Lists:
//     GET    /lists                       - paginated lists
//     POST   /lists                       - create a list
//     GET    /lists/{id}                  - one list
//     PUT    /lists/{id}                  - update a list
//     DELETE /lists/{id}                  - delete a list (and its subscribers)
//     GET    /lists/{id}/subscribers      - paginated subscribers of one list
//     POST   /lists/{id}/subscribe        - subscribe an email address
//   Subscribers:
//     GET    /subscribers/{id}            - one subscriber
//     PUT    /subscribers/{id}            - update a subscriber
//     DELETE /subscribers/{id}            - delete a subscriber
//     GET    /subscribers/confirm?token=  - confirm a double opt-in token
//     POST   /subscribers/confirm         - confirm a double opt-in token
//     POST   /subscribers/{id}/resend-confirm - resend the confirmation email
//     POST   /subscribers/{id}/unsubscribe - unsubscribe a subscriber
//     GET    /unsubscribe?token=          - unsubscribe through a signed link
//   Campaigns:
//     GET    /campaigns                   - paginated campaigns
//     POST   /campaigns                   - create a campaign
//     GET    /campaigns/{id}              - one campaign
//     PUT    /campaigns/{id}              - update a draft campaign
//     DELETE /campaigns/{id}              - delete a campaign
//     POST   /campaigns/{id}/schedule     - schedule a campaign
//     POST   /campaigns/{id}/send-now     - send a campaign immediately
//     POST   /campaigns/{id}/test-send    - send a test copy to one address
//     GET    /campaigns/{id}/stats        - campaign stats with rates
//   Tracking:
//     GET    /track/open/{campaign_id}/{subscriber_id}  - 1x1 tracking pixel
//     GET    /track/click/{campaign_id}/{subscriber_id}?url=... - tracked redirect
//   Webhooks:
//     POST   /webhooks/{provider}         - provider delivery/bounce/complaint webhook
//   GET    /stats                         - overall newsletter stats
//
// Hook contracts:
//   Emits `newsletter.subscribed`, `newsletter.confirmed`,
//   `newsletter.unsubscribed`, `newsletter.campaign.sent`,
//   `newsletter.campaign.completed`, `newsletter.open`,
//   `newsletter.click`.
//   Sends through `email.send_requested` (Email Providers module) and
//   renders templates through `template.render_requested` (Email
//   Templates module). Scheduling emits `scheduler.enqueue` with kind
//   `newsletter.send`; the scheduler must know that kind (see the
//   coordinator report for the snippet). The module also answers
//   `newsletter.campaign.send` itself, which is the execution entry
//   point the scheduler calls back through.
//
// Notes on honesty:
//   - Subscriber, list, and campaign records are kept in memory (same
//     pattern as the other modules in this tree); durable persistence
//     is a follow-up for the integrator (migration sketch in the
//     coordinator report).
//   - Confirmation and unsubscribe tokens are HMAC-signed with a
//     per-process secret, so they stop verifying after a restart.
//   - A campaign is `completed` once its send phase finished (every
//     confirmed subscriber was queued with the provider); delivery,
//     bounce, and complaint confirmations arrive later through the
//     provider webhooks and update the stats.

pub mod ai_capabilities;
pub mod constants;
pub mod types;
pub mod state;
mod helpers;
mod sending;
mod lists;
mod subscribers;
mod campaigns;
mod tracking;
mod webhooks;
pub mod routes;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::constants::EVENT_NEWSLETTER_SUBSCRIBED;
pub use self::constants::EVENT_NEWSLETTER_CONFIRMED;
pub use self::constants::EVENT_NEWSLETTER_UNSUBSCRIBED;
pub use self::constants::EVENT_NEWSLETTER_CAMPAIGN_SENT;
pub use self::constants::EVENT_NEWSLETTER_CAMPAIGN_COMPLETED;
pub use self::constants::EVENT_NEWSLETTER_OPEN;
pub use self::constants::EVENT_NEWSLETTER_CLICK;
pub use self::constants::EVENT_NEWSLETTER_CAMPAIGN_SEND;
pub use self::constants::EVENT_EMAIL_SEND_REQUESTED;
pub use self::constants::EVENT_TEMPLATE_RENDER_REQUESTED;
pub use self::constants::EVENT_EMAIL_PROVIDER_WEBHOOK_VERIFY;
pub use self::constants::EVENT_SCHEDULER_ENQUEUE;
pub use self::types::NewsletterList;
pub use self::types::Subscriber;
pub use self::types::CampaignStats;
pub use self::types::CampaignStatsView;
pub use self::types::Campaign;
pub use self::types::TrackingEvent;
pub use self::state::NewsletterState;
pub use self::routes::NewsletterModule;
pub use self::routes::register;
