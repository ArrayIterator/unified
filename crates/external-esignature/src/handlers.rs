//! eSignature external module.

use super::*;


pub(crate) fn status_name(status: RequestStatus) -> &'static str {
    match status {
        RequestStatus::Draft => "draft",
        RequestStatus::Sent => "sent",
        RequestStatus::PartiallySigned => "partially_signed",
        RequestStatus::Completed => "completed",
        RequestStatus::Declined => "declined",
        RequestStatus::Expired => "expired",
    }
}

fn parse_status(raw: &str) -> UnifiedResult<RequestStatus> {
    match raw {
        "draft" => Ok(RequestStatus::Draft),
        "sent" => Ok(RequestStatus::Sent),
        "partially_signed" => Ok(RequestStatus::PartiallySigned),
        "completed" => Ok(RequestStatus::Completed),
        "declined" => Ok(RequestStatus::Declined),
        "expired" => Ok(RequestStatus::Expired),
        _ => Err(UnifiedError::bad_request(format!(
            "unknown status filter '{raw}'"
        ))),
    }
}

// HTTP handlers (thin wrappers over the state operations)

fn require_tenant(query: &TenantQuery) -> UnifiedResult<String> {
    match &query.tenant_id {
        Some(tenant_id) if valid_tenant_id(tenant_id) => Ok(tenant_id.clone()),
        _ => Err(UnifiedError::bad_request(
            "tenant_id query parameter is required",
        )),
    }
}

fn client_ip(req: &HttpRequest) -> String {
    if let Some(forwarded) = req
        .headers()
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
    {
        if let Some(first) = forwarded.split(',').next() {
            let first = first.trim();
            if !first.is_empty() {
                return first.to_string();
            }
        }
    }
    req.peer_addr()
        .map(|addr| addr.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

pub(crate) fn user_agent(req: &HttpRequest) -> String {
    req.headers()
        .get("user-agent")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("unknown")
        .to_string()
}

pub(crate) async fn create_request_handler(
    state: web::Data<Arc<EsignatureState>>,
    body: web::Json<CreateRequestInput>,
) -> HttpResponse {
    match state.create_request(body.into_inner()) {
        Ok(request) => success_response(request, 201),
        Err(error) => error_response(&error, false),
    }
}

pub(crate) async fn list_requests_handler(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EsignatureState>>,
    query: web::Query<ListQuery>,
) -> HttpResponse {
    let tenant_id = match require_tenant(&TenantQuery {
        tenant_id: query.tenant_id.clone(),
    }) {
        Ok(tenant_id) => tenant_id,
        Err(error) => return error_response(&error, false),
    };
    let status = match &query.status {
        Some(raw) => match parse_status(raw) {
            Ok(status) => Some(status),
            Err(error) => return error_response(&error, false),
        },
        None => None,
    };
    let requests = state.list_requests(&tenant_id, status, Some(&ctx.hooks));
    success_response(requests, 200)
}

pub(crate) async fn get_request_handler(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EsignatureState>>,
    id: web::Path<String>,
    query: web::Query<TenantQuery>,
) -> HttpResponse {
    let tenant_id = match require_tenant(&query) {
        Ok(tenant_id) => tenant_id,
        Err(error) => return error_response(&error, false),
    };
    match state.load_for_tenant(&id, &tenant_id, Some(&ctx.hooks)) {
        Ok(request) => success_response(request, 200),
        Err(error) => error_response(&error, false),
    }
}

pub(crate) async fn send_request_handler(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EsignatureState>>,
    id: web::Path<String>,
    query: web::Query<TenantQuery>,
) -> HttpResponse {
    let tenant_id = match require_tenant(&query) {
        Ok(tenant_id) => tenant_id,
        Err(error) => return error_response(&error, false),
    };
    let secret = match signing_secret() {
        Ok(secret) => secret,
        Err(error) => return error_response(&error, false),
    };
    match state.send_request(&id, &tenant_id, &secret, &ctx.hooks) {
        Ok((request, links)) => success_response(
            json!({ "request": request, "signing_links": links }),
            200,
        ),
        Err(error) => error_response(&error, false),
    }
}

pub(crate) async fn remind_handler(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EsignatureState>>,
    id: web::Path<String>,
    query: web::Query<TenantQuery>,
) -> HttpResponse {
    let tenant_id = match require_tenant(&query) {
        Ok(tenant_id) => tenant_id,
        Err(error) => return error_response(&error, false),
    };
    match state.remind(&id, &tenant_id, &ctx.hooks) {
        Ok(payload) => success_response(payload, 200),
        Err(error) => error_response(&error, false),
    }
}

pub(crate) async fn audit_handler(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EsignatureState>>,
    id: web::Path<String>,
    query: web::Query<TenantQuery>,
) -> HttpResponse {
    let tenant_id = match require_tenant(&query) {
        Ok(tenant_id) => tenant_id,
        Err(error) => return error_response(&error, false),
    };
    match state.audit_trail(&id, &tenant_id, Some(&ctx.hooks)) {
        Ok(payload) => success_response(payload, 200),
        Err(error) => error_response(&error, false),
    }
}

pub(crate) async fn verify_audit_handler(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EsignatureState>>,
    id: web::Path<String>,
    query: web::Query<TenantQuery>,
) -> HttpResponse {
    let tenant_id = match require_tenant(&query) {
        Ok(tenant_id) => tenant_id,
        Err(error) => return error_response(&error, false),
    };
    match state.verify_audit_chain(&id, &tenant_id, Some(&ctx.hooks)) {
        Ok(verification) => success_response(verification, 200),
        Err(error) => error_response(&error, false),
    }
}

pub(crate) async fn certificate_handler(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EsignatureState>>,
    id: web::Path<String>,
    query: web::Query<TenantQuery>,
) -> HttpResponse {
    let tenant_id = match require_tenant(&query) {
        Ok(tenant_id) => tenant_id,
        Err(error) => return error_response(&error, false),
    };
    match state.certificate(&id, &tenant_id, Some(&ctx.hooks)) {
        Ok(certificate) => success_response(certificate, 200),
        Err(error) => error_response(&error, false),
    }
}

pub(crate) async fn verify_certificate_handler(
    state: web::Data<Arc<EsignatureState>>,
    certificate_hash: web::Path<String>,
) -> HttpResponse {
    match state.verify_certificate(&certificate_hash) {
        Ok(verification) => success_response(verification, 200),
        Err(error) => error_response(&error, false),
    }
}

pub(crate) async fn ceremony_handler(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EsignatureState>>,
    token: web::Path<String>,
) -> HttpResponse {
    let secret = match signing_secret() {
        Ok(secret) => secret,
        Err(error) => return error_response(&error, false),
    };
    match state.ceremony(&token, &secret, Some(&ctx.hooks)) {
        Ok(payload) => success_response(payload, 200),
        Err(error) => error_response(&error, false),
    }
}

pub(crate) async fn sign_handler(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EsignatureState>>,
    token: web::Path<String>,
    body: web::Json<SignInput>,
    req: HttpRequest,
) -> HttpResponse {
    let secret = match signing_secret() {
        Ok(secret) => secret,
        Err(error) => return error_response(&error, false),
    };
    let ip = client_ip(&req);
    let agent = user_agent(&req);
    match state.record_signature(&token, &secret, body.into_inner(), &ip, &agent, &ctx.hooks) {
        Ok((request, certificate)) => success_response(
            json!({ "request": request, "certificate": certificate }),
            200,
        ),
        Err(error) => error_response(&error, false),
    }
}

pub(crate) async fn decline_handler(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EsignatureState>>,
    token: web::Path<String>,
    body: web::Json<DeclineInput>,
) -> HttpResponse {
    let secret = match signing_secret() {
        Ok(secret) => secret,
        Err(error) => return error_response(&error, false),
    };
    match state.decline_via_token(&token, &secret, body.into_inner(), &ctx.hooks) {
        Ok(request) => success_response(request, 200),
        Err(error) => error_response(&error, false),
    }
}
