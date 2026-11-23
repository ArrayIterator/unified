# C3 Pilot 3 — Worker Group B Results

Tenant edge guards for **elearning, knowledgebase, properties, events**
(tracker task 314). Guard idiom follows pilots 1–2: `require_tenant(&req)?`
first in every tenant-data handler, then `caller_company` resolved from
the verified `Caller` only — the `X-Company-Id` header is never trusted
and there is no fallback to `public`. Company callers read own + global
records only; unresolved callers read global only; writes require a
resolved company. Create bodies verified with `require_company_match`
(omitted body company adopts the caller scope); update bodies with
`require_company_change_for` (an explicit clear stays a clear).

## Counts

| Module | Routed | Guarded | Excluded | Missing |
| --- | --- | --- | --- | --- |
| elearning | 58 | 57 | 1 | 0 |
| knowledgebase | 17 | 17 | 0 | 0 |
| properties | 37 | 33 | 4 | 0 |
| events | 37 | 32 | 5 | 0 |
| **Total** | **149** | **139** | **10** | **0** |

Routed counts were verified against the `.to(handler)` registrations in
each module's `module.rs` (not against the guard map, which miscounts
two modules). Guarded counts were verified by counting
`require_tenant` call sites in handler code (test-module calls excluded)
and mapping shared-helper gates to the routed handlers they cover.

## Exclusions

### elearning — 1 excluded

- `verify_certificate` — public verification endpoint (no caller, no
  tenant context). Anyone holding a certificate code may verify it.
  Untouched by design.

### knowledgebase — 0 excluded

All 17 routed handlers are guarded.

### properties — 4 excluded

All four are anonymous public handlers with no `HttpRequest`, so no
tenant context exists to gate on:

- `list_public_properties`
- `get_public_property`
- `get_public_plan_svg`
- `get_public_plan_print`

`public_handlers.rs` is untouched.

### events — 5 excluded

All five are anonymous public handlers with no `HttpRequest`. The
public calendar/ICS surface is gated by the API Manager `ApiAccessRule`
outside this module:

- `list_public_events`
- `get_public_event`
- `public_event_ics`
- `public_calendar_ics`
- `public_calendar_json`

`handlers_public.rs` public handlers are untouched; the authenticated
calendar handlers (`event_ics`, `calendar_ics`, `calendar_json`) in the
same file ARE guarded (see below).

## Per-module detail

### elearning — 57 guarded / 1 excluded / 0 missing

Guard helpers in `helpers.rs` (String-shaped company ids): `require_tenant`,
`caller_company`, `require_company_match`, `require_company_match_for`,
`require_company_change_for`, `in_company_scope`,
`require_record_in_company_scope`, plus `course_in_scope`,
`require_course_by_id_in_scope`, `require_course_in_scope`,
`quiz_course_in_scope`, `path_visible`. Only `Course.company_id`
exists; modules, lessons, quizzes, attempts, certificates, reviews,
and enrollments scope through their parent course.

- Gate inserted in 55 handlers (courses, modules/lessons, enrollments,
  quizzes, certificates, reviews, paths, settings) plus the shared
  `transition_course` helper, which is the single gate for
  `publish_course` / `archive_course`, and `ai_capability` (hand-gated;
  the lesson's course is scope-checked before delegating to the AI
  runner).
- List filters: `list_courses`, `list_enrollments`, `list_my_enrollments`,
  `list_my_progress`, `list_certificates`, `list_quiz_attempts` (via
  `quiz_course_in_scope`), `list_paths` (via `path_visible`).
- Create body verified: `create_course` via `require_company_match`.
- Update body verified: `update_course` via `require_company_change_for`.
- Load + scope check after every course/module/lesson/quiz/question/
  attempt/certificate/review/enrollment/progress load, via the parent
  course. `get_path` retains only in-scope member courses;
  `add_path_course` / `remove_path_course` check course scope;
  `list_my_paths` filters enrolled course ids by scope.
- Gate-only (documented, no company field on the record):
  `get_settings`, `update_settings`, `create_path`, `update_path`,
  `delete_path`, `get_path_progress` (own-user progress data only).
- Tests: `tenant_guard_tests` in `helpers.rs` — fail-closed, resolves,
  verified-caller preference, mismatch rejection, header ignored, scope,
  match-for, change-for (explicit clear stays clear), missing-course
  fail-closed. **UNRUN** (see Verification).

### knowledgebase — 17 guarded / 0 excluded / 0 missing

Guard helpers in `helpers.rs` (Uuid-shaped company ids): `require_tenant`,
`caller_company`, `in_company_scope`, `require_record_in_company_scope`,
`require_article_in_scope`. `Article.company_id: Option<Uuid>`;
`Category` has no company field (gate-only).

- Gate inserted in 16 closure handlers (articles 8, categories 5,
  tickets 3) plus `ai_capability` in `helpers.rs` (hand-gated; grounding
  stays scoped via the AI runner and tenant-filtered hook search).
- Scope edits: `create_article` stamps `company_id: _scope` (no body
  company field); `list_articles` filter; `get_article` /
  `update_article` load + check; `delete_article` loads via
  `require_article_in_scope` before removal; `list_versions` /
  `get_version` / `link_ticket` / `list_article_tickets` /
  `unlink_ticket` via `require_article_in_scope`; `search_endpoint`
  retains hits whose article is in scope.
- **Old `caller_company_id` helper deleted.** It resolved the verified
  caller first, then fell back to the `user.company_requested` hook.
  The guard path no longer uses the hook: unresolved callers read
  global only. Its doc comment in `types.rs` now points at
  `caller_company`. Verified by reading the code that the map's
  task-312 appendix claim is true: the old helper never trusted the
  header in code (its doc comment was stale). Its test
  `caller_company_id_from_header_then_hook_then_none` asserted header
  stamping — an assertion that could not pass against the code
  (pre-existing broken, unrun test); replaced by
  `caller_company_prefers_tenant_then_verified_caller_and_ignores_headers`.
- Tests: `tenant_guard_tests` in `helpers.rs` (fail-closed, resolves,
  verified preference, mismatch, scope, `require_article_in_scope`
  own/foreign/global/unresolved/missing). `handlers_articles.rs` tests
  rewritten: `verified_request` now also attaches a `TenantContext`
  (empty company); new `tenant_context()`,
  `verified_request_with_company()`, `tenantless_request()` builders;
  `create_article_requires_manage_permission` asserts verified-company
  stamping and that a company-less caller creates a global article;
  `reads_require_view_and_a_role_header` replaced by
  `reads_require_tenant_view_and_a_verified_caller` (no tenant → 401;
  tenant but no caller → 401; no view → 403; view → 200);
  `list_articles_hides_other_companies_articles` parses the JSON
  envelope and asserts a company-A caller sees only own + global items
  and gets 403 on company-B's article. **UNRUN** (see Verification).

### properties — 33 guarded / 4 excluded / 0 missing

Guard helpers in `helpers.rs` (Uuid-shaped company ids):
`require_tenant`, `caller_company`, `require_company_match_uuid`,
`require_company_match_uuid_for`, `require_company_change_uuid_for`,
`in_company_scope`, `require_record_in_company_scope`,
`property_in_scope`, `require_property_by_id_in_scope`,
`require_property_in_scope`. `Property.company_id: Option<Uuid>`;
plans, blocks, history, and purchases scope through their parent
property. `LocationSettings` is gate-only.

- Gate inserted in 28 closure handlers (property 8, plan 5, block 13,
  location 2) plus hand-gated match-style handlers: `export_blocks`
  (gate + property scope check inside its load closure),
  `get_plan_svg` / `get_plan_print` (gate + property scope check inside
  the load closure), `search_location` / `reverse_location` (gate-only:
  external geocoder, no tenant data read or stored).
- Scope edits: `list_properties` filter (`in_company_scope`, keeping
  the existing `company_id` query filter which ANDs with scope);
  `create_property` body verified via `require_company_match_uuid`;
  `update_property` load + scope check, company change via
  `require_company_change_uuid_for` (`input.company_id` is
  `Option<Option<Uuid>>`); load + scope check after every
  `load_property` in get/update/delete/publish/archive/mark_sold_out,
  all plan handlers, all block handlers (via parent property);
  `list_blocks` / `list_plans` scope via the parent property load.
- Tests: `tenant_guard_tests` in `helpers.rs` (same pattern as
  knowledgebase/elearning, including the header-ignored test).
  `block_handlers.rs` `verified_request` now also attaches a
  `TenantContext` (empty company), like the knowledgebase fix; seeded
  properties have `company_id: None` (global), so scope-`None` callers
  still see them and existing assertions should hold. **UNRUN**
  (see Verification).

### events — 32 guarded / 5 excluded / 0 missing

Guard helpers in `helpers.rs` (String-shaped company ids):
`require_tenant`, `caller_company`, `require_company_match`,
`require_company_match_for`, `require_company_change_for`,
`in_company_scope`, `require_record_in_company_scope`,
`event_in_scope`, `require_event_by_id_in_scope`,
`require_event_in_scope`. `Event.company_id: Option<String>`;
occurrences, ticket types, registrations, attendees, waitlist entries,
and reminders scope through their parent event.

- Gate inserted in 25 closure handlers (events 8, tickets 5,
  reminders 4, registrations 8) plus the shared `transition_event`
  helper, which is the single gate for `publish_event` /
  `cancel_event` / `complete_event`, plus hand-gated match-style
  handlers: `export_attendees` (gate + event scope check),
  `event_ics` (gate + event scope check), `calendar_ics` and
  `calendar_json` (gate + scope-filtered feed).
- Scope edits: `list_events` filter; `create_event` body verified
  (`input.company_id: Option<String>`, validated via `require_text`
  first, then `require_company_match`); `update_event` via
  `require_company_change_for` (`input.company_id:
  Option<Option<String>>`); load + scope check after every
  `load_event` (get/update/delete/occurrences/ticket types/register/
  list_registrations/list_attendees/export_attendees/list_waitlist/
  reminders); registration handlers load the registration, then scope
  through its event via `require_event_by_id_in_scope`
  (`get_registration`, `cancel_registration`, `request_refund`,
  `check_in_registration` — both the by-code and by-id branches).
- Authenticated calendar feed: new `feed_events_for_scope` and
  `calendar_json_payload_for_scope` in `handlers_public.rs`
  (the public `feed_events(state, public_only)` and
  `calendar_json_payload(state, public_only)` are unchanged for the
  excluded public handlers). `calendar_json_payload` was refactored
  to delegate to `calendar_json_payload_for_feed` so the scoped
  variant reuses the same item mapping.
- Tests: `tenant_guard_tests` in `helpers.rs` — fail-closed, resolves,
  verified-caller preference, mismatch rejection, header ignored, scope,
  match-for, change-for (explicit clear stays clear), missing-event
  fail-closed. **UNRUN** (see Verification). No handler (tokio) tests
  exist in events.

## Files touched

- `src/modules/elearning/helpers.rs`, `courses.rs`,
  `modules_lessons.rs`, `enrollments.rs`, `quizzes.rs`,
  `certificates.rs`, `reviews.rs`, `paths.rs`, `settings_handlers.rs`,
  `ai.rs`
- `src/modules/knowledgebase/helpers.rs`, `types.rs`,
  `handlers_articles.rs`, `handlers_categories.rs`,
  `handlers_tickets.rs`
- `src/modules/properties/helpers.rs`, `property_handlers.rs`,
  `plan_handlers.rs`, `block_handlers.rs`, `print_handlers.rs`,
  `location_handlers.rs` (`public_handlers.rs` untouched)
- `src/modules/events/helpers.rs`, `handlers_events.rs`,
  `handlers_tickets.rs`, `handlers_reminders.rs`,
  `handlers_registrations.rs`, `handlers_public.rs`
- `docs/integration/C3_MODULE_GUARD_MAP.md` NOT touched (coordinator
  merges rows).

## Verification

- Parse check: `rustfmt --edition 2024 --emit stdout` on all 26
  touched files — **all parse clean** (rustfmt invoked with
  `PATH="$HOME/.cargo/bin:$PATH"`; `--emit stdout` tests parsing, not
  style).
- Guard-count verification: routed handler counts taken from the
  `.to(handler)` registrations in each module's `module.rs`
  (elearning 58, knowledgebase 17, properties 37, events 37);
  `require_tenant` call sites counted per file with test-module calls
  excluded (elearning 56 sites covering 57 routed handlers via the
  shared `transition_course` gate; knowledgebase 17; properties 33;
  events 30 sites covering 32 routed handlers via the shared
  `transition_event` gate). Missing = 0 in all four modules.
- **Root-bin files were NOT typechecked and the guards are UNRUN.**
  `cargo check -p unified` and `cargo test` on the root bin were not
  run (bin crate ~5.7 GiB RSS, OOM-killed on this VM). All new and
  adjusted unit tests are unrun; existing handler-test assertions were
  updated only where the gate changes the expected outcome
  (knowledgebase `handlers_articles.rs`, properties
  `block_handlers.rs`).

## Residuals and follow-ups

- **elearning paths have no company field.** Path handlers are
  gate-only; a path is visible iff it has no courses or at least one
  in-scope course (`path_visible`). `get_path_progress` is gate-only
  and may name other companies' course UUIDs in its progress data.
- **knowledgebase `delete_category` global in-use check.** Categories
  have no company field; the delete conflict check ("category in use")
  is global and can leak that another company's article uses the
  category.
- **properties global slug uniqueness leak.** `create_property`'s
  global slug-uniqueness check can leak cross-company existence via
  conflict errors (same pattern as pilot 2 products).
- **events check-in by code.** `check_in_registration` scopes through
  the registration's event after resolving by code or id; a code from
  another company's event is rejected with 403 after the lookup.
- **Header trust still open until task 317.** The guards here never
  read `X-Company-Id`; forged headers are ignored by this code, but
  other tree-wide header-trust sites are task 317's scope.
- **knowledgebase `caller_company_id` removal.** The deleted helper's
  `user.company_requested` hook fallback is no longer used by the guard
  path; any other caller of that hook (none found in this module)
  should be checked by the coordinator.
- **elearning `list_quiz_questions` double scope check** (the shared
  `load_lesson` + `load_quiz_for_lesson` pattern) is redundant but
  harmless.
