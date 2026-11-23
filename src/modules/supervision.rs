// Project Supervision module -- site inspections, progress
// reporting, defect tracking, approvals, daily logs, and photo
// evidence for projects owned by the Project Management module.
//
// This module does NOT own projects, tasks, or milestones and
// never duplicates their rows. Every project, task, and milestone
// is referenced by its opaque string id only, and project
// existence / progress is resolved through hook queries answered
// by the projects module (see "Projects-module contract" below).
// The projects module is the single source of truth for project
// data; this module stores only supervision data about projects.
//
// Routes live under `/api/v1/supervision`:
//
//   Assignments:
//     GET/POST /assignments -- list (filter by project_id,
//       user_id, role, status) / assign a supervisor.
//     GET/PATCH/DELETE /assignments/{id}.
//   Inspection templates:
//     GET/POST /templates -- list (filter by project_id,
//       category) / create a template with checklist items.
//     GET/PATCH/DELETE /templates/{id}.
//   Inspections:
//     GET/POST /inspections -- list (filter by project_id,
//       template_id, status, inspector_id, overdue) / create
//       (optionally from a template; template items are copied
//       into the inspection as pending results).
//     GET/PATCH /inspections/{id}.
//     POST /inspections/{id}/submit -- inspector submits the
//       inspection for approval.
//     POST /inspections/{id}/complete -- record the overall
//       verdict and the inspector signature.
//     POST /inspections/{id}/approve, /return -- approval chain
//       decision (records an approval row).
//     POST /inspections/{id}/items/{item_id}/result -- record
//       pass / fail / na plus notes for one checklist item.
//     POST /inspections/{id}/items/{item_id}/follow-up -- ask
//       the projects module (via event) to create a follow-up
//       task for a failed item.
//   Progress reports:
//     GET/POST /progress-reports -- list (filter by project_id,
//       milestone_id, status, period) / create a report. The
//       report stores a snapshot: the reported percent, the
//       planned percent, and the project percent read through
//       the hook query at creation time (plus its source), never
//       a second copy of project progress treated as truth.
//     GET/PATCH /progress-reports/{id}.
//     POST /progress-reports/{id}/submit -- draft -> submitted.
//     POST /progress-reports/{id}/approve -- submitted -> approved
//       (records an approval row).
//     POST /progress-reports/{id}/return -- submitted -> returned
//       (records an approval row with a comment).
//   Defects:
//     GET/POST /defects -- list (filter by project_id,
//       inspection_id, severity, status, task_id) / log a defect.
//     GET/PATCH /defects/{id}.
//     POST /defects/{id}/follow-up -- ask the projects module
//       (via event) to create a follow-up task for the defect.
//     POST /defects/{id}/verify -- resolved -> verified.
//   Approvals:
//     GET /approvals -- list approval rows (filter by
//       target_kind, target_id, approver_id, decision).
//   Daily logs:
//     GET/POST /daily-logs -- list (filter by project_id, date,
//       supervisor_id, signed) / create a site daily log.
//     GET/PATCH /daily-logs/{id}.
//     POST /daily-logs/{id}/sign-off -- supervisor sign-off.
//   Photo evidence:
//     GET/POST /photos -- list (filter by subject_kind,
//       subject_id, project_id) / attach a photo reference.
//       Metadata only: storage target + key + SHA-256 hash +
//       caption. No image bytes live in this module.
//     GET/DELETE /photos/{id}.
//   Dashboard:
//     GET /dashboard?project_id=... -- per-project supervision
//       summary (open defects, overdue inspections, progress
//       variance) computed on read.
//
// HONESTY -- READ THIS FIRST. What is real in this module:
// in-memory supervision assignments, inspection templates,
// inspections with per-item results, progress-report snapshots
// with an approval workflow, a defect log with a status machine,
// approval rows with a full decision trail, daily logs with
// sign-off, photo metadata referencing the storage module, and a
// dashboard computed on read from this module's own tables plus
// project hook queries. What is NOT built here: projects, tasks,
// milestones, or sprints (the projects module owns them -- this
// module only links by id); follow-up task creation itself (this
// module only emits `project.task.create_requested` and records
// that it asked -- the projects module must answer it, see the
// contract below); project progress as a live value (the report
// stores the percent the hook query returned at snapshot time
// plus its source, and the dashboard re-queries on read); file
// upload or image bytes (the storage module owns bytes -- this
// module stores references only); and actual email / push
// delivery (the notifications module owns delivery -- this module
// only emits `notification.requested`).
//
// Projects-module contract (wired into the projects module,
// which answers all four queries below; until the wiring
// existed the calls failed closed):
//   - `project.get_requested` with `{"project_id": "<id>"}` is
//     answered with a result
//     `{"exists": true, "project": {"id", "name", "status",
//     "archived"}}` or `{"exists": false}`. Until it existed,
//     every endpoint that takes a project id returned an
//     internal error instead of inventing a project -- fail
//     closed, never guess.
//   - `project.progress_requested` with
//     `{"project_id": "<id>", "milestone_id": "<id>" | null}` must
//     be answered with a result
//     `{"percent_complete": <0..100>, "milestone_id": ..., "milestone_reached": <bool>}`
//     computed from the projects module's own tasks / milestones
//     (the projects module has no stored percent field on
//     `Project`; it must derive it, for example from completed
//     vs total tasks). When unanswered, progress reports record
//     `project_percent_complete: null` and `source:
//     "unverified"` instead of a fabricated number.
//   - `project.task.create_requested` with
//     `{"project_id", "title", "description", "source":
//     "supervision", "inspection_id" | null, "defect_id" | null,
//     "requested_by"}` must be answered with a result
//     `{"created": true, "task_id": "<id>"}` (or
//     `{"created": false, "error": "..."}`). Until answered, the
//     follow-up endpoints record `follow_up_requested: true` and
//     `follow_up_task_id: null` and report that no task was
//     created yet -- they never claim a task exists.
//   - `project.task.get_requested` with `{"task_id": "<id>"}` must
//     be answered with `{"exists": true, "task": {...}}` or
//     `{"exists": false}`; used to validate defect task links.
// Events this module listens to (all acknowledged; the projects
// module emits the full serialized project / task JSON):
//   - `project.created`, `project.updated` -- acknowledged only;
//     this module keeps no project cache.
//   - `project.archived` -- acknowledged; active assignments for
//     that project are suspended (status `suspended`).
//   - `project.task.created`, `project.task.assigned`,
//     `project.task.completed` -- acknowledged only; a completed
//     task never auto-resolves a defect (resolution needs a human
//     verification step through `POST /defects/{id}/verify`).
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the events module. Capability checks use
// the `role.check` hook and fail closed (no answer means not
// allowed).

pub mod ai_capabilities;
pub mod types;
pub mod state;
mod helpers;
mod handlers_assignments;
mod handlers_inspections;
mod handlers_reports;
mod handlers_defects;
mod handlers_logs;
mod events;
pub mod module;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::*;
pub use self::state::*;
pub use self::helpers::*;
pub use self::handlers_assignments::*;
pub use self::handlers_inspections::*;
pub use self::handlers_reports::*;
pub use self::handlers_defects::*;
pub use self::handlers_logs::*;
pub use self::events::*;
pub use self::module::*;
