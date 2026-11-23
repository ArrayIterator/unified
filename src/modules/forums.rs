// Forums module — public-facing discussion forums with SEO
// slugs, categories, threads with solved/best-answer marking,
// two-level nested replies with quotes and mentions, polls,
// thread/forum subscriptions with notifications, voting with
// reputation and a leaderboard, report-based moderation with an
// audited action log and new-user spam controls, and search
// indexing.
//
// Routes live under `/api/v1/forums`:
//
//   Public (no auth; see the public-access note below):
//     GET  /public/forums — public forums.
//     GET  /public/forums/{slug} — one public forum.
//     GET  /public/forums/{slug}/threads — threads of a public forum.
//     GET  /public/threads/{id} — one public thread with its posts.
//     GET  /public/search — substring search over public content.
//   Forums:
//     GET/POST /forums — list forums / create a forum.
//     GET/PATCH/DELETE /forums/{id}.
//     POST /forums/{id}/subscribe, /forums/{id}/unsubscribe.
//   Categories:
//     GET/POST /forums/{forum_id}/categories — list / create.
//     GET/PATCH/DELETE /categories/{id}.
//   Threads:
//     GET/POST /forums/{forum_id}/threads — list / create (creating
//       a thread also creates its opening post from `body`).
//     GET/PATCH/DELETE /threads/{id}.
//     POST /threads/{id}/pin, /threads/{id}/lock,
//       /threads/{id}/solve (best-answer post),
//       /threads/{id}/view (record a view),
//       /threads/{id}/report,
//       /threads/{id}/subscribe, /threads/{id}/unsubscribe.
//     GET/POST /threads/{id}/poll — the thread's poll (one per
//       thread).
//   Posts and replies:
//     GET/POST /threads/{id}/posts — list posts of a thread / reply
//       to the thread (a reply names `parent_post_id` to answer one
//       post; replies nest two levels deep: depth 0, 1, 2).
//     GET/PATCH/DELETE /posts/{id}.
//     POST /posts/{id}/vote — up (+1) or down (-1); one vote per
//       user per post, changing a vote updates it.
//     GET  /posts/{id}/votes — vote totals plus the caller's vote.
//     POST /posts/{id}/report — report a post with a reason.
//   Polls:
//     GET  /polls/{id} — a poll with options, vote counts, and the
//       caller's vote.
//     POST /polls/{id}/vote — one vote per user per poll.
//     POST /polls/{id}/close — poll creator or moderator.
//   Subscriptions:
//     GET  /subscriptions — the caller's own subscriptions.
//   Reports and moderation audit:
//     GET  /reports — open reports first (moderators).
//     POST /reports/{id}/review — dismiss, hide, or delete the
//       target (moderators, audited).
//     GET  /moderation-actions — audited moderator actions.
//   Settings:
//     GET  /settings — rate limits, spam threshold, new-user
//       restrictions.
//     PATCH /settings (moderators).
//   Search and reputation:
//     GET  /search — substring search over threads and posts.
//     GET  /reputation/{user_id} — reputation totals of one user.
//     GET  /reputation/leaderboard — top users by score
//       (?limit=, default 20, max 100).
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory forum, category, thread, post, vote, report,
// subscription, poll, moderation-action, reputation, and
// post-edit-history management; one vote per user per post
// enforced by upsert and one vote per user per poll enforced by
// rejection; edit history kept on every post edit (the previous
// body is stored, never silently overwritten); per-user hourly
// rate limits for threads and posts (defaults 10 threads / 30
// posts, configurable); a spam-report threshold that auto-hides a
// reported post pending review (default 5 open reports,
// configurable) — reported threads are queued for moderator
// review without auto-action; visibility rules (public / members-only /
// staff-only forums, staff-only content answers 404 to anyone
// without the manage capability); two-level nested replies with
// quotes, mentions, and attachment metadata; solved threads with
// a best-answer post; view counts; polls with per-option vote
// counts; thread/forum subscriptions that notify subscribers
// through `notification.requested`; reputation recomputed from
// stored votes and best answers (10 per upvote received, -2 per
// downvote received, 15 per best answer) with a leaderboard; and
// a report review flow (dismiss / hide / delete) that records an
// audited moderation action. What is NOT built here: durable
// storage until the migration runs (the DashMap stores are
// authoritative; migration `2026-11-22-000110_forums` is the
// persistence target); badge awarding (gamification owns badges —
// this module only tracks badge counts via the
// `gamification.badge.awarded` listener and emits forums events
// so gamification can award points); email / push delivery (the
// notifications module owns delivery — this module only emits
// `notification.requested`); full-text search (this module emits
// `search.index` and searches by substring; ranking lives in the
// search module); and live file uploads (attachments are metadata
// references to documents/storage ids only, never file bytes).
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
// `GET /api/v1/forums/public/*`. The routes are registered in
// `route_info()` with `route_version_support()` defaulting to
// `All`, so the API Manager's per-version registry picks them up.
// Do NOT treat the absence of a capability check on a public route
// as an open door: without the rule the API Manager denies the
// call.
//
// Actor identity: the caller is the server-verified caller from
// `unified_core::caller::require_caller` (user id plus
// server-resolved role, both required for capability checks),
// exactly like the events module. Capability checks use the
// `role.check` hook and fail closed (no answer means not allowed).
//
// Events emitted on the hook bus:
//   - `forums.forum.created`, `forums.forum.updated`,
//     `forums.forum.deleted`
//   - `forums.category.created`, `forums.category.updated`,
//     `forums.category.deleted`
//   - `forums.thread.created`, `forums.thread.updated`,
//     `forums.thread.deleted`, `forums.thread.pinned`,
//     `forums.thread.locked`, `forums.thread.solved`
//   - `forums.post.created`, `forums.post.replied`,
//     `forums.post.updated`, `forums.post.deleted`
//   - `forums.vote.cast`
//   - `forums.post.reported` (emitted for both post and thread
//     reports; there is no separate `forums.thread.reported`)
//   - `forums.spam.check_requested` (new posts/replies; a
//     listener may mark the payload spam, otherwise built-in
//     rules apply)
//   - `forums.poll.created`, `forums.poll.voted`,
//     `forums.poll.closed`
//   - `forums.subscription.created`,
//     `forums.subscription.deleted`
//   - `forums.report.reviewed`
//   - `search.index` (threads and posts, for the search module)
//   - `notification.requested` (new posts for subscribers, in the
//     notifications module's payload shape).
// Events this module listens to:
//   - `user.deleted` — anonymizes the user's threads, posts, and
//     post edits to `deleted-user`; deletes their subscriptions,
//     votes, poll votes, and reputation row.
//   - `gamification.badge.awarded` — increments the user's badge
//     count (badges themselves are owned by gamification; this
//     module never awards a badge).
//   - `search.reindex.requested` — republishes every stored thread
//     and post through `search.index` so the search index is
//     rebuilt from the forums stores.
//
// HOW IT DIFFERS FROM COMMUNITY. The community module owns the
// private/members community space: a member directory,
// announcements, one-level-only replies, and topics without
// public SEO. The forums module owns the public-facing product:
// SEO slugs and public read routes, two-level nested replies,
// quotes, mentions, and attachment metadata, solved/best-answer
// marking, view counts, polls, subscriptions with notifications,
// reputation with a leaderboard, new-user link limits, and search
// indexing. Community has no member directory equivalent here,
// no announcements, and no one-level-only reply limit — forums
// has no member directory and no announcements. The two modules
// never share rows: separate stores, separate routes, separate
// events.

pub mod ai_capabilities;
mod handlers_forums;
mod handlers_moderation;
mod handlers_polls;
mod handlers_posts;
mod handlers_subscriptions;
mod handlers_threads;
mod helpers;
mod hooks;
mod module;
pub mod state;
pub mod types;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::module::ForumsModule;
pub use self::module::register;
pub use self::state::ForumState;
pub use self::types::CreateCategoryRequest;
pub use self::types::CreateForumRequest;
pub use self::types::CreatePollRequest;
pub use self::types::CreatePostRequest;
pub use self::types::CreateThreadRequest;
pub use self::types::Forum;
pub use self::types::ForumCategory;
pub use self::types::ForumModerationAction;
pub use self::types::ForumPoll;
pub use self::types::ForumPollOption;
pub use self::types::ForumPollVote;
pub use self::types::ForumPost;
pub use self::types::ForumPostEdit;
pub use self::types::ForumReport;
pub use self::types::ForumReputation;
pub use self::types::ForumSettings;
pub use self::types::ForumSubscription;
pub use self::types::ForumThread;
pub use self::types::ForumVote;
pub use self::types::ModerationReasonRequest;
pub use self::types::ReportRequest;
pub use self::types::ReviewReportRequest;
pub use self::types::SolveThreadRequest;
pub use self::types::UpdateCategoryRequest;
pub use self::types::UpdateForumRequest;
pub use self::types::UpdatePostRequest;
pub use self::types::UpdateSettingsRequest;
pub use self::types::UpdateThreadRequest;
pub use self::types::VotePollRequest;
pub use self::types::VoteRequest;
