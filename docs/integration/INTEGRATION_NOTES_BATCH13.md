# INTEGRATION NOTES — Batch 13 (Education: Academic / Curriculum / Admissions / School Fees)

Coordinator-verified by read-back against disk. Nothing was pushed. Remote head untouched (`7cf6853c`).

## 1. What was built

| Item | File | Lines | Routes | Migration | Tables |
|---|---|---|---|---|---|
| Academic system | `src/modules/academic.rs` (new) | 6,126 | 90 | `2026-11-22-000089_academic` (443 lines) | 18 |
| Curriculum (2013) | `src/modules/curriculum.rs` (new) | 3,942 | 68 | `2026-11-22-000087_curriculum` (337 lines) | 13 |
| Admissions | `src/modules/admissions.rs` (new) | 4,601 | 48 | `2026-11-22-000088_admissions` (279 lines) | 10 |
| School fees | `src/modules/billing.rs` (+3,909, B4's portion only) | +3,909 | +30 | `2026-11-22-000090_school_fees` (141 lines) | 7 |
| **Batch 13 totals** | **3 new files + billing extension** | **18,578 new lines** | **236 new routes** | **4 migrations** | **48 tables** |

Tree at verification: 131 `.rs` files in `src/modules/`, 407,816 Rust lines in `src/`, 96 migration dirs.
`billing.rs` is now 8,211 lines total (3,341 base + 3,909 school fees + ~961 batch-12 printing/invoicing added after B4 finished; B4 re-verified its 30/30 routes intact after batch 12's edits).

### Tables
- Academic (18): `academic_programs`, `academic_students`, `academic_lecturers`, `academic_subjects`, `academic_rooms`, `academic_semesters`, `academic_calendar_events`, `academic_schedules`, `academic_enrollments`, `academic_attendance`, `academic_grade_components`, `academic_grades`, `academic_grade_appeals`, `academic_transcripts`, `academic_theses`, `academic_standing_records`, `academic_definitions`, `academic_settings`. Cross-module ids (`user_id`, `department_id`, `elearning_course_id`) are plain TEXT, no FKs; FKs only between academic tables.
- Curriculum (13): `curriculum_frameworks`, `curriculum_levels`, `curriculum_subjects`, `curriculum_core_competencies`, `curriculum_basic_competencies`, `curriculum_structures`, `curriculum_assessment_domains`, `curriculum_grade_descriptors`, `curriculum_lesson_plans`, `curriculum_report_cards`, `curriculum_report_card_entries`, `curriculum_interventions`, `curriculum_settings`. Seeds: framework `curriculum-2013`, 3 levels, 5 assessment domains, 9 settings rows.
- Admissions (10): `admission_periods`, `admission_period_quotas`, `admission_stage_definitions`, `admission_required_documents`, `admission_applications`, `admission_application_scores`, `admission_checklist_items`, `admission_waitlist_entries`, `admission_results`, `admission_settings`. Money NUMERIC(19,4), scores NUMERIC(10,4).
- School fees (7): `school_fee_types`, `school_fee_schedules`, `school_fee_charges`, `school_fee_discounts`, `school_fee_receipts`, `school_fee_guardians`, `school_fee_reminders`. All money NUMERIC(19,4); fee layer uses integer minor units, converting to major-unit f64 only at the invoice boundary.

## 2. Module registration (mod.rs — integrator work, batch 13 did NOT edit it)

```rust
pub mod academic;
pub mod curriculum;
pub mod admissions;
// billing.rs is already registered; school fees live inside it.
```

In `register_all`, add (next to the other `register` calls):

```rust
academic::register(registry);
curriculum::register(registry);
admissions::register(registry);
```

Verified: `src/modules/mod.rs` currently has **0** mentions of academic/curriculum/admissions/school_fee.

## 3. Diesel schema / models (schema.rs, models.rs — integrator work)

Batch 13 added **0** lines to `schema.rs` / `models.rs` (verified: 0 mentions of `academic_`/`curriculum_`/`admission_`/`school_fee` / `Academic`/`Curriculum`/`Admission`/`SchoolFee`).
Derive the `table!` blocks and Queryable/Insertable structs from the migration `up.sql` files listed in §1 (column definitions live there; do not invent columns). Table names are listed in §1. Note the modelling conventions each builder used:
- Academic: FKs only between academic tables; cross-module ids are TEXT.
- Curriculum: framework/level/subject/competency hierarchy + `curriculum_settings` key/value rows.
- Admissions: FK cascades between admission tables; `invoice_id` on charges is TEXT (no FK — invoices live in billing).
- School fees: `school_fee_charges` unique `(schedule_id, period_label)` where not void; `school_fee_reminders` PK `(charge_id, offset_days, recipient_user_id)`; guardian link unique active pair.

## 4. Migration order and renumbering (integrator work)

Batch-13 numbers **000087–000090 are unique and sequential** (verified against the live directory listing). Each builder dodged live collisions while writing (000078 taken by batch 11, 000079 by batch 11 import/export hardening, 000080 taken twice, 000081 by batch 12 manufacturing).

**Live duplicate numbers the integrator must renumber (verified fresh at batch-13 verification time):**
- `000056` ×2: `000056_community` vs `000056_storage_product_byo`
- `000070` ×2: `000070_presets` vs `000070_saas_domains`
- `000075` ×2: `000075_appointments` vs `000075_digital_products`
- `000077` ×2: `000077_field_service` vs `000077_webinars`
- `000080` ×2: `000080_backup_drill_verify_tenant` vs `000080_marketing`
- `000100_calculator` out of sequence

**Stale collision claims (verified FALSE on disk — do not act on them):** 000068 (only `000068_licensing_saas`; network_tools is 000069), 000074 (single occupant now), 000081 (only `000081_manufacturing` — the old `000081_curriculum` dir was moved to recoverable trash by B2, `000081_admissions` was renumbered to 000088). Batch 10's warning about a "batch 12 `000081_curriculum`" could NOT be reproduced: no second curriculum module file or migration exists on disk; the only `curriculum.rs` is batch 13 B2's (3,942 lines, verified) and the only curriculum migration is `000087_curriculum`. If batch 12 lands a curriculum module later, reconcile then.

## 5. Event contracts — VERIFIED against source (read this before wiring)

Notation: ✅ = both sides verified on disk. ⚠️ = mismatch / missing side, integrator must decide.

### 5.1 Verified working pairs
- ✅ E-learning emits `elearning.course.created/updated/deleted` (elearning.rs:1677, 1784, 1836) and `elearning.enrollment.completed` (:1391, :1399) — academic subscribes to all of them.
- ✅ Forms implements `forms.mapping.requested` (forms.rs:1308) and emits `forms.submission.received` (:2655) — admissions subscribes to both.
- ✅ Billing emits `invoice.paid` (billing.rs:1880, 2134, 2724, 3365, 4366) — admissions and school fees subscribe.
- ✅ Printing subscribes to `print.job.requested` (printing.rs:2400, handler `on_print_job_requested`) — academic (transcript) and curriculum (report card) both emit it. Printing also subscribes to `school_fees.receipt.print_requested` (:2423, handler `on_school_fee_receipt_print_requested`) — school fees emits it. **All three print paths work via subscribed events.**
- ✅ School fees reuses the billing invoice/payment flow (`invoice.payment.recorded`, `invoice.paid`, `invoice.voided` sync handlers verified in billing.rs).

### 5.2 Mismatches / missing sides (integrator convergence points)
1. ⚠️ **Admissions → Academic handoff has NO listener.** Admissions emits `academic.student.create_requested` `{application_id, period_id, applicant_user_id, full_name, email, phone, program_id, level, academic_year, term}` on enroll, and subscribes to `academic.student.created` `{application_id, student_id}`. Academic does NOT subscribe to `academic.student.create_requested` (verified: 0 occurrences in academic.rs) and its `academic.student.created` payload is `{student_id, user_id, program_id}` — no `application_id`, so admissions cannot link the student back to the application from this event alone. **Integrator:** add a `academic.student.create_requested` handler in academic.rs (create the student, then emit `academic.student.created` WITH `application_id` echoed), or change admissions to poll/query.
2. ⚠️ **Curriculum's academic event payloads do not match academic's emits.** Curriculum consumes `academic.student.created` expecting `{student_ref, level_id}`, `academic.student.enrolled` expecting `{student_ref, level_id, grade_year, academic_year}`, and `academic.grade.recorded` expecting `{academic_grade_id, student_ref, basic_competency_id, score}`. Academic actually emits `academic.student.created` `{student_id, user_id, program_id}` and `academic.grade.recorded` `{grade_id, enrollment_id, component_id, score}` (verified at academic.rs:1964 and :4365/:4407). **Integrator:** decide the canonical identity key (`student_id` vs `student_ref`) and the grade→competency mapping (academic grades are per-component; curriculum needs per-basic-competency) and adapt one side. Do NOT duplicate student/grade records on either side.
3. ⚠️ **School fees consumes events academic never emits.** School fees subscribes to `academic.student.enrolled` and `academic.student.withdrawn` (auto-create schedules / cancel schedules). Academic emits NEITHER (verified: 0 occurrences). Academic DOES emit `academic.enrollment.approved` `{enrollment_id, student_id, subject_id, semester_id}` (academic.rs:3879) and `academic.enrollment.dropped` `{enrollment_id, student_id, subject_id}` (:3980). **Integrator:** either add `academic.student.enrolled` / `.withdrawn` emits to academic.rs, or repoint school fees at `academic.enrollment.approved` / `.dropped`.
4. ⚠️ **`elearning.course.requested` has NO answerer.** Academic emits this hook query expecting `{exists, course_id, title, status}`; no other file in `src/modules/` mentions it (verified). Academic handles the unanswered case (stores links with `elearning_course_verified: false`, never rejects). **Integrator:** add an answering handler in elearning.rs.
5. ⚠️ **`billing.invoice.requested` has NO listener in billing.** Admissions emits it to request the registration-fee invoice (same pattern as appointments); billing.rs contains no listener (verified). Until the integrator adds one, admissions fee payment is confirmed via the manual `mark_paid` path, and the `invoice.paid` → application linkage (admissions expects `invoice.order_id` == application id) cannot work.
6. ⚠️ **`academic.student.lookup` has NO answerer.** Curriculum emits this hook query expecting `{exists: bool}`; nothing answers it (verified). Curriculum tolerates unanswered queries. Integrator may add an academic-side handler.
7. ℹ️ **Module-specific print events are unhandled signals.** `academic.transcript.print_requested` and `curriculum.report_card.print_requested` have no subscriber in printing.rs (verified); both modules ALSO emit the shared `print.job.requested`, which IS subscribed — so printing works. Integrator may leave the module-specific events as audit signals or drop them; do not wire a second print path.
8. ℹ️ **Documents module has no synchronous lookup hook.** Admissions links checklist documents via the `document.uploaded` naming convention (`module_id == "admissions"`, document name == `document_key`); the explicit attach endpoint cannot verify a document id against the documents store. Integrator may add a lookup hook if wanted.

### 5.3 Full emitted/consumed lists (per builder reports, spot-verified above)
- Academic emits: `academic.student.created`, `academic.lecturer.created`, `academic.program.created`, `academic.subject.created/updated`, `academic.schedule.created`, `academic.enrollment.requested/approved/rejected/dropped/completed`, `academic.attendance.recorded`, `academic.grade.recorded`, `academic.grade.appeal_filed/resolved`, `academic.transcript.issued`, `academic.transcript.print_requested`, `print.job.requested`, `academic.thesis.created/defense_scheduled/status_changed`, `academic.standing.changed`, `notification.requested`. Consumes: `timetracker.entry_logged` + `timetracker.time.logged`, `elearning.course.created/updated/deleted`, `elearning.enrollment.completed`; emits hook query `elearning.course.requested`.
- Curriculum emits: `curriculum.framework.{created,updated,deleted,default_changed}`, `curriculum.level.{created,updated,deleted}`, `curriculum.subject.{created,updated,deleted}`, `curriculum.core_competency.{created,updated,deleted}`, `curriculum.basic_competency.{created,updated,deleted}`, `curriculum.structure.{created,updated,deleted}`, `curriculum.assessment_domain.{created,updated,deleted}`, `curriculum.grade_descriptor.{created,updated,deleted}`, `curriculum.lesson_plan.{created,updated,deleted,published,archived}`, `curriculum.report_card.{created,updated,deleted,issued}`, `curriculum.report_card.entry.{added,updated,removed}`, `curriculum.report_card.print_requested`, `print.job.requested`, `curriculum.intervention.{created,updated,deleted,completed,cancelled}`, `curriculum.settings.updated`. Consumes (lookup caches only, opaque refs): `academic.student.created`, `academic.student.enrolled`, `academic.grade.recorded`; emits hook query `academic.student.lookup`.
- Admissions emits: `admissions.*` lifecycle events (period/stage/application/score/checklist/result/waitlist/settings — see admissions.rs header), `billing.invoice.requested`, `admissions.fee_invoice_requested`, `academic.student.create_requested`, `admissions.enrollment_requested`, `admissions.waitlist_promoted`, `notification.requested`. Consumes: `forms.mapping.requested`, `forms.submission.received`, `document.uploaded`, `document.deleted`, `invoice.paid`, `academic.student.created`.
- School fees emits: `school_fees.type.created/.updated/.deleted`, `school_fees.settings.updated`, `school_fees.schedule.created/.updated/.cancelled`, `school_fees.charge.created/.paid/.voided`, `school_fees.fee.reminder`, `school_fees.reminders.completed`, `school_fees.receipt.issued`, `school_fees.receipt.print_requested`, `school_fees.discount.created/.approved/.deleted`, `school_fees.guardian.linked/.unlinked`, plus reused `invoice.payment.recorded`, `invoice.paid`, `invoice.voided`, `notification.requested` (kind `school_fee_reminder`). Consumes: `academic.student.enrolled`, `academic.student.withdrawn`, `invoice.payment.recorded`, `invoice.paid`, `invoice.voided`.

## 6. AUDITED_EVENTS additions (audit module — integrator work)

Add all `academic.*`, `curriculum.*`, `admissions.*`, and `school_fees.*` event names emitted above (per §5.3). Academic's report explicitly notes its events must be added for audit coverage; curriculum/admissions/school-fees emit audit-shaped events with the same requirement.

## 7. Role capabilities (roles module — integrator work)

No `academic:*`, `curriculum:*`, `admissions:*`, or `school_fees:*` capability is granted by any default role today (verified per builder reports) — until granted, only admins can use the write paths; all checks fail closed via the `role.check` hook. Integrator must define and seed:
- `academic:view/manage/advise` (+ grade/attendance sub-checks as implemented in academic.rs: `require_permission` / `check_capability` / `can_grade` / `can_approve_enrollment` / `is_staff`)
- `curriculum:view/manage/teach` (teacher-ownership enforced on lesson-plan write/delete)
- `admissions:*` (per admissions.rs: `require_permission`/`check_capability`, fail-closed)
- `school_fees.view` / `school_fees.manage` (behind the `role.check` hook; B4's handlers call `fee_caller` + `fee_require_*`)

## 8. Scheduler

Batch 13 requires **no new scheduler kind** (verified: no builder registered one). Notes for the integrator: school-fee reminders run on demand via `POST /api/v1/billing/school-fees/reminders/run` — add a scheduler arm for it if wanted. Curriculum report-card/admissions result flows are request-driven, not scheduled.

## 9. README / TODO lines (integrator work)

- Module count +3 (academic, curriculum, admissions; school fees is inside billing, not a new module). Migrations +4 (000087–000090). Tables +48 (160 → 208 against the last pushed baseline; recount at push time — the tree has grown past that baseline).
- Suggested README entries: "Academic System — students, lecturers, programs, schedules, enrollment, attendance, grades/GPA, transcripts, thesis, standing"; "Curriculum — data-driven frameworks (Curriculum 2013 seeded), competencies, lesson plans, three-domain assessment, report cards, remedial/enrichment"; "Admissions — periods/quotas, forms-driven registration, staged selection + scoring, results, waitlist, fee via billing, academic handoff"; "School Fees — tuition schedules, one-off fees, arrears, discounts/scholarships, receipts (inside Billing)".
- TODO: record the convergence items in §5.2 as integrator tasks, plus the renumbering list in §4.

## 10. Modularity rule (standing, absolute) — checked

- Presets/frameworks/fee types/grade scales/standing rules/status vocabularies are all DATA (admin-editable definitions/settings endpoints or seeded settings rows), not code.
- Disclosed hard-coded spots (per builder, recorded honestly, not hidden): seed default values (grade scale A–F, standing rules, status lists, transition tables, credit cap 24, passing points 2.0, paper size `a4`, grade bands A–E, reminder days, aging cut-offs, currency `USD`, receipt prefix `RCPT`); score bounds 0–100; ISO day numbering 1–7 and `HH:MM` time format; ISO 3-letter currency format; `YYYY-MM-DD` dates; status vocabularies and transition targets referenced by literal in a few guards (if an admin renames a status in settings, those guards stop matching — documented in module headers); cross-module event names (fixed strings by definition).
- No hard-coded provider/plan/preset lists in batch-13 code (not applicable to education, verified by grep: 0 forbidden-name hits, 0 Indonesian word-boundary hits, 0 stub macros across all batch-13 files — see §11).

## 11. Coordinator verification (by read-back, not by trusting reports)

- Line counts match every builder report exactly: academic 6,126; curriculum 3,942; admissions 4,601; billing 3,341 → 8,211 (B4's +3,909 verified as the delta to 7,250 immediately after its edits; batch 12 added ~961 lines after).
- Route parity: academic 90 `.route(` == 90 `RouteInfo {`; curriculum 68 == 68; admissions 48 == 48; billing school-fees 30 configure == 30 RouteInfo (B4 verified; my `school-fees` grep count of 61 = 30 + 30 + 1 section comment).
- Migration CREATE == DROP on all four: 18/18, 13/13, 10/10, 7/7.
- Combined greps over all batch-13 files: forbidden reference-project name **0**; `todo!`/`unimplemented!` **0** (unfiltered, three new files); Indonesian words word-boundary (`siakad|k13|ppdb|spp|rapor|rpp|merdeka|guru|siswa|sekolah|nilai|krs|ipk`) **0** in the three new files and in the billing.rs school-fees section. Note: `semester` is standard English academic vocabulary and is used.
- Shared files untouched by batch 13: `mod.rs`, `schema.rs`, `models.rs` have 0 mentions of batch-13 modules (verified by grep).
- Cross-module event checks in §5 were verified against the real emitting/subscribing files, not taken from builder reports (including the payload mismatches in §5.2 items 1–3, found by reading `academic.rs:1964`, `:4365`, `:3879`, `:3980`).
- B4's concurrent-edit handling verified: billing.rs was stable at B4's start (md5 unchanged across two reads), B4's edits are append-only in a delimited `// School fees` section, batch 12's 14 invoice-template/print routes coexist (RouteInfo total now 23 original + 14 batch-12 + 30 school-fees = 67); B4 re-verified its 30/30 routes intact after batch 12's edits.

## 12. OPEN list (explicit)

1. **`cargo check` / `cargo build` has NEVER run anywhere** (no toolchain, crates.io blocked) — compilation is unverified for all four items. Riskiest unverified surfaces per builders: `impl_listable!` macro expansions and `PageResult`/`json!` generic inference (curriculum); DashMap guard patterns and the fee-layer minor/major unit boundary (school fees); the TimeTracker timestamp handling B1 fixed by review only.
2. Migrations never executed against live PostgreSQL; no endpoint live-tested; in-memory DashMap stores are authoritative everywhere (tables are persistence targets).
3. Integrator items: §2 registration, §3 schema/models, §4 renumbering, §5.2 convergence fixes (items 1–6), §6 audit events, §7 capabilities, §9 README/TODO.
4. `billing.invoice.requested` has no listener (admissions fee flow degrades to manual `mark_paid` until added).
5. No `academic.student.create_requested` handler (admissions→academic handoff dead-ends until added).
6. No `elearning.course.requested` answerer (academic subject links stay `elearning_course_verified: false`).
7. No `academic.student.lookup` answerer (curriculum tolerates it).
8. School fees: refunds/credit notes do not sync back to charge paid totals (no hook on invoice-credit events); discount stacking sums with no priority rule (admins must avoid overlapping discounts); receipts record total paid so far per charge, not one receipt per payment.
9. Curriculum: the `"issued"` lock check and `"published"/"archived"/"completed"/"cancelled"` transition targets rely on seeded status vocabulary values — renaming those statuses in settings breaks the guards (documented in the module header).
10. Parallel-round notes for the integrator: batch 14 (online exams, library, SaaS packages) and batch 16 (global calendar) are running concurrently — the academic calendar inside academic.rs and batch 16's global calendar need reconciliation at integration (academic calendar events should feed the global calendar via events, not be duplicated), and batch 14's exams module should be checked against curriculum's three-domain assessment for overlap.
