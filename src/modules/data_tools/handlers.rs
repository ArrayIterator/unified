// data_tools - HTTP handlers: rules, scan, run, run history, and
// the admin page.
//
// Auth follows the documents / scheduler shape: the caller is
// identified by the verified headers and every route asks the roles
// module through the `role.check` hook (fail closed). Reads need
// `data_tools:read`; rule changes, scans, runs, and the admin page
// need `data_tools:admin`. Rules are platform control-plane objects,
// so there is no per-tenant scoping on these routes (same as the
// Scheduler's jobs); tenant-scoped rules are follow-up work.

use super::validation::{authorize, escape_html, paginate};
use super::*;
use actix_web::{HttpRequest, HttpResponse, web};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::json;
use unified_core::pagination::PaginationParams;

// Handlers - rules

pub(crate) async fn list_rules(
    state: web::Data<Arc<DataToolsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_READ) {
        return json::error_response(&error, debug);
    }
    json::success_response(paginate(state.list_rules(), &query.into_inner()), 200)
}

pub(crate) async fn update_rule(
    state: web::Data<Arc<DataToolsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RulePatch>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_ADMIN) {
        return json::error_response(&error, debug);
    }
    match state.update_rule(&path.into_inner(), &body.into_inner()) {
        Ok(rule) => json::success_response(rule, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

// Handlers - scan / run / runs

/// Dry-run preview: counts per target, zero deletes, guaranteed.
pub(crate) async fn scan_now(
    state: web::Data<Arc<DataToolsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_ADMIN) {
        return json::error_response(&error, debug);
    }
    json::success_response(state.scan(), 200)
}

pub(crate) async fn run_now(
    state: web::Data<Arc<DataToolsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: Option<web::Json<RunRequest>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_ADMIN) {
        return json::error_response(&error, debug);
    }
    let dry_run = body.map(|body| body.into_inner().dry_run).unwrap_or(None);
    match state.run_cleanup("manual", dry_run) {
        Ok(run) => json::success_response(run, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn list_runs(
    state: web::Data<Arc<DataToolsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_READ) {
        return json::error_response(&error, debug);
    }
    json::success_response(paginate(state.list_runs(), &query.into_inner()), 200)
}

// Handler - admin page (HTML)

/// The Data Tools admin page: rules table, scan now, run, and run
/// history. Served from this module (not the dashboards section
/// allowlist - wiring `/admin/data-tools` into the dashboards module
/// is follow-up work for its owner; the Handlebars template for that
/// integration ships at `templates/default/data-tools/`).
/// The rules table is server-rendered (values escaped) so the page is
/// readable without JavaScript; the actions call the JSON endpoints
/// above and rebuild the tables from the responses (all values
/// inserted with `textContent`, never `innerHTML`).
pub(crate) async fn admin_page(
    state: web::Data<Arc<DataToolsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_ADMIN) {
        return json::error_response(&error, debug);
    }
    let mut rows = String::new();
    for rule in state.list_rules() {
        let checked = |flag: bool| if flag { "checked" } else { "" };
        rows.push_str(&format!(
            "<tr data-rule-id=\"{}\"><td>{}</td><td>{}</td>\
             <td><input type=\"number\" min=\"0\" max=\"3650\" value=\"{}\" data-field=\"retention_days\" aria-label=\"Retention days\"></td>\
             <td><input type=\"checkbox\" {} data-field=\"enabled\" aria-label=\"Enabled\"></td>\
             <td><input type=\"checkbox\" {} data-field=\"dry_run_default\" aria-label=\"Dry run by default\"></td>\
             <td><button type=\"button\" data-action=\"save-rule\">Save</button></td></tr>\n",
            escape_html(&rule.id),
            escape_html(&rule.target),
            escape_html(&rule.description),
            rule.retention_days,
            checked(rule.enabled),
            checked(rule.dry_run_default),
        ));
    }
    let html = format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<title>Data Tools - Admin</title>\n<style>\nbody{{font-family:system-ui,sans-serif;margin:2rem auto;max-width:960px;padding:0 1rem;color:#111}}\ntable{{border-collapse:collapse;width:100%;margin:1rem 0}}\nth,td{{border:1px solid #ccc;padding:.4rem .6rem;text-align:left}}\ninput[type=number]{{width:5rem}}\nbutton{{padding:.4rem .8rem;margin:.2rem;cursor:pointer}}\n#status{{margin:1rem 0;font-weight:600}}\n.note{{color:#555;font-size:.9rem}}\n</style>\n</head>\n<body>\n<h1>Data Tools</h1>\n<p class=\"note\">Orphaned-data resolver, expired-data purging, and cleanup rules. \
A scan is always a dry run: it counts, it never deletes. A run deletes only what the rules allow; \
the cross-check target only ever reports. Every run is recorded below.</p>\n<h2>Cleanup rules</h2>\n<table>\n<thead><tr><th>Target</th><th>Description</th><th>Retention (days)</th><th>Enabled</th><th>Dry run by default</th><th></th></tr></thead>\n<tbody id=\"rules\">\n{rows}</tbody>\n</table>\n<h2>Scan &amp; run</h2>\n<button type=\"button\" id=\"scan-now\">Scan now (dry run)</button>\n<button type=\"button\" id=\"run-now\">Run cleanup</button>\n<label><input type=\"checkbox\" id=\"run-dry\"> dry run</label>\n<p id=\"status\" role=\"status\"></p>\n<h2>Run history</h2>\n<table>\n<thead><tr><th>Started</th><th>Finished</th><th>Dry run</th><th>Status</th><th>Triggered by</th><th>Counts</th></tr></thead>\n<tbody id=\"runs\"></tbody>\n</table>\n<script>\nconst statusEl = document.getElementById('status');\nfunction setStatus(text) {{ statusEl.textContent = text; }}\nfunction rowCell(text) {{ const td = document.createElement('td'); td.textContent = text; return td; }}\nfunction outcomeText(outcomes) {{\n  return (outcomes || []).map(o => o.target + ': ' + o.counted + ' counted, ' + o.removed + ' removed' + (o.note ? ' (' + o.note + ')' : '')).join('; ');\n}}\nasync function loadRuns() {{\n  const res = await fetch('/api/v1/data-tools/runs?per_page=20');\n  if (!res.ok) {{ setStatus('Could not load run history.'); return; }}\n  const body = await res.json();\n  const tbody = document.getElementById('runs');\n  tbody.replaceChildren();\n  for (const run of (body.data && body.data.items) || []) {{\n    const tr = document.createElement('tr');\n    tr.append(rowCell(run.started_at || ''), rowCell(run.finished_at || ''), rowCell(String(run.dry_run)), rowCell(run.status || ''), rowCell(run.triggered_by || ''), rowCell(outcomeText(run.counts)));\n    tbody.append(tr);\n  }}\n}}\nasync function loadRules() {{\n  const res = await fetch('/api/v1/data-tools/rules');\n  if (!res.ok) {{ setStatus('Could not load rules.'); return; }}\n  const body = await res.json();\n  const tbody = document.getElementById('rules');\n  tbody.replaceChildren();\n  for (const rule of (body.data && body.data.items) || []) {{\n    const tr = document.createElement('tr');\n    tr.dataset.ruleId = rule.id;\n    tr.append(rowCell(rule.target), rowCell(rule.description));\n    const retentionTd = document.createElement('td');\n    const retention = document.createElement('input');\n    retention.type = 'number'; retention.min = '0'; retention.max = '3650';\n    retention.value = String(rule.retention_days); retention.dataset.field = 'retention_days';\n    retentionTd.append(retention); tr.append(retentionTd);\n    for (const field of ['enabled', 'dry_run_default']) {{\n      const td = document.createElement('td');\n      const box = document.createElement('input');\n      box.type = 'checkbox'; box.checked = !!rule[field]; box.dataset.field = field;\n      td.append(box); tr.append(td);\n    }}\n    const actionTd = document.createElement('td');\n    const save = document.createElement('button');\n    save.type = 'button'; save.textContent = 'Save'; save.dataset.action = 'save-rule';\n    actionTd.append(save); tr.append(actionTd);\n    tbody.append(tr);\n  }}\n}}\ndocument.getElementById('rules').addEventListener('click', async (event) => {{\n  const button = event.target.closest('[data-action=\"save-rule\"]');\n  if (!button) return;\n  const tr = button.closest('tr');\n  const patch = {{}};\n  tr.querySelectorAll('[data-field]').forEach((input) => {{\n    patch[input.dataset.field] = input.type === 'checkbox' ? input.checked : Number(input.value);\n  }});\n  const res = await fetch('/api/v1/data-tools/rules/' + encodeURIComponent(tr.dataset.ruleId), {{\n    method: 'PATCH', headers: {{ 'Content-Type': 'application/json' }}, body: JSON.stringify(patch),\n  }});\n  setStatus(res.ok ? 'Rule saved.' : 'Could not save rule.');\n}});\ndocument.getElementById('scan-now').addEventListener('click', async () => {{\n  const res = await fetch('/api/v1/data-tools/scan', {{ method: 'POST' }});\n  if (!res.ok) {{ setStatus('Scan failed.'); return; }}\n  const body = await res.json();\n  setStatus('Scan (dry run, nothing deleted): ' + outcomeText(body.data && body.data.targets));\n}});\ndocument.getElementById('run-now').addEventListener('click', async () => {{\n  const dry = document.getElementById('run-dry').checked;\n  const res = await fetch('/api/v1/data-tools/run', {{\n    method: 'POST', headers: {{ 'Content-Type': 'application/json' }}, body: JSON.stringify({{ dry_run: dry }}),\n  }});\n  if (!res.ok) {{ setStatus('Run failed (a cleanup may already be in progress).'); return; }}\n  const body = await res.json();\n  setStatus('Run ' + (body.data && body.data.status) + ': ' + outcomeText(body.data && body.data.counts));\n  loadRuns();\n}});\nloadRules();\nloadRuns();\n</script>\n</body>\n</html>\n",
        rows = rows
    );
    HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(html)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_request_defaults_to_rule_defaults_when_dry_run_omitted() {
        let body: RunRequest = serde_json::from_str("{}").expect("empty run body must parse");
        assert_eq!(body.dry_run, None);
        let explicit: RunRequest =
            serde_json::from_str("{\"dry_run\": true}").expect("explicit body must parse");
        assert_eq!(explicit.dry_run, Some(true));
    }

    #[test]
    fn rule_patch_rejects_unknown_fields_gracefully() {
        // Unknown JSON fields are ignored by serde default behavior
        // for this struct (no deny_unknown_fields): the patch still
        // parses and applies the known fields.
        let patch: RulePatch =
            serde_json::from_str("{\"retention_days\": 10, \"target\": \"other\"}")
                .expect("patch must parse");
        assert_eq!(patch.retention_days, Some(10));
        assert_eq!(patch.enabled, None);
    }
}
