// Community / Forum module — discussion categories, topics, posts with
// one-level nested replies, votes, flags and moderation, a member
// directory, announcements, and badge counts fed by the gamification
// module.
//
// Routes live under `/api/v1/community`:
//
//   Categories:
//     GET/POST /categories — list categories / create a category.
//     GET/PATCH/DELETE /categories/{id}.
//   Topics:
//     GET/POST /topics — list topics / create a topic.
//     GET/PATCH/DELETE /topics/{id}.
//     POST /topics/{id}/pin, /topics/{id}/lock.
//   Posts and replies:
//     GET/POST /topics/{id}/posts — list posts of a topic / reply
//       to the topic (a reply names `parent_post_id` to answer one
//       post; replies are one level deep only).
//     GET/PATCH/DELETE /posts/{id}.
//     POST /posts/{id}/vote — up (+1) or down (-1); one vote per
//       user per post, changing a vote updates it.
//     GET  /posts/{id}/votes — vote totals plus the caller's vote.
//     POST /posts/{id}/flag — flag a post with a reason.
//     POST /posts/{id}/hide, /posts/{id}/unhide (moderators).
//   Flags and moderation audit:
//     GET  /flags — open flags first (moderators).
//     POST /flags/{id}/review — dismiss a flag or hide the post.
//     GET  /moderation-actions — audited moderator actions.
//   Members:
//     GET  /members — member directory (display name, join date,
//       badge counts; never emails or private data).
//     POST /members — join the community (one row per user).
//     GET  /members/{user_id} — one member.
//   Announcements:
//     GET/POST /announcements — list / create (draft until
//       published).
//     GET/PATCH/DELETE /announcements/{id}.
//     POST /announcements/{id}/publish — publish + notify members.
//   Settings:
//     GET  /settings — rate limits and spam threshold.
//     PATCH /settings (moderators).
//   Public (no auth; see the public-access note below):
//     GET  /public/topics — topics in public categories.
//     GET  /public/topics/{id} — one public topic with its posts.
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory category, topic, post, vote, flag, announcement,
// member, moderation-action, and post-edit-history management;
// one vote per user per post enforced by upsert; edit history kept
// on every post edit (the previous body is stored, never silently
// overwritten); per-user hourly rate limits for topics and posts
// (defaults 10 topics / 30 posts, configurable); a spam-flag
// threshold that auto-hides a post pending review (default 5
// flags); visibility rules (public / members-only / staff-only
// categories, staff-only content answers 404 to anyone without the
// manage capability); and announcement publishing that notifies
// every member through `notification.requested`. What is NOT built
// here: badge definitions and badge awarding (the gamification
// module owns badges — this module only listens to
// `gamification.badge.awarded` to keep member badge counts, and
// emits community events so gamification can award points for
// community help); actual email / push delivery (the notifications
// module owns delivery — this module only emits
// `notification.requested`); full-text search (list endpoints
// filter by exact fields and substring-match title/body only);
// and durable storage (the DashMap stores are authoritative;
// migration `2026-11-22-000056_community` is the persistence
// target).
//
// Public-access gate: the API Manager module enforces public access
// through its admin-managed `ApiAccessRule` records (default deny:
// an endpoint is public only when a rule exists with
// `public_access: true`, and denied outright when a rule exists
// with `allowed: false`). There is no reusable per-request
// "is this endpoint public?" helper to call from a handler — the
// check lives in the API Manager middleware / `resolve_public`
// path — so the public routes below rely on that gate: an
// administrator must create the matching `ApiAccessRule` rows for
// `GET /api/v1/community/public/*`. The routes are registered in
// `route_info()` with `route_version_support()` defaulting to
// `All`, so the API Manager's per-version registry picks them up.
// Do NOT treat the absence of a capability check on a public route
// as an open door: without the rule the API Manager denies the
// call.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the events module. Capability checks use
// the `role.check` hook and fail closed (no answer means not allowed).
//
// Events emitted on the hook bus:
//   - `community.category.created`, `community.category.updated`,
//     `community.category.deleted`
//   - `community.topic.created`, `community.topic.updated`,
//     `community.topic.deleted`, `community.topic.pinned`,
//     `community.topic.locked`
//   - `community.post.created`, `community.post.replied`,
//     `community.post.updated`, `community.post.deleted`,
//     `community.post.hidden`, `community.post.unhidden`
//   - `community.vote.cast`
//   - `community.post.flagged`
//   - `community.flag.reviewed`
//   - `community.announcement.created`,
//     `community.announcement.published`
//   - `community.member.joined`
//   - `notification.requested` (announcement published, in the
//     notifications module's payload shape).
// Events this module listens to:
//   - `gamification.badge.awarded` — increments the member's
//     badge count (badges themselves are owned by gamification;
//     this module never awards a badge).

pub mod ai_capabilities;
pub mod types;
pub mod state;
mod helpers;
mod hooks;
mod handlers_categories;
mod handlers_posts;
mod handlers_members;
mod module;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::CategoryVisibility;
pub use self::types::CommunityCategory;
pub use self::types::CommunityTopic;
pub use self::types::CommunityPost;
pub use self::types::CommunityPostEdit;
pub use self::types::CommunityVote;
pub use self::types::CommunityFlag;
pub use self::types::CommunityModerationAction;
pub use self::types::CommunityMember;
pub use self::types::CommunityAnnouncement;
pub use self::types::CommunitySettings;
pub use self::types::CreateCategoryRequest;
pub use self::types::UpdateCategoryRequest;
pub use self::types::CreateTopicRequest;
pub use self::types::UpdateTopicRequest;
pub use self::types::CreatePostRequest;
pub use self::types::UpdatePostRequest;
pub use self::types::VoteRequest;
pub use self::types::FlagRequest;
pub use self::types::ReviewFlagRequest;
pub use self::types::JoinRequest;
pub use self::types::CreateAnnouncementRequest;
pub use self::types::UpdateAnnouncementRequest;
pub use self::types::UpdateSettingsRequest;
pub use self::types::ModerationReasonRequest;
pub use self::state::CommunityState;
pub use self::module::CommunityModule;
pub use self::module::register;
