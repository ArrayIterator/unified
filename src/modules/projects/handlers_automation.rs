// Projects module - automation rule and ticket bridge handlers.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use super::*;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse, web};
use serde_json::{Value, json};
use std::sync::Arc;

// Automation rules

pub(crate) async fn list_automation_rules(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let result: UnifiedResult<PageResult<ProjectAutomationRule>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let project = state.get_project(path.as_str())?;
        let (viewer_id, viewer_role) = viewer_of(&req);
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        let sort = params.allowed_sort(&["created_at", "updated_at", "name"], "created_at");
        let mut items: Vec<ProjectAutomationRule> = state
            .automation_rules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|rule| {
                rule.project_id == project.id
                    && matches_search(&rule.name, &params)
                    && params
                        .filters
                        .get("trigger")
                        .is_none_or(|trigger| rule.trigger == *trigger)
                    && params.filters.get("enabled").is_none_or(|flag| {
                        let wanted = flag == "true";
                        rule.enabled == wanted
                    })
            })
            .collect();
        match sort.as_str() {
            "updated_at" => items.sort_by(|a, b| a.updated_at.cmp(&b.updated_at)),
            "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

pub(crate) async fn create_automation_rule(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    body: web::Json<CreateAutomationRule>,
) -> HttpResponse {
    let result: UnifiedResult<ProjectAutomationRule> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let input = body.into_inner();
        let (viewer_id, viewer_role) = viewer_of(&req);
        let project = state.get_project(path.as_str())?;
        require_capability(&state, viewer_role.as_deref(), "projects:manage")?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        s4_require_not_archived(&project)?;
        let name = input.name.trim().to_string();
        if name.is_empty() {
            return Err(UnifiedError::bad_request("Rule name must not be empty"));
        }
        if !s3_valid_rule_trigger(&input.trigger) {
            return Err(UnifiedError::bad_request(
                "trigger must be one of: task_created, status_changed, task_completed, due_soon, label_added",
            ));
        }
        let conditions = if input.conditions.is_null() {
            json!({})
        } else {
            input.conditions
        };
        s3_validate_rule_actions(&project, &input.actions)?;
        let rule = ProjectAutomationRule {
            id: new_id(),
            project_id: project.id.clone(),
            name,
            trigger: input.trigger,
            conditions,
            actions: input.actions,
            enabled: input.enabled.unwrap_or(true),
            run_count: 0,
            last_run_at: None,
            created_at: now(),
            updated_at: now(),
        };
        state.automation_rules.insert(rule.id.clone(), rule.clone());
        let actor = viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project.id,
            actor,
            "automation_rule.created",
            "automation_rule",
            Some(rule.id.clone()),
            json!({ "name": rule.name, "trigger": rule.trigger }),
        );
        Ok(rule)
    })();
    match result {
        Ok(rule) => json::success_response(rule, 201),
        Err(error) => json::error_response(&error, false),
    }
}

pub(crate) async fn get_automation_rule(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<ProjectAutomationRule> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let (viewer_id, viewer_role) = viewer_of(&req);
        let (project_id, rule_id) = path.into_inner();
        let project = state.get_project(&project_id)?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        let rule = state
            .automation_rules
            .get(rule_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Automation rule not found"))?;
        if rule.project_id != project_id {
            return Err(UnifiedError::not_found("Automation rule not found"));
        }
        Ok(rule)
    })();
    json::respond(result, false)
}

pub(crate) async fn update_automation_rule(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateAutomationRule>,
) -> HttpResponse {
    let result: UnifiedResult<ProjectAutomationRule> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let input = body.into_inner();
        let (viewer_id, viewer_role) = viewer_of(&req);
        let (project_id, rule_id) = path.into_inner();
        let project = state.get_project(&project_id)?;
        require_capability(&state, viewer_role.as_deref(), "projects:manage")?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        s4_require_not_archived(&project)?;
        let existing = state
            .automation_rules
            .get(rule_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Automation rule not found"))?;
        if existing.project_id != project_id {
            return Err(UnifiedError::not_found("Automation rule not found"));
        }
        if let Some(name) = &input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request("Rule name must not be empty"));
            }
        }
        if let Some(trigger) = &input.trigger {
            if !s3_valid_rule_trigger(trigger) {
                return Err(UnifiedError::bad_request(
                    "trigger must be one of: task_created, status_changed, task_completed, due_soon, label_added",
                ));
            }
        }
        if let Some(actions) = &input.actions {
            s3_validate_rule_actions(&project, actions)?;
        }
        let mut rule = state
            .automation_rules
            .get_mut(rule_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Automation rule not found"))?;
        if let Some(name) = input.name {
            rule.name = name.trim().to_string();
        }
        if let Some(trigger) = input.trigger {
            rule.trigger = trigger;
        }
        if let Some(conditions) = input.conditions {
            rule.conditions = if conditions.is_null() {
                json!({})
            } else {
                conditions
            };
        }
        if let Some(actions) = input.actions {
            rule.actions = actions;
        }
        if let Some(enabled) = input.enabled {
            rule.enabled = enabled;
        }
        rule.updated_at = now();
        let updated = rule.clone();
        drop(rule);
        let actor = viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project.id,
            actor,
            "automation_rule.updated",
            "automation_rule",
            Some(rule_id.clone()),
            json!({ "name": updated.name, "trigger": updated.trigger }),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

pub(crate) async fn delete_automation_rule(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let (viewer_id, viewer_role) = viewer_of(&req);
        let (project_id, rule_id) = path.into_inner();
        let project = state.get_project(&project_id)?;
        require_capability(&state, viewer_role.as_deref(), "projects:manage")?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        s4_require_not_archived(&project)?;
        let rule = state
            .automation_rules
            .get(rule_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Automation rule not found"))?;
        if rule.project_id != project_id {
            return Err(UnifiedError::not_found("Automation rule not found"));
        }
        state.automation_rules.remove(rule_id.as_str());
        let actor = viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project.id,
            actor,
            "automation_rule.deleted",
            "automation_rule",
            Some(rule_id.clone()),
            json!({ "name": rule.name }),
        );
        Ok(json!({ "deleted": true, "id": rule_id }))
    })();
    json::respond(result, false)
}

pub(crate) async fn run_automation_rule(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let (viewer_id, viewer_role) = viewer_of(&req);
        let (project_id, rule_id) = path.into_inner();
        let project = state.get_project(&project_id)?;
        require_capability(&state, viewer_role.as_deref(), "projects:manage")?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        s4_require_not_archived(&project)?;
        let rule = state
            .automation_rules
            .get(rule_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Automation rule not found"))?;
        if rule.project_id != project_id {
            return Err(UnifiedError::not_found("Automation rule not found"));
        }
        // A manual run evaluates the rule against every task in the
        // project right now, whether or not the rule is enabled: the
        // explicit invocation is the trigger.
        let mut matched: u64 = 0;
        let mut applied: u64 = 0;
        let tasks: Vec<ProjectTask> = state
            .tasks
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|task| task.project_id == project.id)
            .collect();
        for mut task in tasks {
            if !s3_rule_conditions_match(&task, &rule.conditions) {
                continue;
            }
            matched += 1;
            let applied_actions = s3_apply_rule_actions(&state, &project, &mut task, &rule);
            if applied_actions > 0 {
                task.updated_at = now();
                state.tasks.insert(task.id.clone(), task);
                applied += 1;
            }
        }
        if let Some(mut stored) = state.automation_rules.get_mut(rule_id.as_str()) {
            stored.run_count += 1;
            stored.last_run_at = Some(now());
        }
        let actor = viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project.id,
            actor,
            "automation_rule.ran",
            "automation_rule",
            Some(rule_id.clone()),
            json!({ "matched": matched, "applied": applied }),
        );
        Ok(json!({
            "rule_id": rule_id,
            "matched": matched,
            "applied": applied,
        }))
    })();
    json::respond(result, false)
}

// Ticket bridge

pub(crate) async fn convert_task_to_ticket(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    body: web::Json<ConvertTaskToTicket>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let input = body.into_inner();
        let (viewer_id, viewer_role) = viewer_of(&req);
        let task = state.get_task(path.as_str())?;
        let project = state.get_project(&task.project_id)?;
        require_capability(&state, viewer_role.as_deref(), "projects:edit")?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        s4_require_not_archived(&project)?;
        state.emit(
            "ticket.create_requested",
            json!({
                "title": task.title,
                "body": task.description,
                "user_id": task.reporter_id,
                "category": "project-task",
                "department_id": input.department_id,
                "project_id": project.id,
                "task_id": task.id,
            }),
        );
        let actor = viewer_id
            .as_deref()
            .unwrap_or(task.reporter_id.as_str());
        state.log_activity(
            &project.id,
            actor,
            "task.converted_to_ticket",
            "task",
            Some(task.id.clone()),
            json!({ "department_id": input.department_id }),
        );
        Ok(json!({ "requested": true, "task_id": task.id }))
    })();
    json::respond(result, false)
}

pub(crate) async fn create_task_from_ticket(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    body: web::Json<TaskFromTicket>,
) -> HttpResponse {
    let result: UnifiedResult<ProjectTask> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let input = body.into_inner();
        let (viewer_id, viewer_role) = viewer_of(&req);
        let project = state.get_project(&input.project_id)?;
        require_capability(&state, viewer_role.as_deref(), "projects:edit")?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        s4_require_not_archived(&project)?;
        let title = input.title.trim().to_string();
        if title.is_empty() {
            return Err(UnifiedError::bad_request("Task title must not be empty"));
        }
        // The task reporter is the header caller: a body `user_id`
        // naming someone else is forgery unless the caller holds
        // `projects:manage` (admin/on-behalf flow).
        let user_id = resolve_body_actor(
            &state,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &input.user_id,
            "user_id",
        )?;
        let task = ProjectTask {
            id: new_id(),
            project_id: project.id.clone(),
            board_id: None,
            parent_id: None,
            epic_id: None,
            title,
            description: input.body.unwrap_or_default(),
            task_type: "task".to_string(),
            status: s3_default_status(&project),
            priority: "normal".to_string(),
            assignee_id: None,
            reporter_id: user_id,
            estimate_points: None,
            estimate_hours: None,
            labels: Vec::new(),
            custom_values: json!({}),
            start_date: None,
            due_date: None,
            completed_at: None,
            sprint_id: None,
            phase_id: None,
            cycle_id: None,
            pi_id: None,
            milestone_id: None,
            process_group: None,
            knowledge_area: None,
            waste_flag: None,
            pair_partner_id: None,
            test_first: false,
            dmaic_phase: None,
            appetite_weeks: None,
            created_at: now(),
            updated_at: now(),
        };
        state.tasks.insert(task.id.clone(), task.clone());
        apply_automation_rules(&state, &project, &task, "task_created");
        let stored = state.get_task(&task.id)?;
        let actor = viewer_id
            .as_deref()
            .unwrap_or(stored.reporter_id.as_str());
        state.log_activity(
            &project.id,
            actor,
            "task.created",
            "task",
            Some(stored.id.clone()),
            json!({
                "title": stored.title,
                "ticket_id": input.ticket_id,
                "department_id": input.department_id,
            }),
        );
        state.emit(
            "project.task.created",
            serde_json::to_value(&stored).unwrap_or(Value::Null),
        );
        Ok(stored)
    })();
    match result {
        Ok(task) => json::success_response(task, 201),
        Err(error) => json::error_response(&error, false),
    }
}

#[cfg(test)]
mod tests {

    /// Build a request carrying the server-verified caller the auth
    /// middleware would attach for this user and role. The old
    /// self-asserted identity headers grant nothing,
    /// so tests attach the verified caller to the request
    /// extensions instead.
    fn verified_request(user_id: &str, role: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid(user_id), Vec::new())
                .with_roles(vec![role.to_string()]),
        );
        req
    }

    /// Build a request carrying a verified caller with no roles
    /// resolved: the caller resolves, but holds the anonymous role.
    /// This is the "no role" case in the new model (roles are
    /// resolved server-side, never claimed in a header).
    fn verified_request_without_roles(user_id: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::auth::Caller::verified(
                test_user_uuid(user_id),
                Vec::new(),
            ));
        req
    }

    /// Deterministic test-user identity: distinct names map to
    /// distinct UUIDs (FNV-1a folded into the low bits), so
    /// ownership tests can still tell two users apart. A value
    /// that already parses as a UUID is used as-is.
    fn test_user_uuid(name: &str) -> uuid::Uuid {
        if let Ok(id) = uuid::Uuid::parse_str(name) {
            return id;
        }
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in name.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        uuid::Uuid::from_u128(u128::from(hash) | 0x0000_0000_0000_4000_8000_0000_0000_0000)
    }

    use super::*;
    use actix_web::http::StatusCode;
    use actix_web::test::TestRequest;

    /// A state whose `role.check` hook allows exactly the given
    /// permissions (all others denied).
    fn allowing_state(allowed: &[&str]) -> Arc<ProjectsState> {
        use unified_core::hooks::HookContext;
        let state = Arc::new(ProjectsState::new());
        let allowed: Vec<String> = allowed
            .iter()
            .map(|permission| permission.to_string())
            .collect();
        state.bus().register(
            "role.check",
            "test",
            Arc::new(move |ctx: &mut HookContext| {
                let permission = ctx
                    .get("permission")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                ctx.set(
                    "allowed",
                    json!(allowed.iter().any(|held| held == permission)),
                );
            }),
        );
        state
    }

    fn seed_project(state: &ProjectsState) {
        state.projects.insert(
            "p1".to_string(),
            Project {
                id: "p1".to_string(),
                name: "Test".to_string(),
                description: String::new(),
                methodology: "scrum".to_string(),
                status: "active".to_string(),
                visibility: "private".to_string(),
                owner_id: "user-1".to_string(),
                company_id: None,
                client_id: None,
                start_date: None,
                due_date: None,
                color: None,
                icon: None,
                labels: Vec::new(),
                custom_field_definitions: json!({}),
                archived: false,
                created_at: now(),
                updated_at: now(),
            },
        );
    }

    fn caller_req(role: &str) -> HttpRequest {
        verified_request("user-1", role)
    }

    fn ticket_body(user_id: &str) -> web::Json<TaskFromTicket> {
        web::Json(TaskFromTicket {
            ticket_id: "tick-1".to_string(),
            title: "From ticket".to_string(),
            body: None,
            user_id: user_id.to_string(),
            project_id: "p1".to_string(),
            department_id: None,
        })
    }

    #[tokio::test]
    async fn create_task_from_ticket_rejects_forged_reporter() {
        let state = allowing_state(&["projects:edit"]);
        seed_project(&state);
        let resp = create_task_from_ticket(
            caller_req("member"),
            web::Data::new(state.clone()),
            ticket_body("someone-else"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        assert!(state.tasks.is_empty());
    }

    #[tokio::test]
    async fn create_task_from_ticket_records_the_caller() {
        let state = allowing_state(&["projects:edit"]);
        seed_project(&state);
        let resp = create_task_from_ticket(
            caller_req("member"),
            web::Data::new(state.clone()),
            ticket_body(&test_user_uuid("user-1").to_string()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        let stored = state
            .tasks
            .iter()
            .next()
            .expect("task stored")
            .value()
            .clone();
        assert_eq!(stored.reporter_id, test_user_uuid("user-1").to_string());
    }

    #[tokio::test]
    async fn create_task_from_ticket_allows_manage_on_behalf() {
        let state = allowing_state(&["projects:edit", "projects:manage"]);
        seed_project(&state);
        let resp = create_task_from_ticket(
            caller_req("admin"),
            web::Data::new(state.clone()),
            ticket_body("someone-else"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        let stored = state
            .tasks
            .iter()
            .next()
            .expect("task stored")
            .value()
            .clone();
        assert_eq!(stored.reporter_id, "someone-else");
    }
}
