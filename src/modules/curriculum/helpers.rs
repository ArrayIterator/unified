//! Curriculum module - competency-based school curricula. - Helpers.
//!
//! Split from `src/modules/curriculum.rs`; behaviour unchanged.

use super::ai_capabilities::*;
use super::assessment_domains::*;
use super::basic_competencies::*;
use super::constants::*;
use super::core_competencies::*;
use super::frameworks::*;
use super::grade_component_mappings::*;
use super::grade_descriptors::*;
use super::hooks::*;
use super::inputs::*;
use super::interventions::*;
use super::lesson_plans::*;
use super::levels::*;
use super::module::*;
use super::report_card_entries::*;
use super::report_cards::*;
use super::settings::*;
use super::state::*;
use super::structures::*;
use super::subjects::*;
use super::types::*;

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, Pagination, PaginationParams};
use actix_web::HttpRequest;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::Serialize;
use serde_json::{json, Value};
use uuid::Uuid;

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the e-learning module.
pub(super) struct Caller {
    pub(super) actor: String,
    pub(super) role: String,
}

pub(super) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
pub(super) fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub(super) fn require_permission(
    ctx: &AppContext,
    role: &str,
    permission: &str,
) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

// Tenant edge guard (C3 pilot 4, group B).

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller` or any capability check.
///
/// Records in this module carry no company field, so the guard is a
/// fail-closed gate only: no per-record company check is applied.
/// Real partitioning needs a `tenant_id` on the records, a C11
/// follow-up.
pub(super) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

/// The company scope the caller acts in: the tenant context's
/// company first, the verified caller's server-resolved company
/// otherwise. Fails closed (403) when both are present and disagree
/// -- a request must not carry one company in its tenant context and
/// another in its verified identity. The `X-Company-Id` header is
/// NEVER trusted here: only the verified caller (company resolved
/// from the database at authentication time) and the
/// middleware-resolved tenant context are sources.
pub(super) fn caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<Option<String>> {
    let verified = unified_core::caller::caller_from(req)
        .company_id
        .filter(|value| !value.is_empty());
    match (&tenant.company_id, &verified) {
        (Some(tenant_company), Some(verified_company))
            if tenant_company != verified_company =>
        {
            Err(UnifiedError::forbidden(
                "Verified caller company does not match the tenant company scope",
            ))
        }
        (Some(tenant_company), _) => Ok(Some(tenant_company.clone())),
        (None, verified) => Ok(verified.clone()),
    }
}

/// True when the caller may manage curriculum content: the manage
/// capability or the teacher capability.
pub(super) fn can_teach(ctx: &AppContext, role: &str) -> bool {
    check_capability(ctx, role, PERM_CURRICULUM_MANAGE)
        || check_capability(ctx, role, PERM_CURRICULUM_TEACH)
}

pub(super) fn require_teach(ctx: &AppContext, role: &str) -> UnifiedResult<()> {
    if can_teach(ctx, role) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{PERM_CURRICULUM_MANAGE}\" or \"{PERM_CURRICULUM_TEACH}\""
        )))
    }
}

pub(super) fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

pub(super) fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

pub(super) fn optional_text(
    value: &Option<String>,
    field: &str,
    max: usize,
) -> UnifiedResult<Option<String>> {
    match value {
        Some(raw) => Ok(Some(require_text(raw, field, max)?)),
        None => Ok(None),
    }
}

/// A machine code: lowercase letters, digits, and hyphens only.
pub(super) fn require_code(value: &str, field: &str) -> UnifiedResult<String> {
    let code = require_text(value, field, MAX_CODE_LEN)?;
    if !code
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err(UnifiedError::bad_request(format!(
            "{field} must use lowercase letters, digits, hyphens, or underscores"
        )));
    }
    Ok(code)
}

/// Validate a value against a settings list. The list is the live
/// admin-configured one - never a constant in this file.
pub(super) fn require_in_list(
    value: &str,
    field: &str,
    allowed: &[String],
) -> UnifiedResult<String> {
    let checked = require_text(value, field, MAX_CODE_LEN)?;
    if !allowed.iter().any(|item| item == &checked) {
        return Err(UnifiedError::bad_request(format!(
            "{field} \"{checked}\" is not allowed; allowed values: {}",
            allowed.join(", ")
        )));
    }
    Ok(checked)
}

pub(super) fn require_score(value: f64, field: &str) -> UnifiedResult<f64> {
    if !value.is_finite() || value < MIN_SCORE || value > MAX_SCORE {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be between {MIN_SCORE} and {MAX_SCORE}"
        )));
    }
    Ok(value)
}

pub(super) fn require_string_list(
    values: &[String],
    field: &str,
    max_item: usize,
) -> UnifiedResult<Vec<String>> {
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        out.push(require_text(value, field, max_item)?);
    }
    Ok(out)
}

pub(super) fn optional_uuid(raw: &Option<String>, what: &str) -> UnifiedResult<Option<Uuid>> {
    match raw {
        Some(value) if !value.trim().is_empty() => Ok(Some(parse_uuid(value.trim(), what)?)),
        _ => Ok(None),
    }
}

pub(super) fn required_uuid(raw: &Option<String>, what: &str) -> UnifiedResult<Uuid> {
    match raw {
        Some(value) => parse_uuid(value.trim(), what),
        None => Err(UnifiedError::bad_request(format!("{what} id is required"))),
    }
}

/// Shared list behaviour: free-text search plus exact-match filters
/// from the query, sorted by creation time, paginated.
pub(super) trait Listable {
    fn search_text(&self) -> String;
    fn filter_value(&self, key: &str) -> Option<String>;
    fn created_at(&self) -> DateTime<Utc>;
}

pub(super) fn list_response<T>(items: Vec<T>, params: &PaginationParams) -> Value
where
    T: Listable + Clone + Serialize,
{
    let pagination: Pagination = params.normalized();
    let search = pagination
        .search
        .clone()
        .unwrap_or_default()
        .to_lowercase();
    let mut items: Vec<T> = items
        .into_iter()
        .filter(|item| {
            if !search.is_empty() && !item.search_text().to_lowercase().contains(&search) {
                return false;
            }
            pagination.filters.iter().all(|(key, value)| {
                item.filter_value(key).is_some_and(|found| &found == value)
            })
        })
        .collect();
    items.sort_by(|a, b| {
        let ord = a.created_at().cmp(&b.created_at());
        if pagination.order == "desc" {
            ord.reverse()
        } else {
            ord
        }
    });
    let total = items.len() as u64;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(((pagination.page - 1) * pagination.per_page) as usize)
        .take(pagination.per_page as usize)
        .collect();
    let page = PageResult::new(page_items, total, &pagination);
    json!(page)
}

pub(super) fn get_record<T: Clone>(
    map: &DashMap<Uuid, T>,
    id: &Uuid,
    what: &str,
) -> UnifiedResult<T> {
    map.get(id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("{what} not found: {id}")))
}

pub(super) fn now() -> DateTime<Utc> {
    Utc::now()
}

/// Validate a student reference against the academic module when it
/// is installed. The lookup is a hook query: when no handler answers
/// (academic module absent) the reference is accepted; when a
/// handler answers `exists: false` the request is rejected.
///
/// The answer is read from the hook RESULTS (the tree's hook-query
/// convention - see the logistics module's `lookup_order`), with a
/// fallback to the payload `exists` key for answerers that set it
/// there instead. Reading the payload alone is wrong: the emitted
/// payload is the request, not the answer. A confirmed student is
/// cached in `known_students` so repeat validations stay local.
pub(super) fn check_student_reference(
    ctx: &AppContext,
    state: &CurriculumState,
    student_ref: &str,
) -> UnifiedResult<()> {
    if state.known_students.contains_key(student_ref) {
        return Ok(());
    }
    if let Some(context) = ctx.hooks.emit_if_registered(
        "academic.student.lookup",
        json!({ "student_ref": student_ref }),
    ) {
        let answered = context
            .results
            .iter()
            .find_map(|result| {
                result
                    .get("exists")
                    .or_else(|| result.get("found"))
                    .and_then(|value| value.as_bool())
            })
            .or_else(|| context.get("exists").and_then(|value| value.as_bool()));
        match answered {
            Some(true) => {
                let program_id = context
                    .results
                    .iter()
                    .find_map(|result| {
                        result
                            .get("program_id")
                            .and_then(|value| value.as_str())
                            .map(str::to_string)
                    });
                state.known_students.insert(
                    student_ref.to_string(),
                    KnownStudent {
                        student_ref: student_ref.to_string(),
                        level_id: None,
                        program_id,
                        source_event: "academic.student.lookup".to_string(),
                        seen_at: now(),
                    },
                );
                return Ok(());
            }
            Some(false) => {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown student reference: {student_ref}"
                )));
            }
            None => {
                // A registered handler answered with no usable
                // `exists` value: treat it as no answer (standalone
                // tolerance) rather than rejecting a reference the
                // academic module did not actually deny.
            }
        }
    }
    Ok(())
}

/// Find the grade band label for a score from the configured bands.
///
/// Bands match as half-open intervals `[min_score, max_score)`, so
/// contiguous bands cover every score with no gaps; the top edge is
/// inclusive, so a score of exactly `MAX_SCORE` (100) still labels
/// with the band whose `max_score` is 100. A score outside every
/// band (possible only with admin-configured gaps) returns `None`.
pub(super) fn grade_label_for(settings: &CurriculumSettings, score: f64) -> Option<String> {
    settings
        .default_grade_bands
        .iter()
        .find(|band| {
            score >= band.min_score
                && (score < band.max_score
                    || (score == band.max_score && score == MAX_SCORE))
        })
        .map(|band| band.label.clone())
}

/// Find the stored descriptor text for a score, optionally scoped to
/// one assessment domain; falls back to the unscoped descriptor.
///
/// A scoped match (a row whose `domain_id` is the requested domain)
/// wins immediately. Unscoped rows are collected and resolved
/// deterministically: the narrowest score range wins, then the
/// earliest created row, then the smallest id - so the result never
/// depends on DashMap iteration order.
pub(super) fn descriptor_for(
    state: &CurriculumState,
    framework_id: Uuid,
    domain_id: Option<Uuid>,
    score: f64,
) -> Option<String> {
    let mut best: Option<GradeDescriptor> = None;
    for entry in state.grade_descriptors.iter() {
        let row = entry.value();
        if row.framework_id != framework_id || score < row.min_score || score > row.max_score {
            continue;
        }
        if domain_id.is_some() && row.domain_id == domain_id {
            return Some(row.descriptor.clone());
        }
        if row.domain_id.is_none() {
            let better = match &best {
                None => true,
                Some(current) => {
                    let row_width = row.max_score - row.min_score;
                    let current_width = current.max_score - current.min_score;
                    row_width < current_width
                        || (row_width == current_width && row.created_at < current.created_at)
                        || (row_width == current_width
                            && row.created_at == current.created_at
                            && row.id < current.id)
                }
            };
            if better {
                best = Some(row.clone());
            }
        }
    }
    best.map(|row| row.descriptor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn require_code_accepts_lowercase_digits_hyphens_underscores() {
        assert_eq!(require_code("math-101", "code").unwrap(), "math-101");
        assert_eq!(require_code("grade_1", "code").unwrap(), "grade_1");
        assert!(require_code("Math", "code").is_err());
        assert!(require_code("a b", "code").is_err());
        assert!(require_code("", "code").is_err());
        assert!(require_code("a.b", "code").is_err());
    }

    #[test]
    fn require_in_list_checks_membership() {
        let allowed = vec!["a4".to_string(), "letter".to_string()];
        assert_eq!(
            require_in_list(" a4 ", "paper_size", &allowed).unwrap(),
            "a4"
        );
        assert!(require_in_list("a3", "paper_size", &allowed).is_err());
        assert!(require_in_list("", "paper_size", &allowed).is_err());
    }

    #[test]
    fn require_score_bounds() {
        assert_eq!(require_score(0.0, "score").unwrap(), 0.0);
        assert_eq!(require_score(100.0, "score").unwrap(), 100.0);
        assert_eq!(require_score(55.5, "score").unwrap(), 55.5);
        assert!(require_score(-0.01, "score").is_err());
        assert!(require_score(100.01, "score").is_err());
        assert!(require_score(f64::NAN, "score").is_err());
    }

    #[test]
    fn require_string_list_validates_each_item() {
        let values = vec!["a".to_string(), " b ".to_string()];
        assert_eq!(
            require_string_list(&values, "items", 10).unwrap(),
            vec!["a".to_string(), "b".to_string()]
        );
        assert!(require_string_list(&["".to_string()], "items", 10).is_err());
        assert!(require_string_list(&["toolong".to_string()], "items", 3).is_err());
        assert!(require_string_list(&[], "items", 10).unwrap().is_empty());
    }

    #[test]
    fn optional_text_empty_string_is_an_error_here() {
        assert_eq!(optional_text(&None, "note", 10).unwrap(), None);
        assert_eq!(
            optional_text(&Some(" x ".to_string()), "note", 10).unwrap(),
            Some("x".to_string())
        );
        assert!(optional_text(&Some("".to_string()), "note", 10).is_err());
    }

    #[test]
    fn optional_and_required_uuid() {
        let raw = "123e4567-e89b-12d3-a456-426614174000";
        assert_eq!(optional_uuid(&None, "framework").unwrap(), None);
        assert_eq!(
            optional_uuid(&Some("  ".to_string()), "framework").unwrap(),
            None
        );
        assert_eq!(
            optional_uuid(&Some(raw.to_string()), "framework").unwrap(),
            Some(Uuid::parse_str(raw).unwrap())
        );
        assert!(optional_uuid(&Some("bad".to_string()), "framework").is_err());
        assert_eq!(
            required_uuid(&Some(raw.to_string()), "framework").unwrap(),
            Uuid::parse_str(raw).unwrap()
        );
        assert!(required_uuid(&None, "framework").is_err());
    }

    #[test]
    fn grade_label_for_default_bands() {
        let settings = CurriculumSettings::default();
        assert_eq!(grade_label_for(&settings, 95.0).as_deref(), Some("A"));
        assert_eq!(grade_label_for(&settings, 90.0).as_deref(), Some("A"));
        assert_eq!(grade_label_for(&settings, 85.0).as_deref(), Some("B"));
        assert_eq!(grade_label_for(&settings, 89.99).as_deref(), Some("B"));
        assert_eq!(grade_label_for(&settings, 75.0).as_deref(), Some("C"));
        assert_eq!(grade_label_for(&settings, 65.0).as_deref(), Some("D"));
        assert_eq!(grade_label_for(&settings, 50.0).as_deref(), Some("E"));
        assert_eq!(grade_label_for(&settings, 0.0).as_deref(), Some("E"));
        assert_eq!(grade_label_for(&settings, 100.0).as_deref(), Some("A"));
        // The bands are contiguous half-open intervals, so every
        // score in [0, 100] labels - including values that used to
        // fall in the gaps between bands (89.995 matched no band).
        assert_eq!(grade_label_for(&settings, 89.995).as_deref(), Some("B"));
        assert_eq!(grade_label_for(&settings, 89.9999).as_deref(), Some("B"));
        assert_eq!(grade_label_for(&settings, 79.999).as_deref(), Some("C"));
        assert_eq!(grade_label_for(&settings, 59.999).as_deref(), Some("E"));
        assert_eq!(grade_label_for(&settings, 60.0).as_deref(), Some("D"));
    }

    #[test]
    fn descriptor_for_prefers_scoped_then_unscoped() {
        let state = CurriculumState::default();
        let framework_id = Uuid::new_v4();
        let domain_id = Uuid::new_v4();
        let other_domain = Uuid::new_v4();
        let now = Utc::now();
        let unscoped = GradeDescriptor {
            id: Uuid::new_v4(),
            framework_id,
            domain_id: None,
            min_score: 0.0,
            max_score: 100.0,
            grade_label: "E".to_string(),
            descriptor: "Overall".to_string(),
            created_at: now,
            updated_at: now,
        };
        let scoped = GradeDescriptor {
            id: Uuid::new_v4(),
            framework_id,
            domain_id: Some(domain_id),
            min_score: 80.0,
            max_score: 100.0,
            grade_label: "A".to_string(),
            descriptor: "Scoped".to_string(),
            created_at: now,
            updated_at: now,
        };
        state.grade_descriptors.insert(unscoped.id, unscoped);
        state.grade_descriptors.insert(scoped.id, scoped);
        assert_eq!(
            descriptor_for(&state, framework_id, Some(domain_id), 90.0).as_deref(),
            Some("Scoped")
        );
        assert_eq!(
            descriptor_for(&state, framework_id, Some(other_domain), 50.0).as_deref(),
            Some("Overall")
        );
        assert_eq!(
            descriptor_for(&state, framework_id, None, 50.0).as_deref(),
            Some("Overall")
        );
        assert_eq!(descriptor_for(&state, Uuid::new_v4(), None, 50.0), None);
    }

    #[test]
    fn descriptor_for_picks_narrowest_unscoped_deterministically() {
        let state = CurriculumState::default();
        let framework_id = Uuid::new_v4();
        let base = Utc::now();
        // Wide unscoped row created first, narrow one created later:
        // the narrow row must win no matter which order the DashMap
        // happens to iterate them in.
        let wide = GradeDescriptor {
            id: Uuid::new_v4(),
            framework_id,
            domain_id: None,
            min_score: 0.0,
            max_score: 100.0,
            grade_label: "E".to_string(),
            descriptor: "Wide".to_string(),
            created_at: base,
            updated_at: base,
        };
        let narrow = GradeDescriptor {
            id: Uuid::new_v4(),
            framework_id,
            domain_id: None,
            min_score: 80.0,
            max_score: 100.0,
            grade_label: "A".to_string(),
            descriptor: "Narrow".to_string(),
            created_at: base + chrono::Duration::seconds(10),
            updated_at: base,
        };
        state.grade_descriptors.insert(wide.id, wide);
        state.grade_descriptors.insert(narrow.id, narrow);
        assert_eq!(
            descriptor_for(&state, framework_id, None, 90.0).as_deref(),
            Some("Narrow")
        );
        // A score only the wide row covers still falls back to it.
        assert_eq!(
            descriptor_for(&state, framework_id, None, 50.0).as_deref(),
            Some("Wide")
        );
    }

    #[test]
    fn descriptor_for_ties_break_by_earliest_created_at() {
        let state = CurriculumState::default();
        let framework_id = Uuid::new_v4();
        let base = Utc::now();
        // Same range, different creation times: the earliest row
        // wins, regardless of insertion or iteration order.
        let newer = GradeDescriptor {
            id: Uuid::new_v4(),
            framework_id,
            domain_id: None,
            min_score: 0.0,
            max_score: 100.0,
            grade_label: "E".to_string(),
            descriptor: "Newer".to_string(),
            created_at: base + chrono::Duration::seconds(10),
            updated_at: base,
        };
        let older = GradeDescriptor {
            id: Uuid::new_v4(),
            framework_id,
            domain_id: None,
            min_score: 0.0,
            max_score: 100.0,
            grade_label: "E".to_string(),
            descriptor: "Older".to_string(),
            created_at: base,
            updated_at: base,
        };
        state.grade_descriptors.insert(newer.id, newer);
        state.grade_descriptors.insert(older.id, older);
        assert_eq!(
            descriptor_for(&state, framework_id, None, 50.0).as_deref(),
            Some("Older")
        );
    }
}
