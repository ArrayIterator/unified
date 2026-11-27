// Announcements and Changelog module.
//
// Announcements (draft / published / archived, with audience,
// channels, scheduled publish time, expiry, and pinning) and
// changelog entries (version, date, category, body, link).
//
// Audience rules (enforced server-side, see `audience_matches`):
// an announcement is addressed to `all`, `users`, `admins`, one
// `role:<id>`, one `department:<name>`, one `segment:<name>`, or an
// explicit list of users (`user` + `audience_user_ids`). The public
// list, the unread list and the dashboard feed only ever return
// announcements addressed to the calling user, whose identity is
// resolved from the authenticated identity headers (`X-User-Id`,
// `X-Role`, `X-Department`, `X-Segment`) - never from query
// parameters. A caller without identity headers sees broadcast
// (`all`) announcements only. `users` means every user (there is no
// user-directory hook to distinguish users from admins, so it
// collapses to broadcast); `admins` means callers whose role is
// `admin`.
//
// Access control: admin endpoints require the caller identity headers
// (`X-User-Id` + `X-Role`) and the `announcements.view` (reads) or
// `announcements.manage` (writes, publish) capability through the
// `role.check` hook, failing closed. The public list and the changelog
// reads are PUBLIC (no auth; gated by the API Manager's `ApiAccessRule`
// for those paths). Marking read and the unread/dashboard feeds only
// ever act for the authenticated caller.
//
// Publishing and scheduling: `POST /{id}/publish` (or setting the
// status to `published`) only fans the announcement out when its
// publish time has arrived. A future `publish_at` leaves the
// announcement scheduled (`published_at` stays empty) until
// `POST /announcements/publish-due` -- or the
// `announcement.publish_due` hook, which the scheduler can drive --
// publishes everything due. An announcement whose expiry passed
// before its publish time never goes out.
//
// Fan-out: on publish the module emits `announcement.published` and
// a `notification.requested` (kind `announcement`) carrying the full
// audience, the channels and the optional GrapeJS email-template id,
// so the Notifications module can fan it out into the notification
// centre and, when the email channel is selected, send it by email
// (recipient addresses come from `audience_emails`, aligned with
// `audience_user_ids`, or from the registered audience members in
// the Notifications module). The event name was verified against
// `src/modules/notifications.rs`, which registers its handler on
// exactly `notification.requested`.
//
// Read tracking: one receipt per (announcement, user). Marking read
// is idempotent, and only ever records a receipt for the authenticated
// caller. `GET /api/v1/announcements/unread` lists the published,
// currently visible announcements the authenticated caller has not
// read yet. `GET /api/v1/announcements/dashboard` is the
// user-dashboard feed: the same audience-filtered list plus the
// unread count, for the authenticated caller.
//
// Events emitted on the hook bus:
//   - `announcement.published` - an announcement was published.
//   - `changelog.created`      - a changelog entry was created.
//   - `notification.requested` - emitted on publish (kind
//     `announcement`) so the Notifications module can fan the
//     announcement out to its audience.

pub mod ai_capabilities;
pub mod types;
mod validation;
pub mod state;
mod handlers_announcements;
mod handlers_changelog;
mod module;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::AUDIENCE_ALL;
pub use self::types::AUDIENCE_USERS;
pub use self::types::AUDIENCE_ADMINS;
pub use self::types::AUDIENCE_ROLE;
pub use self::types::AUDIENCE_DEPARTMENT;
pub use self::types::AUDIENCE_SEGMENT;
pub use self::types::AUDIENCE_USER;
pub use self::types::ANNOUNCEMENT_CHANNELS;
pub use self::types::STATUS_DRAFT;
pub use self::types::STATUS_PUBLISHED;
pub use self::types::STATUS_ARCHIVED;
pub use self::types::CATEGORY_FEATURE;
pub use self::types::CATEGORY_FIX;
pub use self::types::CATEGORY_SECURITY;
pub use self::types::Announcement;
pub use self::types::ChangelogEntry;
pub use self::types::ReadReceipt;
pub use self::types::CreateAnnouncementRequest;
pub use self::types::UpdateAnnouncementRequest;
pub use self::types::MarkReadRequest;
pub use self::types::CreateChangelogRequest;
pub use self::types::UpdateChangelogRequest;
pub use self::state::AnnouncementsState;
pub use self::module::AnnouncementsModule;
pub use self::module::register;
