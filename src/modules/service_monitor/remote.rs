// service_monitor module -- Remote library checks over SSH and their endpoints.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::events;
use unified_core::json::{error_response, success_response};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::process::Command;
use std::sync::Arc;

use super::*;

/// Transient authentication for one remote check. A password here
/// lives only for the duration of the re-check request that supplied
/// it and is never stored, logged, or returned.
///
/// `Debug` is hand-written: the password variant prints
/// `"[redacted]"` instead of the secret, so a future debug log can
/// never dump a plaintext password.
#[derive(Clone)]
pub(crate) enum RemoteAuth {
    Keyfile { path: String },
    Password { password: String },
}

impl std::fmt::Debug for RemoteAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Keyfile { path } => f
                .debug_struct("Keyfile")
                .field("path", path)
                .finish(),
            Self::Password { .. } => f
                .debug_struct("Password")
                .field("password", &"[redacted]")
                .finish(),
        }
    }
}

/// One resolved remote target for a check run.
#[derive(Debug, Clone)]
pub(crate) struct RemoteTarget {
    /// Report label: registered server id, or `ssh:user@host`.
    pub(crate) label: String,
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) username: String,
    pub(crate) auth: RemoteAuth,
}

/// Inline (request-scoped) remote server: never persisted, used for
/// this re-check only.
#[derive(Debug, Clone, Deserialize)]
pub struct InlineSshServer {
    pub host: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    pub username: String,
    pub auth: InlineSshAuth,
}

/// Inline authentication: a keyfile path, or a one-time password
/// that is used for this check only and never stored.
///
/// `Debug` is hand-written for the same reason as `RemoteAuth`: the
/// password variant prints `"[redacted]"` instead of the secret.
#[derive(Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum InlineSshAuth {
    Keyfile { keyfile: String },
    Password { password: String },
}

impl std::fmt::Debug for InlineSshAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Keyfile { keyfile } => f
                .debug_struct("Keyfile")
                .field("keyfile", keyfile)
                .finish(),
            Self::Password { .. } => f
                .debug_struct("Password")
                .field("password", &"[redacted]")
                .finish(),
        }
    }
}

/// Re-check request body. `targets` names `"local"` and/or
/// registered SSH server ids (empty means local only); `servers`
/// adds request-scoped inline servers; `passwords` maps a
/// password-auth server id to its one-time password for this check.
///
/// `Debug` is hand-written: the `passwords` map prints each server
/// id with its value replaced by `"[redacted]"`, so a future debug
/// log can never dump one-time passwords. (`servers` is safe through
/// the hand-written `Debug` of `InlineSshAuth`.)
#[derive(Clone, Default, Deserialize)]
pub struct LibraryRecheckRequest {
    #[serde(default)]
    pub targets: Vec<String>,
    #[serde(default)]
    pub servers: Vec<InlineSshServer>,
    #[serde(default)]
    pub passwords: HashMap<String, String>,
}

impl std::fmt::Debug for LibraryRecheckRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let redacted_passwords: HashMap<&str, &str> = self
            .passwords
            .iter()
            .map(|(server_id, _)| (server_id.as_str(), "[redacted]"))
            .collect();
        f.debug_struct("LibraryRecheckRequest")
            .field("targets", &self.targets)
            .field("servers", &self.servers)
            .field("passwords", &redacted_passwords)
            .finish()
    }
}

pub(crate) fn resolve_remote_targets(
    request: &LibraryRecheckRequest,
) -> UnifiedResult<Vec<RemoteTarget>> {
    let mut targets: Vec<RemoteTarget> = Vec::new();
    for target in &request.targets {
        if target == "local" {
            continue;
        }
        let server = find_ssh_server(target).ok_or_else(|| {
            UnifiedError::not_found(format!("SSH server \"{target}\" is not registered"))
        })?;
        let auth = match server.auth_kind.as_str() {
            "keyfile" => RemoteAuth::Keyfile {
                path: server.keyfile.clone().unwrap_or_default(),
            },
            _ => {
                let password = request
                    .passwords
                    .get(target)
                    .cloned()
                    .filter(|password| !password.is_empty())
                    .ok_or_else(|| {
                        UnifiedError::bad_request(format!(
                            "SSH server \"{target}\" uses password authentication; supply a one-time password for it in the re-check request (it is used once and never stored)"
                        ))
                    })?;
                RemoteAuth::Password { password }
            }
        };
        targets.push(RemoteTarget {
            label: server.id.clone(),
            host: server.host.clone(),
            port: server.port,
            username: server.username.clone(),
            auth,
        });
    }
    for inline in &request.servers {
        let (auth_kind, keyfile) = match &inline.auth {
            InlineSshAuth::Keyfile { keyfile } => ("keyfile", Some(keyfile.as_str())),
            InlineSshAuth::Password { .. } => ("password", None),
        };
        validate_ssh_connection(
            &inline.host,
            inline.port,
            &inline.username,
            auth_kind,
            keyfile,
        )?;
        let auth = match &inline.auth {
            InlineSshAuth::Keyfile { keyfile } => RemoteAuth::Keyfile {
                path: keyfile.clone(),
            },
            InlineSshAuth::Password { password } => {
                if password.is_empty() {
                    return Err(UnifiedError::bad_request(
                        "Inline SSH server password must not be empty",
                    ));
                }
                RemoteAuth::Password {
                    password: password.clone(),
                }
            }
        };
        targets.push(RemoteTarget {
            label: format!("ssh:{}@{}", inline.username, inline.host),
            host: inline.host.clone(),
            port: inline.port,
            username: inline.username.clone(),
            auth,
        });
    }
    Ok(targets)
}

/// Outcome of one remote probe invocation. `TransportFailed` means
/// the ssh transport itself failed (exit status 255, or the client
/// could not start); `NotFound` means the remote shell reported
/// exit status 127, i.e. the probe binary is not installed there.

pub(crate) enum RemoteProbeOutcome {
    Output(String),
    TransportFailed(String),
    NotFound,
}

pub(crate) fn ssh_available() -> bool {
    Command::new("ssh").arg("-V").output().is_ok()
}

pub(crate) fn sshpass_available() -> bool {
    Command::new("sshpass").arg("-V").output().is_ok()
}

pub(crate) fn run_remote_probe(
    target: &RemoteTarget,
    binary: &str,
    args: &[&str],
) -> RemoteProbeOutcome {
    // No shell is involved anywhere: the binary and its arguments are
    // separate argv entries, and the host/username/port/keyfile were
    // validated on the way in, so no configured value can inject a
    // second command. `BatchMode=yes` forbids interactive prompts;
    // `ConnectTimeout=10` bounds a hung connection;
    // `StrictHostKeyChecking=accept-new` trusts a host key on first
    // use (and pins it afterwards) instead of silently trusting every
    // host. Operators who need strict pinning should pre-seed the
    // application user's known_hosts file.
    let mut command = match &target.auth {
        RemoteAuth::Keyfile { .. } => Command::new("ssh"),
        RemoteAuth::Password { password } => {
            let mut wrapped = Command::new("sshpass");
            wrapped.arg("-e").env("SSHPASS", password).arg("ssh");
            wrapped
        }
    };
    command
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("ConnectTimeout=10")
        .arg("-o")
        .arg("StrictHostKeyChecking=accept-new")
        .arg("-o")
        .arg("LogLevel=ERROR")
        .arg("-p")
        .arg(target.port.to_string())
        .arg("-l")
        .arg(&target.username);
    if let RemoteAuth::Keyfile { path } = &target.auth {
        command.arg("-i").arg(path);
    }
    command.arg(&target.host).arg(binary);
    for arg in args {
        command.arg(arg);
    }
    let output = match command.output() {
        Ok(output) => output,
        Err(error) => {
            return RemoteProbeOutcome::TransportFailed(format!(
                "Could not start the ssh client: {error}"
            ));
        }
    };
    let code = output.status.code().unwrap_or(-1);
    if code == 255 {
        return RemoteProbeOutcome::TransportFailed(format!(
            "SSH exited with status 255: {}",
            truncate_text(&String::from_utf8_lossy(&output.stderr), 300)
        ));
    }
    if code == 127 {
        return RemoteProbeOutcome::NotFound;
    }
    RemoteProbeOutcome::Output(combined_output(&output))
}


pub(crate) fn check_requirement_remote(
    target: &RemoteTarget,
    requirement: &AggregatedRequirement,
    checked_at: DateTime<Utc>,
) -> Result<LibraryCheckResult, String> {
    let probes = probes_for(requirement);
    if probes.is_empty() {
        return Ok(missing_result(
            requirement,
            &target.label,
            &format!(
                "No version probe is registered for \"{}\"; add one to the probe allowlist before this requirement can be verified.",
                requirement.name
            ),
            checked_at,
        ));
    }
    let mut tried: Vec<&str> = Vec::new();
    for (binary, args) in probes {
        tried.push(binary);
        match run_remote_probe(target, binary, args) {
            RemoteProbeOutcome::Output(output) => {
                return Ok(evaluate_found(
                    requirement,
                    &output,
                    &target.label,
                    checked_at,
                ));
            }
            RemoteProbeOutcome::NotFound => continue,
            RemoteProbeOutcome::TransportFailed(detail) => return Err(detail),
        }
    }
    Ok(missing_result(
        requirement,
        &target.label,
        &format!(
            "Probe binaries ({}) were not found on the remote host",
            tried.join(", ")
        ),
        checked_at,
    ))
}


pub(crate) fn check_target_remote(
    target: &RemoteTarget,
    requirements: &[AggregatedRequirement],
) -> LibraryCheckReport {
    let checked_at = Utc::now();
    if !ssh_available() {
        return error_report(
            &target.label,
            "The ssh client is not installed on this host, so remote library checks cannot run.",
        );
    }
    if matches!(target.auth, RemoteAuth::Password { .. }) && !sshpass_available() {
        return error_report(
            &target.label,
            "Password authentication needs the sshpass helper, which is not installed; use keyfile authentication instead.",
        );
    }
    match run_remote_probe(target, "true", &[]) {
        RemoteProbeOutcome::TransportFailed(detail) => {
            return error_report(
                &target.label,
                &format!("SSH connection failed: {detail}"),
            );
        }
        _ => {}
    }
    let mut results: Vec<LibraryCheckResult> = Vec::new();
    for requirement in requirements {
        match check_requirement_remote(target, requirement, checked_at) {
            Ok(result) => results.push(result),
            Err(detail) => {
                return error_report(
                    &target.label,
                    &format!("SSH connection failed during the check: {detail}"),
                );
            }
        }
    }
    build_report(&target.label, checked_at, results)
}

/// Emit `library.check.failed` once per missing/outdated result in
/// a report, so notifications and audit can pick failures up.
/// Error reports (target unreachable) emit nothing: nothing was
/// actually checked there.

pub(crate) fn emit_library_failure_events(ctx: &AppContext, report: &LibraryCheckReport) {
    if report.status == "error" {
        return;
    }
    for result in &report.results {
        if result.status == "missing" || result.status == "outdated" {
            ctx.hooks.emit(
                LIBRARY_CHECK_FAILED_EVENT,
                json!({
                    "library": result.library,
                    "required_version": result.required_version,
                    "found_version": result.found_version,
                    "status": result.status,
                    "target": result.target,
                    "required_by": result.required_by,
                    "reason": result.reason,
                    "checked_at": result.checked_at,
                }),
            );
        }
    }
}

/// Run library checks for the requested targets (local when
/// `targets` is empty or contains `"local"`, plus every resolved
/// remote target), cache every report, emit failure events, and
/// return the reports. Blocking I/O: handlers must run this inside
/// `web::block`. A target that cannot be reached yields an `"error"`
/// report for that target instead of failing the whole run.
pub fn run_library_checks(
    ctx: &AppContext,
    request: &LibraryRecheckRequest,
) -> UnifiedResult<Vec<LibraryCheckReport>> {
    let remote_targets = resolve_remote_targets(request)?;
    let requirements = aggregate_external_requirements(&ctx.modules);
    let want_local = request.targets.is_empty()
        || request.targets.iter().any(|target| target == "local");
    let mut reports: Vec<LibraryCheckReport> = Vec::new();
    if want_local {
        let checked_at = Utc::now();
        let results: Vec<LibraryCheckResult> = requirements
            .iter()
            .map(|requirement| check_requirement_local(requirement, checked_at))
            .collect();
        let report = build_report("local", checked_at, results);
        emit_library_failure_events(ctx, &report);
        store_library_report(&report);
        reports.push(report);
    }
    for target in &remote_targets {
        let report = check_target_remote(target, &requirements);
        emit_library_failure_events(ctx, &report);
        store_library_report(&report);
        reports.push(report);
    }
    Ok(reports)
}

/// List the latest local library check results (paginated, with
/// `?search=` and `?status=` / `?target=` filters). When nothing has
/// been checked yet, the first read runs a fresh local check so the
/// response is useful instead of empty; later reads serve the cache.

pub(crate) async fn list_libraries(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    if let Err(error) = authorize(&ctx, &req, PERM_SERVICE_MONITOR_VIEW) {
        return error_response(&error, ctx.is_debug());
    }
    let pagination = query.normalized();
    let search = pagination.search.clone().map(|s| s.to_lowercase());
    let status_filter = pagination.filters.get("status").cloned();
    let target_filter = pagination.filters.get("target").cloned();
    let debug = ctx.is_debug();
    let ctx_clone = Arc::clone(&ctx);
    // Probe I/O blocks, so it runs off the async worker threads.
    let loaded = web::block(move || {
        let cached = latest_library_results();
        if cached.is_empty() {
            let request = LibraryRecheckRequest::default();
            run_library_checks(&ctx_clone, &request).map(|_| latest_library_results())
        } else {
            Ok(cached)
        }
    })
    .await;
    let mut results = match loaded {
        Ok(Ok(results)) => results,
        Ok(Err(error)) => return error_response(&error, debug),
        Err(error) => {
            return error_response(
                &UnifiedError::internal(format!("Library check failed: {error}")),
                debug,
            );
        }
    };
    results.retain(|result| {
        if let Some(search) = &search {
            let haystack = format!(
                "{} {} {}",
                result.library,
                result.reason,
                result.required_by.join(" ")
            )
            .to_lowercase();
            if !haystack.contains(search.as_str()) {
                return false;
            }
        }
        if let Some(status) = &status_filter {
            if &result.status != status {
                return false;
            }
        }
        if let Some(target) = &target_filter {
            if &result.target != target {
                return false;
            }
        }
        true
    });
    match pagination.sort.as_deref().unwrap_or("library") {
        "status" => results.sort_by(|a, b| {
            a.status
                .cmp(&b.status)
                .then_with(|| a.library.to_lowercase().cmp(&b.library.to_lowercase()))
        }),
        _ => results.sort_by(|a, b| a.library.to_lowercase().cmp(&b.library.to_lowercase())),
    }
    if pagination.order == "desc" {
        results.reverse();
    }
    let total = results.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(results.len());
    let page_items = if start < results.len() {
        results[start..end].to_vec()
    } else {
        Vec::new()
    };
    success_response(PageResult::new(page_items, total, &pagination).to_json(), 200)
}

/// Re-check action: run the library checks again for the requested
/// targets and return one report per target. Remote targets that
/// cannot be reached yield an `"error"` report instead of failing
/// the whole run; unknown server ids and invalid targets are 400/404
/// errors in the standard envelope.

pub(crate) async fn recheck_libraries(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<LibraryRecheckRequest>,
) -> HttpResponse {
    if let Err(error) = authorize(&ctx, &req, PERM_SERVICE_MONITOR_MANAGE) {
        return error_response(&error, ctx.is_debug());
    }
    let debug = ctx.is_debug();
    let request = body.into_inner();
    let ctx_clone = Arc::clone(&ctx);
    let checked = web::block(move || run_library_checks(&ctx_clone, &request)).await;
    match checked {
        Ok(Ok(reports)) => success_response(reports, 200),
        Ok(Err(error)) => error_response(&error, debug),
        Err(error) => error_response(
            &UnifiedError::internal(format!("Library check failed: {error}")),
            debug,
        ),
    }
}

/// List the latest check report per target (local first, then
/// remote targets sorted by label), from the cache. This is the
/// read-only view of remote results: the re-check endpoint runs the
/// checks, this one only reports the latest stored outcome.

pub(crate) async fn list_library_reports(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    if let Err(error) = authorize(&ctx, &req, PERM_SERVICE_MONITOR_VIEW) {
        return error_response(&error, ctx.is_debug());
    }
    success_response(latest_library_reports(), 200)
}

pub(crate) async fn list_registered_ssh_servers(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    if let Err(error) = authorize(&ctx, &req, PERM_SERVICE_MONITOR_VIEW) {
        return error_response(&error, ctx.is_debug());
    }
    success_response(list_ssh_servers(), 200)
}

pub(crate) async fn register_ssh_server_endpoint(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<SshServer>,
) -> HttpResponse {
    if let Err(error) = authorize(&ctx, &req, PERM_SERVICE_MONITOR_MANAGE) {
        return error_response(&error, ctx.is_debug());
    }
    let debug = ctx.is_debug();
    match register_ssh_server(body.into_inner()) {
        Ok(server) => success_response(server, 201),
        Err(error) => error_response(&error, debug),
    }
}


pub(crate) async fn remove_ssh_server_endpoint(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    if let Err(error) = authorize(&ctx, &req, PERM_SERVICE_MONITOR_MANAGE) {
        return error_response(&error, ctx.is_debug());
    }
    let debug = ctx.is_debug();
    match remove_ssh_server(&id.into_inner()) {
        Ok(()) => success_response(json!({ "removed": true }), 200),
        Err(error) => error_response(&error, debug),
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
    use unified_core::config::AppConfig;
    use unified_core::hooks::HookBus;
    use unified_core::modules::ModuleRegistry;
    use actix_web::http::StatusCode;
    use actix_web::test::TestRequest;

    fn test_context() -> AppContext {
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    /// A context whose `role.check` handler allows every permission.
    fn allowing_context() -> AppContext {
        let ctx = test_context();
        ctx.hooks.register(
            "role.check",
            "test",
            Arc::new(|context: &mut unified_core::hooks::HookContext| {
                context.set("allowed", serde_json::Value::Bool(true));
            }),
        );
        ctx
    }

    fn authed_request() -> HttpRequest {
        verified_request("user-1", "admin")
    }

    fn anonymous_request() -> HttpRequest {
        TestRequest::default().to_http_request()
    }

    fn data(ctx: AppContext) -> web::Data<Arc<AppContext>> {
        web::Data::new(Arc::new(ctx))
    }

    fn keyfile_server(id: &str) -> SshServer {
        SshServer {
            id: id.to_string(),
            host: "example.com".to_string(),
            port: 22,
            username: "deploy".to_string(),
            auth_kind: "keyfile".to_string(),
            keyfile: Some("/home/deploy/.ssh/id_ed25519".to_string()),
        }
    }

    #[test]
    fn remote_auth_password_debug_redacts_password() {
        let auth = RemoteAuth::Password {
            password: "super-secret-password".to_string(),
        };
        let rendered = format!("{auth:?}");
        assert!(!rendered.contains("super-secret-password"));
        assert!(rendered.contains("[redacted]"));
    }

    #[test]
    fn remote_auth_keyfile_debug_keeps_path() {
        let auth = RemoteAuth::Keyfile {
            path: "/home/deploy/.ssh/id_ed25519".to_string(),
        };
        let rendered = format!("{auth:?}");
        assert!(rendered.contains("/home/deploy/.ssh/id_ed25519"));
    }

    #[test]
    fn inline_ssh_auth_password_debug_redacts_password() {
        let auth = InlineSshAuth::Password {
            password: "inline-secret".to_string(),
        };
        let rendered = format!("{auth:?}");
        assert!(!rendered.contains("inline-secret"));
        assert!(rendered.contains("[redacted]"));
    }

    #[test]
    fn library_recheck_request_debug_redacts_passwords() {
        let mut passwords = HashMap::new();
        passwords.insert("srv-1".to_string(), "one-time-secret".to_string());
        let request = LibraryRecheckRequest {
            targets: vec!["srv-1".to_string()],
            servers: vec![InlineSshServer {
                host: "example.com".to_string(),
                port: 22,
                username: "deploy".to_string(),
                auth: InlineSshAuth::Password {
                    password: "inline-secret".to_string(),
                },
            }],
            passwords,
        };
        let rendered = format!("{request:?}");
        assert!(!rendered.contains("one-time-secret"));
        assert!(!rendered.contains("inline-secret"));
        assert!(rendered.contains("[redacted]"));
        // The server id (the map key) is not secret and stays visible.
        assert!(rendered.contains("srv-1"));
    }

    #[test]
    fn remote_target_debug_redacts_password_through_auth() {
        let target = RemoteTarget {
            label: "srv-1".to_string(),
            host: "example.com".to_string(),
            port: 22,
            username: "deploy".to_string(),
            auth: RemoteAuth::Password {
                password: "target-secret".to_string(),
            },
        };
        let rendered = format!("{target:?}");
        assert!(!rendered.contains("target-secret"));
        assert!(rendered.contains("[redacted]"));
    }

    #[tokio::test]
    async fn list_libraries_rejects_anonymous_callers() {
        let response = list_libraries(
            data(test_context()),
            anonymous_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn list_libraries_rejects_callers_without_view() {
        let response = list_libraries(
            data(test_context()),
            authed_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn recheck_libraries_rejects_anonymous_callers() {
        let response = recheck_libraries(
            data(test_context()),
            anonymous_request(),
            web::Json(LibraryRecheckRequest::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn recheck_libraries_rejects_callers_without_manage() {
        let response = recheck_libraries(
            data(test_context()),
            authed_request(),
            web::Json(LibraryRecheckRequest::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn recheck_libraries_accepts_callers_with_manage() {
        let response = recheck_libraries(
            data(allowing_context()),
            authed_request(),
            web::Json(LibraryRecheckRequest::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn list_library_reports_rejects_anonymous_callers() {
        let response =
            list_library_reports(data(test_context()), anonymous_request()).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn list_library_reports_accepts_callers_with_view() {
        let response =
            list_library_reports(data(allowing_context()), authed_request()).await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn list_registered_ssh_servers_rejects_anonymous_callers() {
        let response =
            list_registered_ssh_servers(data(test_context()), anonymous_request()).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn list_registered_ssh_servers_accepts_callers_with_view() {
        let response =
            list_registered_ssh_servers(data(allowing_context()), authed_request()).await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn register_ssh_server_rejects_anonymous_callers() {
        let response = register_ssh_server_endpoint(
            data(test_context()),
            anonymous_request(),
            web::Json(keyfile_server("auth-gate-test-server")),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn register_ssh_server_rejects_callers_without_manage() {
        let response = register_ssh_server_endpoint(
            data(test_context()),
            authed_request(),
            web::Json(keyfile_server("auth-gate-test-server")),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn register_ssh_server_accepts_callers_with_manage() {
        let response = register_ssh_server_endpoint(
            data(allowing_context()),
            authed_request(),
            web::Json(keyfile_server("auth-gate-test-server")),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        // Clean up the shared in-memory registry for other tests.
        let _ = remove_ssh_server("auth-gate-test-server");
    }

    #[tokio::test]
    async fn remove_ssh_server_rejects_anonymous_callers() {
        let response = remove_ssh_server_endpoint(
            data(test_context()),
            anonymous_request(),
            web::Path::from("auth-gate-test-server".to_string()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn remove_ssh_server_rejects_callers_without_manage() {
        let response = remove_ssh_server_endpoint(
            data(test_context()),
            authed_request(),
            web::Path::from("auth-gate-test-server".to_string()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
}
