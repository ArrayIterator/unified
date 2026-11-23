# C3 Pilot 5 — Worker Group B Results

Tenant edge guards for **esg, exams, forums, inbox** (tracker task 319).
Guard idiom follows pilots 1–4: `require_tenant(&req)?` first in every
tenant-data handler, then the verified-caller company scope — the
`X-Company-Id` header is never trusted and there is no fallback to
`public`. No new dependencies, no push. `docs/integration/C3_MODULE_GUARD_MAP.md`
was NOT edited (the coordinator merges rows).

## Counts

| Module | Routed | Guarded | Excluded | Missing | Scope-checked | Gate-only |
| --- | --- | --- | --- | --- | --- | --- |
| esg | 33 | 33 | 0 | 0 | 0 | 33 |
| exams | 58 | 58 | 0 | 0 | 56 | 2 |
| forums | 51 | 51 | 0 | 0 | 0 | 51 |
| inbox | 18 | 18 | 0 | 0 | 18 | 0 |
| **Total** | **160** | **160** | **0** | **0** | **74** | **86** |

Routed counts were verified against the `.to(handler)` registrations in
each module's `module.rs` (the guard map lists these four modules only
as coordinator-named unguarded, with no per-module rows, so there is
no map count to reconcile against). Guarded counts were verified by
`docs/integration/pilot5/c3_group_b_verify.py` (see Verification).

## Exclusions — none

All 160 routed handlers are guarded; nothing was excluded.

- **forums public handlers are gated, not excluded.**
  `list_public_forums`, `get_public_forum`,
  `list_public_forum_threads`, `get_public_thread`, `public_search`
  all take `HttpRequest` and already called `resolve_tenant(&req)?`
  (failing closed without a tenant context); adding the
  `require_tenant` gate changes no behaviour. They stay public in
  the visibility sense (a `Public` forum needs no capability beyond
  the tenant gate); the `MembersOnly` / `StaffOnly` visibility rules
  in `helpers.rs` are unchanged.
- **exams has no public surface.** Every routed handler already
  required a verified caller.
- **esg has no public surface.** The AI summary capability
  (`ai_capability`) is an authenticated, capability-gated read.
- **inbox has no public surface.** Ingestion (`events.rs`,
  `upsert_thread`) is not routed and is untouched.

## Per-module detail

### esg — 33 guarded / 0 excluded / 0 missing

Guard helper in `helpers.rs`: `require_tenant` (new; the existing
`resolve_tenant` returning the tenant-id string is kept and still
supplies the `tenant_id` every handler and loader already uses).
Records carry `tenant_id: String` and **no `company_id`**; entries
and targets carry a generic `scope_type` / `scope_id` reference
(a company or a site id), which this module does not resolve
against the caller scope. The guard is therefore gate-only, the
pilot-4 group-C model.

- Gate inserted in all 33 handlers (frameworks 5, metrics 5,
  periods 7, entries 6, targets 5, reports 4, AI 1), as
  `let _tenant = require_tenant(&req)?;` immediately before the
  existing `let tenant_id = resolve_tenant(&req)?;` line (direct
  style in `ai_capability`: a match block returning the module's
  error expression, before the first `caller(&req)`).
- Tenant partitioning itself was already enforced and is unchanged:
  lists read through `state.*_of(&tenant_id)`, single-record loads
  go through the `load_*` helpers, which 404 (never 403) on a
  cross-tenant row.
- `require_tenant` was added to every handler file's explicit
  `use super::helpers::{...}` import (13 files across esg/forums).
- Tests: `tenant_guard_tests` in `helpers.rs` — fail-closed without
  context, resolves from middleware context. **UNRUN** (see
  Verification).

### exams — 58 guarded / 0 excluded / 0 missing

Guard helpers in `helpers.rs` (String-shaped company ids, the
pilot-3 elearning model): `require_tenant` (pre-existing),
`caller_company`, `require_company_match`,
`require_company_match_for`, `in_company_scope`,
`require_record_in_company_scope`, plus parent-scope loaders
`bank_in_scope` / `require_bank_by_id_in_scope`,
`exam_in_scope` / `require_exam_by_id_in_scope`,
`require_question_in_scope` (via bank),
`require_attempt_in_scope` (via exam),
`require_rubric_in_scope` (via exam),
`require_grade_in_scope` (via attempt → exam),
`require_result_in_scope` (via exam). Only `QuestionBank` and
`Exam` carry `company_id: Option<String>`; questions scope through
their bank, exam questions / attempts / rubrics / grades / results
scope through their exam.

- The tenant gate was already present in all 58 handlers
  (`let _tenant = require_tenant(&req)?;` first); this task added
  the company-scope layer: `let _scope =
  caller_company(&_tenant, &req)?;` after the caller line in every
  handler.
- List filters: `list_banks`, `list_exams` (`in_company_scope`,
  keeping the existing `company_id` query filter, which ANDs with
  scope), `list_attempts`, `list_my_attempts`, `list_grading_queue`,
  `list_my_results` (`exam_in_scope` on the parent exam).
- Create body verified: `create_bank` / `create_exam` via
  `require_company_match` (previously the body company was stored
  verbatim). `create_exam` / `update_exam` also scope-check every
  named `bank_id` — a bank from another company can no longer be
  linked to an exam.
- Load + scope check after every bank / exam / question / attempt /
  rubric / grade / result load (get/update/delete, transitions,
  attach/detach, grading, results, analytics). `change_exam_status`
  (the shared gate for `publish_exam` / `archive_exam` /
  `close_exam`, like elearning's `transition_course`) now takes the
  scope and checks the exam; its one test call was updated to pass
  `&None`. `duplicate_exam` scope-checks the source; the copy keeps
  the source company.
- Ownership is unchanged and still enforced after the scope check:
  attempts and results additionally require manager/grader or
  `user_id == caller.actor` (`visible_attempt`, `result_visible`,
  inline ownership checks on heartbeat/autosave/submit/proctor).
- AI handlers: `ai_draft_questions` scope-checks its `bank_id`
  (was existence-only) and `ai_analyse_results` scope-checks its
  exam; both keep the AI-funnel company resolution
  (`ai::resolve_caller_company`, verified-caller-first since task
  317). The stale comment in `ai_draft_questions` claiming the AI
  helper resolves "header first, then the user hook" was corrected.
- Gate-only (documented, no company field on the record):
  `get_settings`, `update_settings`.
- No `require_company_change_for` helper: neither
  `UpdateBankInput` nor `UpdateExamInput` carries a company field,
  so there is no company change to verify on update.
- Tests: `tenant_guard_tests` in `helpers.rs` — fail-closed,
  resolves, verified-caller preference, mismatch rejection, header
  ignored, scope, match-for, bank/exam/question/attempt/rubric/
  grade/result scope rules (own/foreign/global/missing).
  **UNRUN** (see Verification).

### forums — 51 guarded / 0 excluded / 0 missing

Guard helper in `helpers.rs`: `require_tenant` (new; `resolve_tenant`
kept as before). Records carry `tenant_id: String` and **no
`company_id`**. The guard is therefore gate-only, the pilot-4
group-C model.

- Gate inserted in all 51 handlers (forums 15 including the 5 public
  reads, categories inside `handlers_forums.rs`, threads 11, posts 8,
  moderation 7, polls 5, subscriptions 5), as
  `let _tenant = require_tenant(&req)?;` immediately before the
  existing `let tenant_id = resolve_tenant(&req)?;` line (including
  the multi-line closure in `vote_poll`).
- Tenant partitioning itself was already enforced and is unchanged:
  lists filter by `tenant_id`, single-record loads go through the
  `load_*` helpers, which 404 (never 403) on a cross-tenant row;
  votes, reputation, and subscriptions are tenant-scoped the same
  way.
- Tests: `tenant_guard_tests` in `helpers.rs` — fail-closed without
  context, resolves from middleware context. **UNRUN** (see
  Verification).

### inbox — 18 guarded / 0 excluded / 0 missing

**No code changes were needed.** All 18 handlers already called
`require_tenant(&req)?` FIRST (before `caller(&req)?`), and every
read filters or loads through `tenant_visible`, every create stamps
the scope company:

- `require_tenant` (`auth.rs:51`) already enforces the
  verified-caller/tenant company match (403 on mismatch, 400 on a
  malformed verified company, header never read) — strictly stronger
  than the `caller_company` used in the other three modules, so no
  redundant `caller_company` calls were added.
- Lists (`list_threads`, `list_canned_replies`, `queue_stats`)
  filter with `tenant_visible`; single-record loads go through
  `load_thread` / `load_canned_reply` (`service.rs`), which 404 on
  an out-of-scope row; `get_thread` also scopes its assignments via
  `assignments_for` (also `tenant_visible`-filtered);
  `merge_thread` loads both threads through `load_thread`;
  `request_reply` loads its canned reply through
  `load_canned_reply`; creates (`register_thread`,
  `create_canned_reply`, `assign_thread`) stamp
  `company_id: scope.company_id.clone()`; `register_thread`'s
  duplicate check is tenant-partitioned via
  `find_thread_for_source`.
- This task verified the 18 handlers (guard + order + scope
  markers) with the group-B verifier and left the code untouched,
  except for flagging the open item below.

## Write strictness

| Module | Variant | Meaning |
| --- | --- | --- |
| esg | tenant-gate-only | `require_tenant` only. No company field on any record; `scope_type`/`scope_id` references are not resolved against the caller scope. |
| exams | mismatch-only | Reads: `caller_company` + scope filter/load check (unresolved callers see global rows only). Writes: `caller_company` + `require_company_match` on creates (omitted body company adopts the caller scope; a body company from an unresolved caller is refused) + parent-scope load checks. No bare `require_company` on mutations — an unresolved caller may still write a global row, exactly like pilot-3 elearning. Updates carry no company field, so no change-verify step exists. |
| forums | tenant-gate-only | `require_tenant` only. No company field on any record; per-forum `Public`/`MembersOnly`/`StaffOnly` visibility is a separate read rule, unchanged. |
| inbox | tenant-gate + `tenant_visible` | `require_tenant` (with verified-caller/tenant company-match enforcement) + `tenant_visible` on every read/load/create. See the open item below. |

Harmonization across these variants is task 315, NOT this task —
the variants are recorded here as found/built, not reconciled.

## Verifier adaptations

`docs/integration/pilot5/c3_group_b_verify.py` (copied from pilot-4
`c3_group_a_verify.py`, same routed-set rule, guard, and order
check) adapts the kind check per module, documented in its
docstring:

- esg, forums: no kind check (gate-only model, pilot-4 group C).
- exams: GET → `caller_company(&_tenant, &req)`; mutations →
  `caller_company` AND a scope application (a
  `require_*_in_scope` load, `require_company_match`,
  `in_company_scope` / `exam_in_scope` filter, or the shared
  `change_exam_status` gate). The pilot-4 group-A strict rule
  (mutations must call bare `require_company`) is NOT applied:
  exams follows the pilot-3 elearning mismatch-only shape, and
  `update_settings` is the one documented gate-only mutation
  (settings carry no company field).
- inbox: every handler must apply `tenant_visible` directly,
  through a scope-checked loader (`load_thread`,
  `load_canned_reply`, `assignments_for`), or by stamping
  `scope.company_id` on create.

## Files touched

- `src/modules/exams/helpers.rs` (guard section +
  `tenant_guard_tests`), `handlers_banks.rs`,
  `handlers_questions.rs`, `handlers_exams.rs`,
  `handlers_attempts.rs`, `handlers_grading.rs`,
  `handlers_results.rs`, `handlers_settings.rs`, `handlers_ai.rs`
- `src/modules/esg/helpers.rs` (`require_tenant` +
  `tenant_guard_tests`), `handlers_ai.rs`, `handlers_entries.rs`,
  `handlers_frameworks.rs`, `handlers_metrics.rs`,
  `handlers_periods.rs`, `handlers_reports.rs`, `handlers_targets.rs`
- `src/modules/forums/helpers.rs` (`require_tenant` +
  `tenant_guard_tests`), `handlers_forums.rs`,
  `handlers_moderation.rs`, `handlers_polls.rs`, `handlers_posts.rs`,
  `handlers_subscriptions.rs`, `handlers_threads.rs`
- `src/modules/inbox/*` — NOT touched (already guarded; verified
  only — see above)
- `docs/integration/pilot5/c3_group_b_verify.py` (new)
- `docs/integration/pilot5/group-b-results.md` (this file)
- `docs/integration/C3_MODULE_GUARD_MAP.md` NOT touched (coordinator
  merges rows).

## Verification

- Parse check: `rustfmt --edition 2024 --emit stdout` on all 25
  touched Rust files (exams 9, esg 8, forums 8) — **all parse
  clean** (rustfmt invoked with `PATH="$HOME/.cargo/bin:$PATH"`;
  `--emit stdout` tests parsing, not style). No whole-file
  formatting was applied (pre-existing style is not rustfmt-clean
  in these files, same as the pilot-3/4 note).
- Guard verification (`c3_group_b_verify.py`, re-run after the final
  edits):

  ```
  esg: guarded=33 (reads=15, writes=18) missing=0 excluded=0 order_violations=0 kind_mismatches=0 double_guards=0
  exams: guarded=58 (reads=23, writes=35) missing=0 excluded=0 order_violations=0 kind_mismatches=0 double_guards=0
  forums: guarded=51 (reads=23, writes=28) missing=0 excluded=0 order_violations=0 kind_mismatches=0 double_guards=0
  inbox: guarded=18 (reads=5, writes=13) missing=0 excluded=0 order_violations=0 kind_mismatches=0 double_guards=0
  ```

  Verifier sanity: run with an excluded name it reports
  `excluded=1` + `double_guards=1` (excluded but guarded) and exits
  1, as designed; the pre-edit inventory (same body-extraction
  approach) showed esg/forums at 0 gated, so the guard detection is
  not vacuous.
- Header scan: no production `X-Company-Id` read exists in any of
  the four modules. The only mentions are doc comments (already
  accurate post-task-317), the exams AI-helper comment fixed here,
  and test code that inserts a forged header to assert it is
  ignored.
- **Root-bin files were NOT typechecked and the guards are UNRUN.**
  `cargo check -p unified` and `cargo test` on the root bin were not
  run (bin crate ~5.7 GiB RSS, OOM-killed on this VM). All new unit
  tests (`tenant_guard_tests` in exams/esg/forums helpers) are
  unrun; one existing test call was updated for the new
  `change_exam_status` signature (`handlers_exams.rs`, `&None`
  scope).

## Residuals and follow-ups

- **OPEN — inbox `tenant_visible` (`src/modules/inbox/auth.rs:94`).**
  `(record_company, scope_company)` pair: `_ => true` when either
  side lacks a company, so a company-less scope sees any company's
  threads within the tenant (the tenant id must still match
  exactly). This is the known task-312 item, still open after pilot
  3: changing it alters documented inbox behaviour and needs a
  PRODUCT decision, not a silent fix. It was NOT decided and NOT
  changed here; every inbox handler around it is guarded as far as
  possible without changing that behaviour. Owner: product decision.
- **exams in-memory stores are not partitioned by `tenant_id`.**
  Pre-existing (documented on `require_tenant` in
  `helpers.rs`): records carry no `tenant_id` yet, so the gate
  rejects unresolved tenants but the DashMap stores themselves are
  not yet per-tenant — open for the integrator (same note as
  before this task).
- **exams global rows stay writable by unresolved callers.**
  Mismatch-only consequence, same as elearning: an unresolved
  caller creating a bank/exam without a body company stores a
  global row. Requiring a resolved company on every write is a
  task-315 decision.
- **esg `scope_type` / `scope_id` are not scope-checked.**
  Entries and targets name a scope reference (e.g. a company or
  site id) that is validated as a plain id only; resolving it
  against the caller scope is a C11 follow-up if company
  partitioning of entries is wanted.
- **forums has no company field at all.** Per-company forum
  partitioning would need a company scope on the record (C11
  follow-up); the gate plus the tenant-matching loaders and the
  per-forum visibility rules are the current boundary.
- **inbox ingestion is out of the routed set.** `events.rs` /
  `upsert_thread` (service) are not HTTP handlers and were not
  re-gated; they stamp the tenant/company from the event payload.
