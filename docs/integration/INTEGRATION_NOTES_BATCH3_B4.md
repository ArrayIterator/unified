# Integration notes — Batch 3: Advisor + Risk Mitigation

Builder scope: `src/modules/advisor.rs`, `src/modules/risk.rs`,
`migrations/2026-11-22-000039_advisor_risk/` (up.sql, down.sql).
This file is the integrator's checklist. Nothing in this file is
code that runs; apply each snippet exactly as written, in order.
Do NOT edit the module files themselves to fix an integration
mismatch — send the mismatch back to the batch-3 builder.

Module ids: `advisor` (requires billing, licensing, crm,
notifications, audit; NOT sellable), `risk` (requires
notifications, audit, projects; NOT sellable).

## 1. `src/modules/mod.rs` — module declarations + registration

### 1a. Declarations (alphabetical placement)

`pub mod advisor;` goes FIRST, before `pub mod ai;`
(`advisor` sorts before `ai`):

```rust
pub mod advisor;
pub mod ai;
pub mod ai_support;
```

`pub mod risk;` goes between `pub mod records;` and
`pub mod roles;`:

```rust
pub mod records;
pub mod risk;
pub mod roles;
```

### 1b. Registration (`register_all`, dependency order)

Both modules register at the end of the "People and support"
group, after `ai::register(registry);` — every module they
require (billing, licensing, crm, notifications, audit,
projects) is registered earlier in `register_all`:

```rust
    // People and support.
    crm::register(registry);
    departments::register(registry);
    tickets::register(registry);
    chat::register(registry);
    ai_support::register(registry);
    ai::register(registry);
    // Advisor and Risk Mitigation (batch 3): the advisor reads
    // billing, licensing, and CRM events; risk requests mitigation
    // tasks from the Projects module (see section 6).
    advisor::register(registry);
    risk::register(registry);
    files::register(registry);
    announcements::register(registry);
```

## 2. `src/db/schema.rs` — table definitions

Append the ten `diesel::table!` blocks below (in this order),
then add the `joinable!` lines and the
`allow_tables_to_appear_in_same_query!` entries. Money note: none
of these tables carries a money column (advisor figures live in
`advisor_suggestions.figures` as JSONB); any money column added
later MUST be `Numeric` / `NUMERIC(19,4)` with
`diesel::data_types::PgNumeric` on the Rust side.

```rust
diesel::table! {
    advisor_suggestions (id) {
        id -> Uuid,
        company_id -> Nullable<Uuid>,
        rule_id -> Varchar,
        title -> Varchar,
        body -> Text,
        recommended_action -> Text,
        severity -> Varchar,
        severity_reason -> Text,
        status -> Varchar,
        source_type -> Varchar,
        source_id -> Varchar,
        source_ids -> Jsonb,
        figures -> Jsonb,
        dedupe_key -> Varchar,
        expires_at -> Nullable<Timestamptz>,
        snoozed_until -> Nullable<Timestamptz>,
        acknowledged_by -> Nullable<Varchar>,
        acknowledged_at -> Nullable<Timestamptz>,
        dismissed_by -> Nullable<Varchar>,
        dismissed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    advisor_rule_runs (id) {
        id -> Uuid,
        company_id -> Nullable<Uuid>,
        rule_id -> Varchar,
        ran_at -> Timestamptz,
        suggestions_created -> Int4,
        suggestions_updated -> Int4,
        inputs_summary -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    risks (id) {
        id -> Uuid,
        company_id -> Nullable<Uuid>,
        title -> Varchar,
        description -> Text,
        category -> Varchar,
        likelihood -> Int2,
        impact -> Int2,
        score -> Int2,
        band -> Varchar,
        owner_id -> Varchar,
        status -> Varchar,
        next_review_at -> Nullable<Timestamptz>,
        alert_threshold -> Int2,
        created_by -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    risk_mitigation_plans (id) {
        id -> Uuid,
        risk_id -> Uuid,
        title -> Varchar,
        description -> Text,
        target_date -> Nullable<Timestamptz>,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    risk_mitigation_tasks (id) {
        id -> Uuid,
        plan_id -> Uuid,
        risk_id -> Uuid,
        title -> Varchar,
        description -> Text,
        assignee_id -> Nullable<Varchar>,
        status -> Varchar,
        due_date -> Nullable<Timestamptz>,
        external_project_task_id -> Nullable<Varchar>,
        awaiting_projects_handler -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    risk_checklist_templates (id) {
        id -> Uuid,
        title -> Varchar,
        description -> Text,
        category -> Varchar,
        created_by -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    risk_checklist_template_items (id) {
        id -> Uuid,
        template_id -> Uuid,
        title -> Varchar,
        description -> Text,
        required -> Bool,
        sort_order -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    risk_checklist_instances (id) {
        id -> Uuid,
        template_id -> Uuid,
        company_id -> Nullable<Uuid>,
        risk_id -> Nullable<Uuid>,
        title -> Varchar,
        status -> Varchar,
        completed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    risk_checklist_instance_items (id) {
        id -> Uuid,
        instance_id -> Uuid,
        template_item_id -> Uuid,
        title -> Varchar,
        required -> Bool,
        sort_order -> Int4,
        completed -> Bool,
        evidence -> Text,
        completed_by -> Nullable<Varchar>,
        completed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    risk_alerts (id) {
        id -> Uuid,
        risk_id -> Uuid,
        company_id -> Nullable<Uuid>,
        kind -> Varchar,
        message -> Text,
        score -> Nullable<Int2>,
        notified_user_id -> Nullable<Varchar>,
        figures -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
```

Joins (next to the other `joinable!` lines):

```rust
diesel::joinable!(risk_mitigation_plans -> risks (risk_id));
diesel::joinable!(risk_mitigation_tasks -> risk_mitigation_plans (plan_id));
diesel::joinable!(risk_mitigation_tasks -> risks (risk_id));
diesel::joinable!(risk_checklist_template_items -> risk_checklist_templates (template_id));
diesel::joinable!(risk_checklist_instances -> risk_checklist_templates (template_id));
diesel::joinable!(risk_checklist_instances -> risks (risk_id));
diesel::joinable!(risk_checklist_instance_items -> risk_checklist_instances (instance_id));
diesel::joinable!(risk_alerts -> risks (risk_id));
```

Add all ten table names to the
`allow_tables_to_appear_in_same_query!` list:
`advisor_suggestions`, `advisor_rule_runs`, `risks`,
`risk_mitigation_plans`, `risk_mitigation_tasks`,
`risk_checklist_templates`, `risk_checklist_template_items`,
`risk_checklist_instances`, `risk_checklist_instance_items`,
`risk_alerts`.

## 3. `src/db/models.rs` — model structs

One `Queryable` + one `Insertable` struct per table. `company_id`
is `Option<Uuid>` (matching `invoices.company_id`); user/owner
ids are `String`/`Option<String>` (matching `invoices.user_id`).
`score`, `likelihood`, `impact`, `alert_threshold` are `i16`
(SMALLINT); `suggestions_created`/`suggestions_updated`/
`sort_order` are `i32`. No `PgNumeric` field exists in these
tables (see the money note in section 2). `figures`,
`source_ids`, and `inputs_summary` are `serde_json::Value`
(Jsonb). Add the usual `#[derive(Debug, Clone, Queryable,
Serialize)]` / `#[derive(Debug, Clone, Insertable,
Deserialize)]` with `#[diesel(table_name = ...)]` and
`#[diesel(check_for_backend(diesel::pg::Pg))]` following the
file's existing convention.

```rust
#[derive(Debug, Clone, Queryable, Serialize)]
#[diesel(table_name = advisor_suggestions)]
pub struct AdvisorSuggestion {
    pub id: Uuid,
    pub company_id: Option<Uuid>,
    pub rule_id: String,
    pub title: String,
    pub body: String,
    pub recommended_action: String,
    pub severity: String,
    pub severity_reason: String,
    pub status: String,
    pub source_type: String,
    pub source_id: String,
    pub source_ids: Value,
    pub figures: Value,
    pub dedupe_key: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub snoozed_until: Option<DateTime<Utc>>,
    pub acknowledged_by: Option<String>,
    pub acknowledged_at: Option<DateTime<Utc>>,
    pub dismissed_by: Option<String>,
    pub dismissed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = advisor_suggestions)]
pub struct NewAdvisorSuggestion {
    pub id: Uuid,
    pub company_id: Option<Uuid>,
    pub rule_id: String,
    pub title: String,
    pub body: String,
    pub recommended_action: String,
    pub severity: String,
    pub severity_reason: String,
    pub status: String,
    pub source_type: String,
    pub source_id: String,
    pub source_ids: Value,
    pub figures: Value,
    pub dedupe_key: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub snoozed_until: Option<DateTime<Utc>>,
    pub acknowledged_by: Option<String>,
    pub acknowledged_at: Option<DateTime<Utc>>,
    pub dismissed_by: Option<String>,
    pub dismissed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Serialize)]
#[diesel(table_name = advisor_rule_runs)]
pub struct AdvisorRuleRun {
    pub id: Uuid,
    pub company_id: Option<Uuid>,
    pub rule_id: String,
    pub ran_at: DateTime<Utc>,
    pub suggestions_created: i32,
    pub suggestions_updated: i32,
    pub inputs_summary: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = advisor_rule_runs)]
pub struct NewAdvisorRuleRun {
    pub id: Uuid,
    pub company_id: Option<Uuid>,
    pub rule_id: String,
    pub ran_at: DateTime<Utc>,
    pub suggestions_created: i32,
    pub suggestions_updated: i32,
    pub inputs_summary: Value,
}

#[derive(Debug, Clone, Queryable, Serialize)]
#[diesel(table_name = risks)]
pub struct Risk {
    pub id: Uuid,
    pub company_id: Option<Uuid>,
    pub title: String,
    pub description: String,
    pub category: String,
    pub likelihood: i16,
    pub impact: i16,
    pub score: i16,
    pub band: String,
    pub owner_id: String,
    pub status: String,
    pub next_review_at: Option<DateTime<Utc>>,
    pub alert_threshold: i16,
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = risks)]
pub struct NewRisk {
    pub id: Uuid,
    pub company_id: Option<Uuid>,
    pub title: String,
    pub description: String,
    pub category: String,
    pub likelihood: i16,
    pub impact: i16,
    pub score: i16,
    pub band: String,
    pub owner_id: String,
    pub status: String,
    pub next_review_at: Option<DateTime<Utc>>,
    pub alert_threshold: i16,
    pub created_by: Option<String>,
}

#[derive(Debug, Clone, Queryable, Serialize)]
#[diesel(table_name = risk_mitigation_plans)]
pub struct RiskMitigationPlan {
    pub id: Uuid,
    pub risk_id: Uuid,
    pub title: String,
    pub description: String,
    pub target_date: Option<DateTime<Utc>>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = risk_mitigation_plans)]
pub struct NewRiskMitigationPlan {
    pub id: Uuid,
    pub risk_id: Uuid,
    pub title: String,
    pub description: String,
    pub target_date: Option<DateTime<Utc>>,
    pub status: String,
}

#[derive(Debug, Clone, Queryable, Serialize)]
#[diesel(table_name = risk_mitigation_tasks)]
pub struct RiskMitigationTask {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub risk_id: Uuid,
    pub title: String,
    pub description: String,
    pub assignee_id: Option<String>,
    pub status: String,
    pub due_date: Option<DateTime<Utc>>,
    pub external_project_task_id: Option<String>,
    pub awaiting_projects_handler: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = risk_mitigation_tasks)]
pub struct NewRiskMitigationTask {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub risk_id: Uuid,
    pub title: String,
    pub description: String,
    pub assignee_id: Option<String>,
    pub status: String,
    pub due_date: Option<DateTime<Utc>>,
    pub external_project_task_id: Option<String>,
    pub awaiting_projects_handler: bool,
}

#[derive(Debug, Clone, Queryable, Serialize)]
#[diesel(table_name = risk_checklist_templates)]
pub struct RiskChecklistTemplate {
    pub id: Uuid,
    pub title: String,
    pub description: String,
    pub category: String,
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = risk_checklist_templates)]
pub struct NewRiskChecklistTemplate {
    pub id: Uuid,
    pub title: String,
    pub description: String,
    pub category: String,
    pub created_by: Option<String>,
}

#[derive(Debug, Clone, Queryable, Serialize)]
#[diesel(table_name = risk_checklist_template_items)]
pub struct RiskChecklistTemplateItem {
    pub id: Uuid,
    pub template_id: Uuid,
    pub title: String,
    pub description: String,
    pub required: bool,
    pub sort_order: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = risk_checklist_template_items)]
pub struct NewRiskChecklistTemplateItem {
    pub id: Uuid,
    pub template_id: Uuid,
    pub title: String,
    pub description: String,
    pub required: bool,
    pub sort_order: i32,
}

#[derive(Debug, Clone, Queryable, Serialize)]
#[diesel(table_name = risk_checklist_instances)]
pub struct RiskChecklistInstance {
    pub id: Uuid,
    pub template_id: Uuid,
    pub company_id: Option<Uuid>,
    pub risk_id: Option<Uuid>,
    pub title: String,
    pub status: String,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = risk_checklist_instances)]
pub struct NewRiskChecklistInstance {
    pub id: Uuid,
    pub template_id: Uuid,
    pub company_id: Option<Uuid>,
    pub risk_id: Option<Uuid>,
    pub title: String,
    pub status: String,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Serialize)]
#[diesel(table_name = risk_checklist_instance_items)]
pub struct RiskChecklistInstanceItem {
    pub id: Uuid,
    pub instance_id: Uuid,
    pub template_item_id: Uuid,
    pub title: String,
    pub required: bool,
    pub sort_order: i32,
    pub completed: bool,
    pub evidence: String,
    pub completed_by: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = risk_checklist_instance_items)]
pub struct NewRiskChecklistInstanceItem {
    pub id: Uuid,
    pub instance_id: Uuid,
    pub template_item_id: Uuid,
    pub title: String,
    pub required: bool,
    pub sort_order: i32,
    pub completed: bool,
    pub evidence: String,
    pub completed_by: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Serialize)]
#[diesel(table_name = risk_alerts)]
pub struct RiskAlert {
    pub id: Uuid,
    pub risk_id: Uuid,
    pub company_id: Option<Uuid>,
    pub kind: String,
    pub message: String,
    pub score: Option<i16>,
    pub notified_user_id: Option<String>,
    pub figures: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = risk_alerts)]
pub struct NewRiskAlert {
    pub id: Uuid,
    pub risk_id: Uuid,
    pub company_id: Option<Uuid>,
    pub kind: String,
    pub message: String,
    pub score: Option<i16>,
    pub notified_user_id: Option<String>,
    pub figures: Value,
}
```

## 4. `src/modules/audit.rs` — record the new events

The audit log only records the events listed in `AUDITED_EVENTS`.
Both modules emit their lifecycle events, but nothing lands in
the audit log until the integrator adds these names to that list
(append to the existing project-lifecycle block):

```rust
    // Advisor suggestions (advisor module).
    "advisor.suggestion_created",
    "advisor.suggestion_acknowledged",
    "advisor.suggestion_dismissed",
    "advisor.suggestion_snoozed",
    // Risk register, mitigation, and checklists (risk module).
    "risk.risk_created",
    "risk.risk_updated",
    "risk.risk_deleted",
    "risk.risk_status_changed",
    "risk.threshold_crossed",
    "risk.review_overdue",
    "risk.mitigation_plan_created",
    "risk.mitigation_plan_updated",
    "risk.mitigation_plan_deleted",
    "risk.mitigation_task_updated",
    "risk.mitigation_task_deleted",
    "risk.checklist_completed",
```

Note: `risk.mitigation_task_requested` is intentionally NOT in
this list — it is a request to the Projects module, not a
completed change; the task creation itself is visible through
the Projects module's own `project.task_created` event once the
handler exists (see section 6).

## 5. `README.md` — lines to add

Add to the module list (in the same section/style as the other
batch modules):

```markdown
- **Advisor** (`advisor`): rule-based recommendations over company
  data — cash-flow warnings, overdue-invoice follow-ups, churn
  signals, expiring licenses, and underused subscriptions. Every
  suggestion cites its rule, severity formula, and source data;
  suggestions are never auto-acted on.
- **Risk Mitigation** (`risk`): risk register with server-computed
  likelihood x impact scores and bands, mitigation plans with
  tasks requested from the Projects module, threshold and
  review-date alerts, and compliance checklists.
```

## 6. `TODO.md` — lines to add

```markdown
- [ ] Advisor + Risk (batch 3): wire the Projects-module handler
  for `risk.mitigation_task_requested` (see section 7); add a
  scheduler job kind that emits `advisor.evaluate_requested`
  (periodic re-evaluation currently only runs on demand or when a
  caller emits the hook); persist the DashMap stores to the
  000039 tables (the in-memory store is authoritative, per project
  convention).
- [ ] Advisor: CRM churn rule and underused-subscription rule need
  source data the CRM/store modules do not emit yet (no
  contact-activity event, no seat-usage event); they currently
  evaluate snapshots supplied with the re-evaluate request.
```

## 7. CONVERGENCE POINT — Projects module: `risk.mitigation_task_requested`

The Risk module requests a backing Projects task by emitting:

```json
{
  "risk_id": "<risk uuid>",
  "plan_id": "<plan uuid>",
  "task_id": "<mitigation task uuid>",
  "project_id": "<projects-module project id>" | null,
  "title": "<task title>",
  "description": "<task description>",
  "assignee_id": "<user id>" | null,
  "due_date": "<RFC3339>" | null
}
```

The Projects module must register a handler for
`risk.mitigation_task_requested` (owner `"projects"`) that
creates the task (in `project_id` when given, otherwise in a
project it chooses or by declining with an error result) and
answers with a result carrying `project_task_id`:

```json
{ "project_task_id": "<created project task id>" }
```

or, when it cannot create the task:

```json
{ "error": "<reason>" }
```

Current state (verified 2026-11-22 against
`src/modules/projects.rs`): the Projects module registers
handlers only for `project.task_from_ticket_requested`,
`project.ticket_from_task_requested`, and
`project.billable_hours_requested`, and emits
`project.task_created` on task creation. Until the handler is
wired, risk tasks stay in status `requested` with
`awaiting_projects_handler: true` and
`external_project_task_id: null` — the request is still emitted
and recorded, and tasks can be tracked and completed locally.
The risk module never invents a project task id.

## 8. Events reference (for the integrator's verification)

Subscribed (must exist — verified against source):
- advisor: `invoice.created`, `invoice.paid`,
  `invoice.payment.recorded`, `invoice.voided` (billing, full
  invoice JSON), `license.expiring` (licensing), and it answers
  `advisor.evaluate_requested`.
- risk: answers `risk.review_check_requested`.

Emitted:
- advisor: `advisor.suggestion_created`,
  `advisor.suggestion_acknowledged`,
  `advisor.suggestion_dismissed`, `advisor.suggestion_snoozed`,
  `notification.requested` (critical suggestions only).
- risk: `risk.risk_created`, `risk.risk_updated`,
  `risk.risk_deleted`, `risk.risk_status_changed`,
  `risk.threshold_crossed`, `risk.review_overdue`,
  `risk.mitigation_plan_created`, `risk.mitigation_plan_updated`,
  `risk.mitigation_plan_deleted`,
  `risk.mitigation_task_updated`, `risk.mitigation_task_deleted`,
  `risk.mitigation_task_requested` (see section 7),
  `risk.checklist_completed`, `notification.requested`
  (threshold crossings and overdue reviews, to the risk owner).

Capabilities (checked via the `role.check` hook, fail-closed;
roles must grant them or every route answers 403):
`advisor:view`, `advisor:manage`, `risk:view`, `risk:create`,
`risk:edit`, `risk:delete`. Actor identity comes from the
`viewer_id` / `viewer_role` query parameters, following the
projects module's convention.

Known gaps (honest, by design — see the module headers):
- CRM emits no contact-activity event and Contact has no
  last-activity field, so `crm.churn_inactive_client` only
  evaluates snapshots in the re-evaluate payload.
- No module emits seat-usage data, so
  `billing.underused_subscription` only evaluates snapshots in
  the re-evaluate payload.
- No scheduler job kind emits `advisor.evaluate_requested` yet;
  periodic evaluation needs a scheduler-side mapping.
- `cargo check` was never run (no Rust toolchain in this
  environment); compile verification is still open.
