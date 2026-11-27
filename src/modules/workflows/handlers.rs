// Workflows module - HTTP handlers.
//
// Split from `src/modules/workflows.rs`; the module root re-exports the public API of this module.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use super::constants::{MAX_DESCRIPTION_LEN, MAX_NAME_LEN, PERM_WORKFLOWS_MANAGE, PERM_WORKFLOWS_VIEW};
use super::engine::{render_config_value};
use super::runs::{RunStart, begin_run, evaluate_conditions, finish_run_async, finish_run_sync, schedule_workflow, subscribe_workflow, unsubscribe_workflow};
use super::state::{WorkflowsState, caller, caller_company, created, err, in_company_scope, load_run, load_workflow_in_scope, ok, paginate, require_company_match, require_permission, require_record_in_company_scope, require_text, require_tenant, steps_for_run};
use super::types::{CreateWorkflowRequest, RetryRunRequest, RunStatus, RunWorkflowRequest, TestWorkflowRequest, TriggerKind, UpdateWorkflowRequest, Workflow, WorkflowRun};
use super::validation::{generate_webhook_token, hashes_equal, token_hash, validate_actions, validate_allowed_hosts, validate_conditions, validate_no_self_trigger_event, validate_trigger};

// Workflow handlers

pub(crate) async fn list_workflows(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WorkflowsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WORKFLOWS_VIEW)?;
        let scope = caller_company(&_tenant, &req)?;
        let params = query.into_inner();
        let active_filter = params.filters.get("is_active").cloned();
        let trigger_filter = params.filters.get("trigger_kind").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Workflow> = state
            .workflows
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|workflow| {
                // Company scope first: a scoped caller sees only
                // their own company's workflows; an unresolved
                // caller sees global workflows only.
                if !in_company_scope(&workflow.company_id, &scope) {
                    return false;
                }
                if let Some(active) = &active_filter {
                    let expected = match active.as_str() {
                        "true" => true,
                        "false" => false,
                        _ => return false,
                    };
                    if workflow.is_active != expected {
                        return false;
                    }
                }
                if let Some(kind) = &trigger_filter {
                    if workflow.trigger.kind.as_str() != kind.as_str() {
                        return false;
                    }
                }
                if let Some(company_id) = &company_filter {
                    if workflow.company_id.as_deref() != Some(company_id.as_str()) {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {}",
                        workflow.name,
                        workflow.description.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|workflow| workflow.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_workflow(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WorkflowsState>,
    req: HttpRequest,
    body: web::Json<CreateWorkflowRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WORKFLOWS_MANAGE)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let description = match &input.description {
            Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
            None => None,
        };
        validate_trigger(&input.trigger)?;
        validate_conditions(&input.conditions)?;
        validate_actions(&input.actions, None)?;
        validate_no_self_trigger_event(&input.trigger, &input.actions)?;
        validate_allowed_hosts(&input.webhook_allowed_hosts)?;
        let company_id = match &input.company_id {
            Some(raw) => Some(require_text(raw, "company_id", 128)?),
            None => None,
        };
        // An omitted body company adopts the caller scope, so a
        // workflow is never stored unscoped under a scoped caller;
        // an unresolved caller may not name a company.
        let company_id = require_company_match(&_tenant, &req, &company_id)?;
        let now = Utc::now();
        let mut workflow = Workflow {
            id: Uuid::now_v7(),
            name,
            description,
            trigger: input.trigger,
            conditions: input.conditions,
            actions: input.actions,
            is_active: input.is_active,
            owner_id: caller.actor.clone(),
            company_id,
            webhook_token_hash: None,
            webhook_allowed_hosts: input.webhook_allowed_hosts,
            run_count: 0,
            success_count: 0,
            failure_count: 0,
            dropped_count: 0,
            last_run_at: None,
            last_run_status: None,
            scheduler_status: "not_scheduled".to_string(),
            scheduler_job_id: None,
            created_at: now,
            updated_at: now,
        };
        // A webhook trigger gets its token now. The raw token is
        // returned once in the response and never stored.
        let mut webhook_token: Option<String> = None;
        if workflow.trigger.kind == TriggerKind::Webhook {
            let (token, hash) = generate_webhook_token();
            workflow.webhook_token_hash = Some(hash.clone());
            state.webhook_tokens.insert(hash, workflow.id);
            webhook_token = Some(token);
        }
        let state_arc = Arc::new(state.as_ref().clone());
        schedule_workflow(&state_arc, &mut workflow);
        state.workflows.insert(workflow.id, workflow.clone());
        subscribe_workflow(&state_arc, &workflow);
        state.emit(
            "workflows.created",
            json!({
                "workflow_id": workflow.id.to_string(),
                "name": workflow.name,
                "trigger_kind": workflow.trigger.kind.as_str(),
                "owner_id": workflow.owner_id,
                "company_id": workflow.company_id,
            }),
        );
        let mut data = json!({ "workflow": workflow });
        if let Some(token) = webhook_token {
            data["webhook_token"] = Value::String(token.clone());
            data["webhook_url"] = Value::String(format!("/api/v1/workflows/hooks/{token}"));
        }
        Ok(data)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_workflow(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WorkflowsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WORKFLOWS_VIEW)?;
        let scope = caller_company(&_tenant, &req)?;
        let workflow = load_workflow_in_scope(&state, &path.into_inner(), &scope)?;
        let runs: Vec<WorkflowRun> = state
            .runs
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|run| run.workflow_id == workflow.id)
            .collect();
        Ok(json!({
            "workflow": workflow,
            "has_webhook_token": workflow.webhook_token_hash.is_some(),
            "subscription_count": state.subscriptions.get(&workflow.id).map(|entry| entry.len()).unwrap_or(0),
            "recorded_runs": runs.len(),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_workflow(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WorkflowsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateWorkflowRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WORKFLOWS_MANAGE)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let mut workflow = load_workflow_in_scope(&state, &path.into_inner(), &scope)?;
        let input = body.into_inner();
        let state_arc = Arc::new(state.as_ref().clone());
        if let Some(name) = input.name {
            workflow.name = require_text(&name, "name", MAX_NAME_LEN)?;
        }
        if let Some(description) = input.description {
            workflow.description = match description {
                Some(raw) => Some(require_text(&raw, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(trigger) = input.trigger {
            validate_trigger(&trigger)?;
            workflow.trigger = trigger;
        }
        if let Some(conditions) = input.conditions {
            validate_conditions(&conditions)?;
            workflow.conditions = conditions;
        }
        if let Some(actions) = input.actions {
            validate_actions(&actions, Some(workflow.id))?;
            workflow.actions = actions;
        }
        if let Some(is_active) = input.is_active {
            workflow.is_active = is_active;
        }
        if let Some(company_id) = input.company_id {
            let next = match company_id {
                Some(raw) => Some(require_text(&raw, "company_id", 128)?),
                None => None,
            };
            // `Some(None)` is an explicit clear and stays a clear;
            // a newly named company must equal the caller scope.
            workflow.company_id = match next {
                Some(value) => require_company_match(&_tenant, &req, &Some(value))?,
                None => None,
            };
        }
        if let Some(hosts) = input.webhook_allowed_hosts {
            validate_allowed_hosts(&hosts)?;
            workflow.webhook_allowed_hosts = hosts;
        }
        // The merged trigger + actions must not form a direct
        // self-loop (a PATCH that changes only the trigger or only
        // the actions can create one).
        validate_no_self_trigger_event(&workflow.trigger, &workflow.actions)?;
        // Token rotation: the old token stops working immediately.
        let mut webhook_token: Option<String> = None;
        if input.rotate_webhook_token {
            if workflow.trigger.kind != TriggerKind::Webhook {
                return Err(UnifiedError::bad_request(
                    "rotate_webhook_token only applies to a workflow with a webhook trigger",
                ));
            }
            if let Some(old_hash) = workflow.webhook_token_hash.take() {
                state.webhook_tokens.remove(&old_hash);
            }
            let (token, hash) = generate_webhook_token();
            workflow.webhook_token_hash = Some(hash.clone());
            state.webhook_tokens.insert(hash, workflow.id);
            webhook_token = Some(token);
        }
        // A webhook trigger that never had a token (for example a
        // workflow whose trigger was just changed to webhook) gets
        // one now.
        if workflow.trigger.kind == TriggerKind::Webhook
            && workflow.webhook_token_hash.is_none()
        {
            let (token, hash) = generate_webhook_token();
            workflow.webhook_token_hash = Some(hash.clone());
            state.webhook_tokens.insert(hash, workflow.id);
            webhook_token = Some(token);
        }
        // A trigger that is no longer a webhook drops its token.
        if workflow.trigger.kind != TriggerKind::Webhook {
            if let Some(old_hash) = workflow.webhook_token_hash.take() {
                state.webhook_tokens.remove(&old_hash);
            }
        }
        workflow.updated_at = Utc::now();
        schedule_workflow(&state_arc, &mut workflow);
        state.workflows.insert(workflow.id, workflow.clone());
        // Re-subscribe from a clean slate (all validation already
        // passed): the old handler is removed first so a stale
        // handler can never fire the workflow twice, and a
        // deactivated workflow keeps no subscription behind.
        unsubscribe_workflow(&state_arc, workflow.id);
        if workflow.is_active {
            subscribe_workflow(&state_arc, &workflow);
        }
        state.emit(
            "workflows.updated",
            json!({
                "workflow_id": workflow.id.to_string(),
                "name": workflow.name,
                "trigger_kind": workflow.trigger.kind.as_str(),
                "is_active": workflow.is_active,
            }),
        );
        let mut data = json!({ "workflow": workflow });
        if let Some(token) = webhook_token {
            data["webhook_token"] = Value::String(token.clone());
            data["webhook_url"] = Value::String(format!("/api/v1/workflows/hooks/{token}"));
        }
        Ok(data)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_workflow(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WorkflowsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WORKFLOWS_MANAGE)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let workflow = load_workflow_in_scope(&state, &path.into_inner(), &scope)?;
        let state_arc = Arc::new(state.as_ref().clone());
        unsubscribe_workflow(&state_arc, workflow.id);
        if let Some(hash) = &workflow.webhook_token_hash {
            state.webhook_tokens.remove(hash);
        }
        let run_ids: Vec<Uuid> = state
            .runs
            .iter()
            .filter(|entry| entry.value().workflow_id == workflow.id)
            .map(|entry| *entry.key())
            .collect();
        for run_id in &run_ids {
            state.runs.remove(run_id);
        }
        let step_ids: Vec<Uuid> = state
            .steps
            .iter()
            .filter(|entry| entry.value().workflow_id == workflow.id)
            .map(|entry| *entry.key())
            .collect();
        for step_id in &step_ids {
            state.steps.remove(step_id);
        }
        state.run_starts.remove(&workflow.id);
        state.workflows.remove(&workflow.id);
        state.emit(
            "workflows.deleted",
            json!({
                "workflow_id": workflow.id.to_string(),
                "name": workflow.name,
                "runs_removed": run_ids.len(),
                "steps_removed": step_ids.len(),
            }),
        );
        Ok(json!({ "deleted": true, "workflow_id": workflow.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn enable_workflow(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WorkflowsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Workflow> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WORKFLOWS_MANAGE)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let mut workflow = load_workflow_in_scope(&state, &path.into_inner(), &scope)?;
        workflow.is_active = true;
        workflow.updated_at = Utc::now();
        let state_arc = Arc::new(state.as_ref().clone());
        schedule_workflow(&state_arc, &mut workflow);
        state.workflows.insert(workflow.id, workflow.clone());
        // Idempotent: a repeated enable must never double-subscribe.
        unsubscribe_workflow(&state_arc, workflow.id);
        subscribe_workflow(&state_arc, &workflow);
        state.emit(
            "workflows.updated",
            json!({
                "workflow_id": workflow.id.to_string(),
                "is_active": true,
            }),
        );
        Ok(workflow)
    })();
    match result {
        Ok(workflow) => ok(workflow),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn disable_workflow(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WorkflowsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Workflow> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WORKFLOWS_MANAGE)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let mut workflow = load_workflow_in_scope(&state, &path.into_inner(), &scope)?;
        workflow.is_active = false;
        workflow.updated_at = Utc::now();
        let state_arc = Arc::new(state.as_ref().clone());
        unsubscribe_workflow(&state_arc, workflow.id);
        state.workflows.insert(workflow.id, workflow.clone());
        state.emit(
            "workflows.updated",
            json!({
                "workflow_id": workflow.id.to_string(),
                "is_active": false,
            }),
        );
        Ok(workflow)
    })();
    match result {
        Ok(workflow) => ok(workflow),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn duplicate_workflow(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WorkflowsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WORKFLOWS_MANAGE)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let source = load_workflow_in_scope(&state, &path.into_inner(), &scope)?;
        let now = Utc::now();
        let mut copy = source.clone();
        copy.id = Uuid::now_v7();
        copy.name = format!("{} (copy)", source.name);
        // A copy starts disabled: it must never double-fire the
        // original's event trigger before anyone looks at it.
        copy.is_active = false;
        copy.owner_id = caller.actor.clone();
        copy.run_count = 0;
        copy.success_count = 0;
        copy.failure_count = 0;
        copy.dropped_count = 0;
        copy.last_run_at = None;
        copy.last_run_status = None;
        copy.scheduler_status = "not_scheduled".to_string();
        copy.scheduler_job_id = None;
        copy.webhook_token_hash = None;
        copy.created_at = now;
        copy.updated_at = now;
        let mut webhook_token: Option<String> = None;
        if copy.trigger.kind == TriggerKind::Webhook {
            let (token, hash) = generate_webhook_token();
            copy.webhook_token_hash = Some(hash.clone());
            state.webhook_tokens.insert(hash, copy.id);
            webhook_token = Some(token);
        }
        state.workflows.insert(copy.id, copy.clone());
        state.emit(
            "workflows.created",
            json!({
                "workflow_id": copy.id.to_string(),
                "name": copy.name,
                "duplicated_from": source.id.to_string(),
                "owner_id": copy.owner_id,
            }),
        );
        let mut data = json!({ "workflow": copy });
        if let Some(token) = webhook_token {
            data["webhook_token"] = Value::String(token.clone());
            data["webhook_url"] = Value::String(format!("/api/v1/workflows/hooks/{token}"));
        }
        Ok(data)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

// Run handlers

pub(crate) async fn run_workflow(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WorkflowsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RunWorkflowRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, debug),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_WORKFLOWS_MANAGE) {
        return err(error, debug);
    }
    let scope = match require_company_for_write(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err(error, debug),
    };
    let workflow = match load_workflow_in_scope(&state, &path.into_inner(), &scope) {
        Ok(workflow) => workflow,
        Err(error) => return err(error, debug),
    };
    let input = body.into_inner();
    if !input.dry_run && !workflow.is_active {
        return err(
            UnifiedError::conflict(
                "This workflow is disabled; enable it before running it (a dry run is still allowed)",
            ),
            debug,
        );
    }
    if !input.payload.is_null() && !input.payload.is_object() && !input.payload.is_array() {
        return err(
            UnifiedError::bad_request("payload must be a JSON object or array"),
            debug,
        );
    }
    let payload = if input.payload.is_null() {
        json!({})
    } else {
        input.payload
    };
    let state_arc = Arc::new(state.as_ref().clone());
    match begin_run(
        &state_arc,
        &workflow,
        "manual",
        Some("manual"),
        &payload,
        input.dry_run,
        0,
    ) {
        RunStart::Dropped(dropped) => ok(json!({
            "run": dropped,
            "steps": [],
            "dropped": true,
            "message": "Rate limit exceeded: at most 60 runs per workflow per minute; this run was dropped.",
        })),
        RunStart::Started(run) => {
            let run_id = run.id;
            let finished = finish_run_async(&state_arc, run, workflow, payload, 0).await;
            let steps = steps_for_run(&state, run_id);
            ok(json!({ "run": finished, "steps": steps }))
        }
    }
}

pub(crate) async fn test_workflow(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WorkflowsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<TestWorkflowRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WORKFLOWS_MANAGE)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let workflow = load_workflow_in_scope(&state, &path.into_inner(), &scope)?;
        let input = body.into_inner();
        let payload = if input.payload.is_null() {
            json!({})
        } else {
            input.payload
        };
        let (matched, leaves) = evaluate_conditions(&workflow, &payload);
        let planned: Vec<Value> = workflow
            .actions
            .iter()
            .enumerate()
            .map(|(index, action)| {
                let mut rendered = render_config_value(&action.config, &payload, &workflow);
                // Webhook header values can carry credentials: show
                // the header names, never the values.
                if action.kind == "call_webhook" {
                    if let Some(headers) = rendered
                        .get_mut("headers")
                        .and_then(Value::as_object_mut)
                    {
                        for value in headers.values_mut() {
                            *value = Value::String("[redacted]".to_string());
                        }
                    }
                }
                json!({
                    "step_index": index,
                    "action_kind": action.kind,
                    "config": rendered,
                    "planned": true,
                })
            })
            .collect();
        Ok(json!({
            "workflow_id": workflow.id.to_string(),
            "condition_matched": matched,
            "conditions": leaves,
            "would_execute": matched,
            "planned_actions": planned,
            "message": if matched {
                "Conditions match. The planned actions above would execute on a real run; nothing was executed."
            } else {
                "Conditions did not match. No action would execute on a real run."
            },
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_workflow_runs(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WorkflowsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WORKFLOWS_VIEW)?;
        let scope = caller_company(&_tenant, &req)?;
        let workflow = load_workflow_in_scope(&state, &path.into_inner(), &scope)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let trigger_filter = params.filters.get("trigger_kind").cloned();
        let dry_run_filter = params.filters.get("dry_run").cloned();
        let mut items: Vec<WorkflowRun> = state
            .runs
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|run| {
                if run.workflow_id != workflow.id {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if run.status.as_str() != status.as_str() {
                        return false;
                    }
                }
                if let Some(trigger) = &trigger_filter {
                    if &run.trigger_kind != trigger {
                        return false;
                    }
                }
                if let Some(dry_run) = &dry_run_filter {
                    let expected = match dry_run.as_str() {
                        "true" => true,
                        "false" => false,
                        _ => return false,
                    };
                    if run.dry_run != expected {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|run| std::cmp::Reverse(run.started_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_workflow_run(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WorkflowsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WORKFLOWS_VIEW)?;
        let scope = caller_company(&_tenant, &req)?;
        let run = load_run(&state, &path.into_inner())?;
        // Runs carry no company of their own: they are scoped
        // through their parent workflow. A run whose workflow is
        // gone is orphaned and no longer resolvable.
        let workflow = state
            .workflows
            .get(&run.workflow_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!("Workflow not found: {}", run.workflow_id))
            })?;
        require_record_in_company_scope(&workflow.company_id, &scope)?;
        let steps = steps_for_run(&state, run.id);
        Ok(json!({
            "run": run,
            "workflow": workflow,
            "steps": steps,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn retry_workflow_run(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WorkflowsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RetryRunRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, debug),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_WORKFLOWS_MANAGE) {
        return err(error, debug);
    }
    let scope = match require_company_for_write(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err(error, debug),
    };
    let source_run = match load_run(&state, &path.into_inner()) {
        Ok(run) => run,
        Err(error) => return err(error, debug),
    };
    if source_run.status != RunStatus::Failed {
        return err(
            UnifiedError::conflict(format!(
                "Only a failed run can be retried (this run is \"{}\")",
                source_run.status.as_str()
            )),
            debug,
        );
    }
    let workflow = match state
        .workflows
        .get(&source_run.workflow_id)
        .map(|entry| entry.value().clone())
    {
        Some(workflow) => workflow,
        None => {
            return err(
                UnifiedError::not_found(format!(
                    "Workflow not found: {}",
                    source_run.workflow_id
                )),
                debug,
            )
        }
    };
    // Runs carry no company of their own: the retry is scoped
    // through the parent workflow.
    if let Err(error) = require_record_in_company_scope(&workflow.company_id, &scope) {
        return err(error, debug);
    }
    if !workflow.is_active {
        return err(
            UnifiedError::conflict("This workflow is disabled; enable it before retrying a run"),
            debug,
        );
    }
    // The original payload is not retained (only a redacted
    // summary is stored), so the retry uses the caller-supplied
    // payload, or an empty object when none is given.
    let input = body.into_inner();
    let payload = if input.payload.is_null() {
        json!({})
    } else {
        input.payload
    };
    let state_arc = Arc::new(state.as_ref().clone());
    match begin_run(
        &state_arc,
        &workflow,
        &source_run.trigger_kind,
        source_run.trigger_name.as_deref(),
        &payload,
        false,
        0,
    ) {
        RunStart::Dropped(dropped) => ok(json!({
            "run": dropped,
            "steps": [],
            "dropped": true,
            "message": "Rate limit exceeded: at most 60 runs per workflow per minute; this retry was dropped.",
        })),
        RunStart::Started(mut run) => {
            run.retried_from_run_id = Some(source_run.id);
            state.runs.insert(run.id, run.clone());
            let run_id = run.id;
            let finished = finish_run_async(&state_arc, run, workflow, payload, 0).await;
            let steps = steps_for_run(&state, run_id);
            ok(json!({ "run": finished, "steps": steps }))
        }
    }
}

pub(crate) async fn list_trigger_subscriptions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WorkflowsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WORKFLOWS_VIEW)?;
        let scope = caller_company(&_tenant, &req)?;
        let params = query.into_inner();
        let mut by_event: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for entry in state.subscriptions.iter() {
            // Subscriptions are keyed by workflow: only in-scope
            // workflows contribute their events, so one company's
            // subscriptions never leak into another's listing.
            let workflow_id = *entry.key();
            let Some(workflow) = state
                .workflows
                .get(&workflow_id)
                .map(|entry| entry.value().clone())
            else {
                continue;
            };
            if !in_company_scope(&workflow.company_id, &scope) {
                continue;
            }
            let workflow_id = workflow_id.to_string();
            for (event_name, _) in entry.value().iter() {
                by_event
                    .entry(event_name.clone())
                    .or_default()
                    .push(workflow_id.clone());
            }
        }
        let mut items: Vec<Value> = by_event
            .into_iter()
            .map(|(event, workflow_ids)| {
                json!({ "event": event, "workflow_ids": workflow_ids })
            })
            .collect();
        items.sort_by(|a, b| {
            a.get("event")
                .and_then(Value::as_str)
                .cmp(&b.get("event").and_then(Value::as_str))
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// Webhook trigger (token-authenticated; no capability check - see
// the public-access note in the module header)

pub(crate) async fn webhook_trigger(
    state: web::Data<WorkflowsState>,
    path: web::Path<String>,
    body: Option<web::Json<Value>>,
) -> HttpResponse {
    let token = path.into_inner();
    if token.len() != 64 || !token.chars().all(|c| c.is_ascii_hexdigit()) {
        return err(UnifiedError::unauthorized("Invalid webhook token"), false);
    }
    let presented_hash = token_hash(&token);
    // Constant-time compare against every stored hash: the lookup
    // itself must not leak which token exists.
    let workflow_id = state
        .webhook_tokens
        .iter()
        .find(|entry| hashes_equal(entry.key(), &presented_hash))
        .map(|entry| *entry.value());
    let Some(workflow_id) = workflow_id else {
        return err(UnifiedError::unauthorized("Invalid webhook token"), false);
    };
    let Some(workflow) = state
        .workflows
        .get(&workflow_id)
        .map(|entry| entry.value().clone())
    else {
        return err(UnifiedError::unauthorized("Invalid webhook token"), false);
    };
    if !workflow.is_active || workflow.trigger.kind != TriggerKind::Webhook {
        return err(
            UnifiedError::conflict("This workflow is disabled or no longer has a webhook trigger"),
            false,
        );
    }
    let payload = body.map(|inner| inner.into_inner()).unwrap_or(Value::Null);
    let payload = if payload.is_null() {
        json!({})
    } else {
        payload
    };
    let state_arc = Arc::new(state.as_ref().clone());
    match begin_run(
        &state_arc,
        &workflow,
        "webhook",
        Some("webhook"),
        &payload,
        false,
        0,
    ) {
        RunStart::Dropped(dropped) => ok(json!({
            "run_id": dropped.id.to_string(),
            "status": "dropped",
            "message": "Rate limit exceeded: at most 60 runs per workflow per minute; this run was dropped.",
        })),
        RunStart::Started(run) => {
            let run_id = run.id;
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let task_state = Arc::clone(&state_arc);
                handle.spawn(async move {
                    finish_run_async(&task_state, run, workflow, payload, 0).await;
                });
                // 202: the run continues in the background.
                json::success_response(
                    json!({ "run_id": run_id.to_string(), "status": "running" }),
                    202,
                )
            } else {
                let finished = finish_run_sync(&state_arc, run, workflow, payload, 0);
                ok(json!({ "run_id": finished.id.to_string(), "status": finished.status.as_str() }))
            }
        }
    }
}

/// Resolve the caller's company for a WRITE (C3, strict by
/// default): a write requires a resolved company scope, so an
/// unresolved caller is refused with 403 instead of silently
/// writing a global (unscoped) record. Mutation handlers call
/// this instead of `caller_company`; read handlers keep
/// `caller_company`, and the named carve-outs (self-service
/// writes, settings with no company field, scan/test triggers,
/// platform surfaces) keep it too.
pub(crate) fn require_company_for_write(
    tenant: &unified_core::tenancy::TenantContext,
    req: &actix_web::HttpRequest,
) -> UnifiedResult<Option<String>> {
    match super::state::caller_company(tenant, req)? {
        Some(company) => Ok(Some(company)),
        None => Err(UnifiedError::forbidden(
            "A write requires a resolved company scope",
        )),
    }
}
