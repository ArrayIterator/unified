// Tax module - filing handlers.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use super::*;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse, web};
use base64::Engine as _;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use url::Url;
use uuid::Uuid;

// Handlers: filings

/// Run the filing-reminder check for one company: refresh the
/// current and previous period filings for every jurisdiction that
/// has tax rules, update their statuses, and emit `tax.filing_due`
/// plus `notification.requested` for filings inside the reminder
/// window (or already overdue) that have not been reminded about
/// since their status last changed.
pub(crate) fn run_filing_check(state: &TaxState, company_id: Uuid) -> Vec<TaxFiling> {
    let settings = state.settings_for(company_id);
    let today = today();
    let mut jurisdictions: HashSet<(String, Option<String>)> = HashSet::new();
    for entry in state.rules.iter() {
        let rule = entry.value();
        if rule.company_id == company_id && rule.enabled {
            jurisdictions.insert((rule.country_code.clone(), rule.region_code.clone()));
        }
    }
    if jurisdictions.is_empty() {
        if let Some(country) = settings.default_country_code.clone() {
            jurisdictions.insert((country, settings.default_region_code.clone()));
        }
    }
    let mut changed: Vec<TaxFiling> = Vec::new();
    for (country_code, region_code) in jurisdictions {
        let (current_start, current_end) = period_for(today, &settings.filing_frequency, &settings);
        let (previous_start, previous_end) = previous_period(
            current_start,
            current_end,
            &settings.filing_frequency,
            &settings,
        );
        for (period_start, period_end) in
            [(current_start, current_end), (previous_start, previous_end)]
        {
            let due_date = period_end
                + chrono::Duration::days(i64::from(settings.filing_due_days_after_period));
            let tax_total = period_tax_total(
                state,
                company_id,
                &country_code,
                region_code.as_deref(),
                period_start,
                period_end,
            );
            let existing = state
                .filings
                .iter()
                .find(|entry| {
                    let filing = entry.value();
                    filing.company_id == company_id
                        && filing.country_code == country_code
                        && filing.region_code == region_code
                        && filing.period_start == period_start
                        && filing.period_end == period_end
                })
                .map(|entry| entry.key().clone());
            let status = if today > due_date {
                "overdue"
            } else if today
                >= due_date - chrono::Duration::days(i64::from(settings.reminder_days_before))
            {
                "due"
            } else {
                "upcoming"
            };
            let mut filing = match existing {
                Some(id) => state
                    .filings
                    .get(&id)
                    .map(|entry| entry.value().clone())
                    .unwrap_or_else(|| TaxFiling {
                        id,
                        company_id,
                        country_code: country_code.clone(),
                        region_code: region_code.clone(),
                        period_start,
                        period_end,
                        frequency: settings.filing_frequency.clone(),
                        due_date,
                        status: status.to_string(),
                        tax_total,
                        submitted_at: None,
                        submitted_via: None,
                        reference: None,
                        last_reminder_at: None,
                        created_at: now(),
                        updated_at: now(),
                    }),
                None => TaxFiling {
                    id: new_id(),
                    company_id,
                    country_code: country_code.clone(),
                    region_code: region_code.clone(),
                    period_start,
                    period_end,
                    frequency: settings.filing_frequency.clone(),
                    due_date,
                    status: status.to_string(),
                    tax_total,
                    submitted_at: None,
                    submitted_via: None,
                    reference: None,
                    last_reminder_at: None,
                    created_at: now(),
                    updated_at: now(),
                },
            };
            if filing.status == "submitted" {
                filing.tax_total = tax_total;
                filing.updated_at = now();
                state.filings.insert(filing.id.clone(), filing.clone());
                continue;
            }
            let was_due = filing.status == "due" || filing.status == "overdue";
            filing.status = status.to_string();
            filing.tax_total = tax_total;
            filing.updated_at = now();
            let needs_reminder = (status == "due" || status == "overdue")
                && (!was_due
                    || filing
                        .last_reminder_at
                        .is_none_or(|at| at.date_naive() < today));
            if needs_reminder {
                filing.last_reminder_at = Some(now());
                state.filings.insert(filing.id.clone(), filing.clone());
                state.emit(
                    "tax.filing_due",
                    json!({
                        "filing_id": filing.id,
                        "company_id": company_id.to_string(),
                        "country_code": filing.country_code,
                        "region_code": filing.region_code,
                        "period_start": filing.period_start,
                        "period_end": filing.period_end,
                        "due_date": filing.due_date,
                        "status": filing.status,
                        "tax_total": minor_to_major(filing.tax_total, &settings.default_currency),
                    }),
                );
                let mut notification = json!({
                    "company_id": company_id.to_string(),
                    "title": format!(
                        "Tax filing {} for {}",
                        filing.status, filing.country_code
                    ),
                    "body": format!(
                        "Tax filing for {} ({} to {}) is {} with tax total {}. Due {}.",
                        filing.country_code,
                        filing.period_start,
                        filing.period_end,
                        filing.status,
                        minor_to_major(filing.tax_total, &settings.default_currency),
                        filing.due_date
                    ),
                    "kind": "tax",
                });
                if let Some(user_id) = &settings.notify_user_id {
                    if let Value::Object(map) = &mut notification {
                        map.insert("user_id".to_string(), Value::String(user_id.clone()));
                    }
                }
                state.emit("notification.requested", notification);
                changed.push(filing);
            } else {
                state.filings.insert(filing.id.clone(), filing.clone());
                changed.push(filing);
            }
        }
    }
    changed
}

pub(crate) async fn list_filings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let auth: UnifiedResult<Uuid> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VIEW)?;
        acting_company(&ctx, &_tenant, &req, &caller)
    })();
    let company_id = match auth {
        Ok(company_id) => company_id,
        Err(error) => return json::error_response(&error, false),
    };
    let params = query.into_inner();
    let mut items: Vec<TaxFiling> = state
        .filings
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|filing| {
            filing.company_id == company_id
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| filing.status == *status)
                && params
                    .filters
                    .get("country_code")
                    .is_none_or(|country| filing.country_code.eq_ignore_ascii_case(country))
                && params
                    .filters
                    .get("frequency")
                    .is_none_or(|frequency| filing.frequency == *frequency)
        })
        .collect();
    items.sort_by(|a, b| a.due_date.cmp(&b.due_date));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn create_filing(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    body: web::Json<CreateTaxFiling>,
) -> HttpResponse {
    let result: UnifiedResult<TaxFiling> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE)?;
        let company_id = acting_company(&ctx, &_tenant, &req, &caller)?;
        let input = body.into_inner();
        if !valid_country_code(input.country_code.trim()) {
            return Err(UnifiedError::bad_request(
                "country_code must be a 2-letter ISO code",
            ));
        }
        let region_code = input
            .region_code
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        if let Some(region) = &region_code {
            if !valid_region_code(region) {
                return Err(UnifiedError::bad_request(
                    "region_code must be at most 16 letters, digits, hyphens, or underscores",
                ));
            }
        }
        let period_start = parse_date(&input.period_start, "period_start")?;
        let period_end = parse_date(&input.period_end, "period_end")?;
        if period_end < period_start {
            return Err(UnifiedError::bad_request(
                "period_end must not be before period_start",
            ));
        }
        let frequency = input
            .frequency
            .unwrap_or_else(|| state.settings_for(company_id).filing_frequency);
        if !FILING_FREQUENCIES.contains(&frequency.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "frequency must be one of: {}",
                FILING_FREQUENCIES.join(", ")
            )));
        }
        if state.filings.iter().any(|entry| {
            let filing = entry.value();
            filing.company_id == company_id
                && filing
                    .country_code
                    .eq_ignore_ascii_case(input.country_code.trim())
                && filing.region_code == region_code
                && filing.period_start == period_start
                && filing.period_end == period_end
        }) {
            return Err(UnifiedError::conflict(
                "A filing already exists for this company, jurisdiction, and period",
            ));
        }
        let settings = state.settings_for(company_id);
        let due_date =
            period_end + chrono::Duration::days(i64::from(settings.filing_due_days_after_period));
        let tax_total = period_tax_total(
            &state,
            company_id,
            &input.country_code.trim().to_uppercase(),
            region_code.as_deref(),
            period_start,
            period_end,
        );
        let today = today();
        let status = if today > due_date {
            "overdue"
        } else if today
            >= due_date - chrono::Duration::days(i64::from(settings.reminder_days_before))
        {
            "due"
        } else {
            "upcoming"
        };
        let filing = TaxFiling {
            id: new_id(),
            company_id,
            country_code: input.country_code.trim().to_uppercase(),
            region_code,
            period_start,
            period_end,
            frequency,
            due_date,
            status: status.to_string(),
            tax_total,
            submitted_at: None,
            submitted_via: None,
            reference: None,
            last_reminder_at: None,
            created_at: now(),
            updated_at: now(),
        };
        state.filings.insert(filing.id.clone(), filing.clone());
        state.record_audit(
            company_id,
            &caller.actor,
            "filing_created",
            "tax_filing",
            &filing.id,
            "tax.filing_created",
            json!({
                "country_code": filing.country_code,
                "period_start": filing.period_start,
                "period_end": filing.period_end,
                "due_date": filing.due_date,
            }),
        );
        Ok(filing)
    })();
    respond_created(result)
}

pub(crate) async fn get_filing(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<TaxFiling> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VIEW)?;
        let company_id = acting_company(&ctx, &_tenant, &req, &caller)?;
        let filing = state
            .filings
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Tax filing not found"))?;
        if filing.company_id != company_id {
            return Err(UnifiedError::not_found("Tax filing not found"));
        }
        Ok(filing)
    })();
    json::respond(result, false)
}

pub(crate) async fn check_due_filings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE)?;
        let company_id = acting_company(&ctx, &_tenant, &req, &caller)?;
        let changed = run_filing_check(&state, company_id);
        Ok(json!({
            "company_id": company_id.to_string(),
            "checked_filings": changed.len(),
            "filings": changed,
        }))
    })();
    json::respond(result, false)
}

// Handlers: filing submission through a connector

/// File one filing through a connector. The `example` kind is a
/// clearly-marked example provider mapping: it performs no network
/// call and returns a simulated confirmation with obviously fake
/// field names. A `generic_http` connector performs a real signed
/// POST to its validated endpoint with timeout and retry policy.
pub(crate) async fn submit_through_connector(
    ctx: &Arc<AppContext>,
    connector: &TaxConnector,
    filing: &TaxFiling,
) -> UnifiedResult<(String, String)> {
    if connector.kind == "example" {
        let reference = format!("EXAMPLE-{}-{}", filing.country_code, filing.id);
        return Ok((
            format!("example provider (simulated): {}", connector.name),
            reference,
        ));
    }
    let (url, pinned) =
        validate_endpoint_url_pinned(&connector.endpoint_url, &connector.allowed_hosts)?;
    let secret = encryption_secret(ctx);
    let credential = match &connector.credential_encrypted {
        Some(stored) => Some(decrypt_tax_credential(&secret, stored)?),
        None => None,
    };
    let signing_secret = match &connector.signing_secret_encrypted {
        Some(stored) => Some(decrypt_tax_credential(&secret, stored)?),
        None => None,
    };
    let payload = apply_request_mapping(
        &connector.request_mapping,
        &json!({
            "filing_id": filing.id,
            "company_id": filing.company_id.to_string(),
            "country_code": filing.country_code,
            "region_code": filing.region_code,
            "period_start": filing.period_start,
            "period_end": filing.period_end,
            "due_date": filing.due_date,
            "tax_total": filing.tax_total,
        }),
    );
    let body = serde_json::to_string(&payload).map_err(|error| {
        UnifiedError::internal(format!("Cannot serialize filing payload: {error}"))
    })?;
    let mut client_builder =
        reqwest::Client::builder().timeout(Duration::from_millis(connector.timeout_ms));
    // Connect to the address validated (and pinned) above, not to
    // whatever the hostname re-resolves to at connect time: without
    // this override a DNS-rebinding answer could redirect the
    // request at an internal address after validation passed.
    if let (Some(addr), Some(host)) = (pinned, url.host_str()) {
        client_builder = client_builder.resolve(host, addr);
    }
    let client = client_builder
        .build()
        .map_err(|error| UnifiedError::internal(format!("Cannot build HTTP client: {error}")))?;
    let mut last_error = String::new();
    let attempts = connector.max_retries as usize + 1;
    for attempt in 0..attempts {
        let timestamp = Utc::now().timestamp();
        let mut request = client
            .post(url.clone())
            .header("Content-Type", "application/json")
            .header("X-Tax-Filing-Id", filing.id.clone());
        match connector.auth_scheme.as_str() {
            "bearer" => {
                if let Some(key) = &credential {
                    request = request.bearer_auth(key);
                }
            }
            "api_key" => {
                if let Some(key) = &credential {
                    request = request.header("X-Api-Key", key);
                }
            }
            "hmac" => {
                if let Some(signing) = &signing_secret {
                    let signature = sign_body(signing, timestamp, &body);
                    request = request
                        .header("X-Tax-Signature", format!("sha256={signature}"))
                        .header("X-Tax-Timestamp", timestamp.to_string());
                }
            }
            _ => {}
        }
        match request.body(body.clone()).send().await {
            Ok(response) => {
                let status = response.status();
                let text = response.text().await.unwrap_or_default();
                if !status.is_success() {
                    last_error = format!(
                        "Provider returned HTTP {}: {}",
                        status.as_u16(),
                        text.chars().take(200).collect::<String>()
                    );
                    continue;
                }
                let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                let mapped = apply_response_mapping(&connector.response_mapping, &parsed);
                let reference = mapped
                    .get("reference")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .or_else(|| {
                        parsed
                            .get("reference")
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                    })
                    .unwrap_or_else(|| format!("{}-{}", connector.name, filing.id));
                return Ok((connector.name.clone(), reference));
            }
            Err(error) => {
                last_error = format!("Provider request failed: {error}");
                if attempt + 1 < attempts {
                    tokio::time::sleep(Duration::from_millis(200 * (1 << attempt))).await;
                }
            }
        }
    }
    Err(UnifiedError::internal(format!(
        "Filing submission failed after {attempts} attempt(s): {last_error}"
    )))
}

/// Submit one filing, through a connector or manually.
///
/// Filing early is intended: there is no due/overdue status
/// requirement, so an `upcoming` filing may be filed before its due
/// date. Concurrent submits of the same filing are serialised by
/// the per-filing mutex (`filing_submit_lock`), held for the whole
/// check-submit-record sequence, so two requests cannot both pass
/// the "already submitted" check and file twice.
pub(crate) async fn submit_filing(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<SubmitTaxFiling>,
) -> HttpResponse {
    let filing_id = path.as_str().to_string();
    let submit_lock = state.filing_submit_lock(&filing_id);
    let _submit_guard = submit_lock.lock().await;
    let result: UnifiedResult<(TaxFiling, SubmitTaxFiling)> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FILE)?;
        let company_id = acting_company(&ctx, &_tenant, &req, &caller)?;
        let input = body.into_inner();
        let filing = state
            .filings
            .get(&filing_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Tax filing not found"))?;
        if filing.company_id != company_id {
            return Err(UnifiedError::not_found("Tax filing not found"));
        }
        if filing.status == "submitted" {
            return Err(UnifiedError::conflict("Filing is already submitted"));
        }
        Ok((filing, input))
    })();
    let (mut filing, input) = match result {
        Ok(values) => values,
        Err(error) => return json::error_response(&error, false),
    };
    let (submitted_via, reference) = if let Some(connector_id) = input.connector_id {
        let connector = match state.connectors.get(connector_id.trim()) {
            Some(entry) => entry.value().clone(),
            None => {
                return json::error_response(
                    &UnifiedError::not_found("Tax connector not found"),
                    false,
                );
            }
        };
        if connector.company_id != filing.company_id {
            return json::error_response(
                &UnifiedError::not_found("Tax connector not found"),
                false,
            );
        }
        if !connector.enabled {
            return json::error_response(
                &UnifiedError::conflict("Tax connector is disabled"),
                false,
            );
        }
        match submit_through_connector(&ctx, &connector, &filing).await {
            Ok(outcome) => outcome,
            Err(error) => return json::error_response(&error, false),
        }
    } else {
        (
            "manual".to_string(),
            input
                .reference
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| format!("MANUAL-{}", filing.id)),
        )
    };
    filing.status = "submitted".to_string();
    filing.submitted_at = Some(now());
    filing.submitted_via = Some(submitted_via.clone());
    filing.reference = Some(reference.clone());
    filing.updated_at = now();
    state.filings.insert(filing.id.clone(), filing.clone());
    let verified = unified_core::caller::caller_from(&req);
    let caller_actor = if verified.is_authenticated() {
        verified.user_id.clone()
    } else {
        "unknown".to_string()
    };
    state.record_audit(
        filing.company_id,
        &caller_actor,
        "filing_submitted",
        "tax_filing",
        &filing.id,
        "tax.filing_submitted",
        json!({
            "country_code": filing.country_code,
            "period_start": filing.period_start,
            "period_end": filing.period_end,
            "submitted_via": submitted_via,
            "reference": reference,
        }),
    );
    json::respond(Ok::<TaxFiling, UnifiedError>(filing), false)
}
