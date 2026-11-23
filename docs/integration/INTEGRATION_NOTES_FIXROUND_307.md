# Fix round 307 — root-bin (`unified`) compile errors

Date: 2026-11-23. Worker: fix-round subagent (task 307). Tree:
`/home/hatch/workspace/unified-push`. No push, no tracker edits (coordinator
owns those). No new dependencies, no behaviour changes, no stubs, no
`allow(dead_code)` or similar silencing. All code/docs English only.

## Result

**Exit 0 was NOT observed.** Error count by lean run (all
`cargo check --offline`, lean env `JOBS=1 INCREMENTAL=0
CARGO_TARGET_DIR=~/workspace/unified-target`, sccache):

| Run | Log | Exit | Errors |
| --- | --- | --- | --- |
| Baseline (re-measure) | `~/workspace/unified-check/lean-build-20261123-fixround2.log` | 101 | **502** |
| Verify 1 (after layer 1) | `~/workspace/unified-check/lean-build-20261123-fixround2-verify.log` | 101 | **162** |
| Verify 2 (after layer 2) | `~/workspace/unified-check/lean-build-20261123-fixround2-verify2.log` | 101 | **6** |
| Verify 3 (after layer 3) | `~/workspace/unified-check/lean-build-20261123-fixround2-verify3.log` | 101 | **no diagnostics — OOM-killed** |

`cargo check -p unified-core --offline` after all fixes: **exit 0**
(log `lean-build-20261123-fixround2-core.log`). No lib crate was touched
(all edits are under `src/modules/`), so no lib regression is expected and
none was observed.

## Why the count moved in layers

The 502 baseline errors were almost all name-resolution failures
(E0425/E0432/E0252). While a name is unresolved, rustc types every use of
it as `{error}` and suppresses follow-on type/borrow errors, so fixing
layer 1 unmasked layer 2 (all pre-existing, none introduced by layer 1),
and fixing layer 2 unmasked nothing except 6 errors my own layer-2 edit
introduced (layer 3, see below).

## Layer 1 — 502 -> 162 (root cause: shared-helper wiring)

The tenant helpers (`require_tenant`, `caller_company`,
`require_company_for_write`, `require_record_in_company_scope`,
`require_record_writable_in_company_scope`, `in_company_scope`, plus
module-specific scope helpers) exist in each module's `helpers.rs` /
`validation.rs` / `auth.rs` / `state.rs`, but were missing from the lists
that make them visible to handlers. Two shapes, one fix each:

- Modules whose handlers use `use super::*;` (properties, elearning,
  domain_verification, gamification, timetracker, auditors,
  storage_manager): the missing names were added to the module root's
  `pub(crate) use self::<source>::{...}` re-export list.
- Modules whose handlers import explicitly (`store`, `products`,
  `software`, `hosting`, `tasks`, `delivery_notes`, `portals`, `domains`,
  `workflows`): the missing names were added to the handler files'
  `use super::helpers::{...}` / `super::state::{...}` /
  `super::validation::{...}` lists.

Other layer-1 fixes (all in the same round):

- E0432 wrong-path imports: `resolve_company_id` was re-exported from
  `timetracker::auth`, `gamification::helpers`, `library::auth` but is
  defined nowhere in those modules and used nowhere — removed from all
  three lists (no cross-module users exist). `is_storage_admin` was
  re-exported from `storage_manager::helpers` but was renamed to
  `is_platform_admin` — re-export renamed. `require_company_for_write`
  is defined locally in `domains/handlers.rs` and `workflows/handlers.rs`
  but was also imported from `super::state` — removed from the state
  imports. `mail` had no `require_company_for_write` at all: added to
  `src/modules/mail.rs` beside `caller_company`, mirroring the
  domains/workflows `Option<String>` shape.
- E0252 duplicates: second `use super::state::AdmissionsState;` line
  deleted in 8 admissions files; duplicate `use super::types::{...};`
  line deleted in 7 field-service files; duplicate `check_capability`
  re-export deleted in `documents.rs`.
- Syntax: `src/modules/ai/providers.rs:123,167` — inline `{id}`
  interpolation with unescaped inner quotes; quotes escaped (the
  author's evident intent, smallest diff).
- Brief item 3a: `documents/handlers_files.rs` test helper `test_document`
  now sets the 5 migration-000124 metadata fields (`title`, `alt_text`,
  `caption`, `description`, `custom_metadata`); also added the missing
  `use uuid::Uuid;` there (line ~170) and reordered that file's import
  block to rustfmt-canonical order.
- Brief item 3b: `SEARCH_KINDS` (`search/constants.rs`) now includes
  `"documents.document"`, `"forums.thread"`, `"forums.post"` — the Search
  module was dropping those `search.index` payloads.
- Brief item 3c: `documents.rs` now re-exports the metadata-PATCH handler
  `update_document_metadata` and the `UpdateDocumentMetadataRequest` type
  (module.rs already routed to the handler directly).
- Brief item 3d: the `storage` case is the `storage_manager`
  `is_storage_admin` E0432 above. No broken import was found in the
  `loyalty` module (all its `super::helpers` imports resolve) and no
  error was reported at `tickets/handlers.rs:949` in any run — both were
  inspection findings against an older tree; nothing to fix there now.
- `debug` x30 in `content/{posts,pages,categories}.rs`: 15 handlers used
  bare `debug` in error responses without defining it — added
  `let debug = ctx.is_debug();` at the top of each, matching the sibling
  handlers in the same files.
- Misc singles: `OrderItem` import in `store/state.rs`, `UnifiedResult`
  import in `auth_manager/module.rs`, `backup` `list_jobs` scope
  (returned from the auth closure alongside the caller instead of being
  read outside it), `appointments` `guest_book` scope (see design
  decisions).

Fix script (auditable, every replacement asserted exactly-once):
`~/workspace/unified-check/fixround2.py`; formatting script:
`~/workspace/unified-check/fixround2-fmt.py` (reformatted only the touched
`use` statements / `SEARCH_KINDS` to rustfmt-canonical form).

## Layer 2 — 162 -> 6 (unmasked type/borrow errors)

- 71x E0308 in String-scope `caller_company` fns: the last match arm was
  `(None, verified) => Ok(verified),` where `verified` is `&Option<String>`
  (the match is on references) but the fn returns `Option<String>`.
  Fixed to `Ok(verified.clone())` at exactly the 71 log-flagged lines.
  (The 4 remaining `Ok(verified)` arms are Uuid-scope fns where the value
  is owned — correct as-is.)
- 73x E0277 top-level `?` guards in `async fn -> HttpResponse` handlers
  (departments, email_providers, mail, curriculum, store): converted to
  match-early-return, copying the error-reporting expression the same
  function already uses for its next fallible call. One
  `ok_or_else(...)?` in `store/products.rs` got the same treatment.
- 6 errors in `exams/handlers_banks.rs::get_bank` (1 E0308 + 5 E0277)
  share one cause: the `UnifiedResult<QuestionBank>` closure's tail
  expression unwrapped with `?` — the `?` was dropped so the closure
  returns the `Result` itself.
- Singles: `data_tools` `&cleanup_event_for(target)` (emit takes `&str`);
  `scheduler` `seed_builtin_jobs(&self.state)` (takes `&Arc<..>`);
  `payments/dispatch.rs` `return ...ok_or_else(...);` (drop the `?` —
  rustc's own suggestion); `files/handlers.rs` three `disk_path(...)?`
  (returns `UnifiedResult<PathBuf>`); `documents/handlers_files.rs`
  range-download stream now collects owned `Bytes` chunks first (the
  stream borrowed a local that dropped at fn end); `chat/helpers.rs` +
  `documents/validation.rs` `body != &caller_company` (`&String` vs
  owned `String`); `software` `&Some(vendor.clone())` (the temporary
  `Some(vendor)` moved the value before the next borrow);
  `vcs::Provider` gained `Hash` (fieldless enum, used as a `HashMap` key).

Fix script: `~/workspace/unified-check/fixround2-layer2.py`.

## Layer 3 — 6 -> ? (my own heuristic bug)

Verify 2's 6 errors were all `cannot find value 'debug'`, introduced by
the layer-2 script: it copied the error-reporting expression from the
first `error_response` in the function, but in 3 functions `debug` is
defined *after* the guards (or not at all):

- `email_providers/handlers.rs::get_connection_handler`: moved
  `let debug = state.is_debug();` above the guards.
- `email_providers/handlers.rs::get_default_connection`: no state/ctx in
  scope and the function never reports errors — Err arms now use `false`.
- `mail/handlers.rs::send_mail`: moved `let state: Arc<MailState> = ...`
  and `let debug = ...` above the guards (both are pure; guards only use
  `&req`).

## Verify 3 — OOM-killed, exit 0 NOT observed

`lean-build-20261123-fixround2-verify3.log`: the `unified` bin rustc was
killed mid-compile — `sccache: Compile terminated by signal 9` — after
emitting 10,580 warnings and **zero** `error` diagnostics; cargo reports
exit 101 for the dead compiler process. The log is complete (tail +
`EXIT=101` present) but contains no error list, so it proves nothing
about the code either way. Memory at the time was tight (other agents
active; ~5.8 GiB available vs the bin crate's ~5.7 GiB RSS). Per the
task's standing rule the run was NOT retried repeatedly.

**What remains:** one more lean `cargo check --offline` on the root bin
when the machine is quiet (or the Colab route below) to observe the
final verdict. The last complete measurement is verify 2 (6 errors), and
all 6 were fixed by the three small layer-3 edits above; nothing else in
the tree changed since, so exit 0 is expected but **not yet observed**.

## Colab route (if the VM cannot run the check)

Notebook: `colab/Unified_cargo_check_private.ipynb`; steps:
`docs/integration/COLAB_RUN_STEPS.md`. Not run by this worker. The
notebook is the fallback if lean runs keep OOM-killing: it runs the same
`cargo check --offline` on a bigger machine. No Colab run is needed if a
quiet-VM lean run observes exit 0 first.

## Design decisions (recorded, not asked)

- `appointments::guest_book` (public, unauthenticated): the scope check
  runs against `&None` — the only scope a guest can have. Fail-closed
  rule A: only global (unscoped) services can be guest-booked;
  company-owned services are denied (mapped to the generic failure).
  The alternative (deleting the check) would have removed a deliberate
  security check; the sibling public list endpoints do no scoping, but
  booking is a write, and the strict-by-default write rule says an
  unresolved caller may write nothing.
- `mail::require_company_for_write` returns `Option<String>` like the
  domains/workflows copies, not `Option<Uuid>` like properties/elearning:
  mail's `caller_company` is String-scoped, so the guard matches it.
- `backup::list_jobs` returns `(Caller, Option<String>)` from its auth
  closure instead of hoisting the resolution out: smallest diff, same
  behaviour, same error mapping.

## rustfmt

Every file changed in layers 1–3 was checked with
`rustfmt --edition 2024 --check` (own-file hunks only): zero diff on
every edited line. The pre-existing tree-wide fmt debt (~9,944 hunks)
was left untouched. The only whole-block reorder is the import block in
`documents/handlers_files.rs`, required to place the added
`use uuid::Uuid;` in rustfmt-canonical position.
