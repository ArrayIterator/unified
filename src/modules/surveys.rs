// Surveys / NPS / CSAT module - survey builder with versions,
// distribution triggers, tokenized responses, NPS + CSAT scoring
// with trends, and low-score follow-ups through the tasks module.
//
// Routes live under `/api/v1/surveys`:
//
//   Surveys:
//     GET/POST / - list surveys / create a survey (draft, version 1).
//     GET/PATCH/DELETE /{id} - get one survey; PATCH on a draft
//       edits in place, PATCH on a published survey creates a new
//       version (draft) and returns it; DELETE removes a draft only.
//     POST /{id}/publish - publish a draft version (the previously
//       published version of the same family is archived).
//     POST /{id}/archive.
//   Questions:
//     GET/POST /{id}/questions - list / add a question (draft
//       surveys only).
//     GET/PATCH/DELETE /{id}/questions/{question_id} (draft only).
//   Results and responses:
//     GET  /{id}/results - NPS + CSAT scores, per-question stats,
//       breakdowns by agent / department / product, and trends
//       (?bucket=daily|weekly, ?from, ?to, ?agent_id,
//       ?department_id, ?product_id).
//     GET  /{id}/responses - paginated, filter by status
//       (submitted / pending), score band, agent_id,
//       department_id, product_id.
//     GET  /responses/{response_id} - one response with answers.
//   Distributions:
//     GET/POST /{id}/distributions - list / create a distribution
//       (trigger: manual, ticket_closed, purchase, scheduled).
//     GET  /distributions/{id}.
//     POST /distributions/{id}/send - send the invitations now
//       (manual recipient list, or re-send pending ones).
//     DELETE /distributions/{id} - delete a pending distribution.
//   Public response (no auth; see the public-access note below):
//     GET  /public/{token} - the survey + questions for one
//       response token.
//     POST /public/{token} - submit the answers for one token.
//
// HONESTY - READ THIS FIRST. What is real in this module:
// in-memory survey versions, questions, distributions, responses,
// and answers; versioning (editing a published survey creates a
// new draft version in the same family; publishing archives the
// previous published version; every response references the exact
// version row it answered); tokenized response links (32 random
// bytes, base64url, one response per recipient per survey version,
// token expiry); NPS computed on read as %promoters minus
// %detractors and CSAT as % satisfied (score 4-5 of 5), with
// daily / weekly trend buckets and breakdowns by agent /
// department / product; trigger listeners for `ticket.updated`
// (a ticket moving to `resolved` / `closed` invites its user to
// the surveys with an active `ticket_closed` distribution) and
// `store.order.completed` / `order.paid` (a completed order
// invites its buyer to the surveys with an active `purchase`
// distribution); scheduled distributions queued through
// `scheduler.enqueue`; low-score follow-up as a
// `task.create_requested` event for the tasks module plus a
// `notification.requested` to the responsible user when one is
// configured. What is NOT built here: actual delivery of survey
// invitations (the module creates the tokenized response rows and
// emits `notification.requested` per recipient - the notifications
// module owns delivery, exactly like the events module's reminder
// path); a handler answering `task.create_requested` (no such
// listener exists in `tasks.rs` today, so a follow-up stays
// `requested` with no task id until the tasks module - or an
// integrator - answers that event; the follow-up link is still
// stored on the response and `surveys.followup.created` is still
// emitted); and a public survey directory (public routes answer
// only for a valid, unexpired token).
//
// Public-access gate: the API Manager module enforces public access
// through its admin-managed `ApiAccessRule` records (default deny:
// an endpoint is public only when a rule exists with
// `public_access: true`). There is no reusable per-request
// "is this endpoint public?" helper to call from a handler - the
// check lives in the API Manager middleware / `resolve_public`
// path - so the public routes below rely on that gate: an
// administrator must create the matching `ApiAccessRule` rows for
// `GET /api/v1/surveys/public/{token}` and
// `POST /api/v1/surveys/public/{token}`. The routes are registered
// in `route_info()` with `route_version_support()` defaulting to
// `All`, so the API Manager's per-version registry picks them up.
// Do NOT treat the absence of a capability check on a public route
// as an open door: without the rule the API Manager denies the
// call. A response token is unguessable (32 random bytes) and
// expires, so a leaked token alone cannot enumerate surveys.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the events module. Capability checks use
// the `role.check` hook and fail closed (no answer means not
// allowed).
//
// Events emitted on the hook bus:
//   - `surveys.created`, `surveys.updated`, `surveys.published`,
//     `surveys.archived`, `surveys.version_created`,
//     `surveys.deleted`
//   - `surveys.question.created`, `surveys.question.updated`,
//     `surveys.question.deleted`
//   - `surveys.distribution.created`, `surveys.distribution.sent`
//   - `surveys.response.created` (invitation issued),
//     `surveys.response.received`
//   - `surveys.followup.created` (low-score follow-up requested)
//   - `task.create_requested` (follow-up task request for the
//     tasks module: `{"source": "surveys", "title", "body",
//     "assignee_id", "department_id", "survey_id",
//     "response_id"}`; the answering handler contributes a
//     `task_id` / `id` result, like `ticket.create_requested`)
//   - `notification.requested` (survey invitations and low-score
//     follow-up notices, in the notifications module's payload
//     shape)
//   - `scheduler.enqueue` with `{"kind": "surveys.send",
//     "payload": {"survey_id", "distribution_id", ...}}` when a
//     scheduled distribution is created.
// Events this module listens to:
//   - `ticket.updated` - a ticket moving to `resolved` or `closed`
//     invites its user to every survey with an active
//     `ticket_closed` distribution (department filter respected).
//   - `store.order.completed`, `order.paid` - a completed order
//     invites its buyer to every survey with an active `purchase`
//     distribution (product filter respected when the order
//     payload names products).
//   - `surveys.send_requested` - the event the scheduler emits
//     when a `surveys.send` job fires, following its naming rule
//     (`backup.run` -> `backup.run_requested`); sends the pending
//     invitations of the named distribution.
//
// Scheduler note: `surveys.send` is NOT in the scheduler's
// `KNOWN_KINDS` list yet, so today the scheduler answers
// `{"queued": false, "error": "Unknown job kind: ..."}` and the
// distribution records `scheduler_status: "declined"` instead of
// pretending it was scheduled. The distribution is still stored
// and its invitations can be sent manually through
// `POST /distributions/{id}/send`; the follow-up is to add
// `"surveys.send"` to `KNOWN_KINDS` in `scheduler.rs` and arm the
// dispatch so the job emits `surveys.send_requested` with the
// distribution payload (see the integrator snippet in the module
// report).

pub mod ai_capabilities;
mod constants;
pub mod types;
pub mod requests;
pub mod state;
mod helpers;
mod service;
mod hooks;
pub mod module;
mod handlers_surveys;
mod handlers_questions;
mod handlers_results;
mod handlers_distributions;
mod handlers_public;

// Public API (unchanged): re-exported from the split files.
// From types.rs:
pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::Survey;
pub use self::types::SurveyAnswer;
pub use self::types::SurveyDistribution;
pub use self::types::SurveyQuestion;
pub use self::types::SurveyResponse;
// From requests.rs:
pub use self::requests::CreateDistributionRequest;
pub use self::requests::CreateQuestionRequest;
pub use self::requests::CreateSurveyRequest;
pub use self::requests::DistributionRecipient;
pub use self::requests::SendDistributionRequest;
pub use self::requests::SubmitAnswer;
pub use self::requests::SubmitResponseRequest;
pub use self::requests::UpdateQuestionRequest;
pub use self::requests::UpdateSurveyRequest;
// From state.rs:
pub use self::state::SurveysState;
// From module.rs:
pub use self::module::SurveysModule;
pub use self::module::register;
