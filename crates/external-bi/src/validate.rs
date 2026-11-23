//! BI external module (split from lib.rs; moved verbatim).

use super::*;


// ---------------------------------------------------------------------------
// Validation helpers (pure, unit-tested)
// ---------------------------------------------------------------------------

/// Field names: 1-64 chars, start with a letter or `_`, then letters,
/// digits, or `_`.
pub fn is_valid_field_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 64 {
        return false;
    }
    let mut chars = name.chars();
    let first = chars.next().expect("non-empty checked above");
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

pub(crate) fn validate_field_defs(fields: &[FieldDef]) -> Result<(), UnifiedError> {
    if fields.is_empty() {
        return Err(bad_request("fields must contain at least one field"));
    }
    if fields.len() > 100 {
        return Err(bad_request("fields must contain at most 100 fields"));
    }
    let mut seen = HashSet::new();
    for field in fields {
        if !is_valid_field_name(&field.name) {
            return Err(bad_request(format!(
                "invalid field name {:?}: use 1-64 chars, letters, digits, '_'",
                field.name
            )));
        }
        if !seen.insert(field.name.clone()) {
            return Err(bad_request(format!(
                "duplicate field name {:?}",
                field.name
            )));
        }
    }
    Ok(())
}

pub(crate) fn validate_tenant_id(id: &str) -> Result<String, UnifiedError> {
    let id = id.trim();
    if id.is_empty()
        || id.len() > 128
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(bad_request(
            "tenant_id must be 1-128 chars of letters, digits, '-' or '_'",
        ));
    }
    Ok(id.to_string())
}

/// Resolve the tenant for a request: `X-Unified-Tenant` header wins,
/// otherwise the body's `tenant_id`. A body value that disagrees with
/// the header is rejected. At least one of them is required.
pub(crate) fn resolve_tenant(req: &HttpRequest, body_tenant: Option<&str>) -> Result<String, UnifiedError> {
    let header = req
        .headers()
        .get("x-unified-tenant")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let body = body_tenant.map(str::trim).filter(|value| !value.is_empty());
    match (header, body) {
        (Some(h), Some(b)) if h != b => Err(bad_request(
            "tenant_id in body does not match the X-Unified-Tenant header",
        )),
        (Some(h), _) => validate_tenant_id(h),
        (None, Some(b)) => validate_tenant_id(b),
        (None, None) => Err(bad_request(
            "tenant_id is required: send the X-Unified-Tenant header or a tenant_id field",
        )),
    }
}

/// Validate one row against the declared fields. Returns the row
/// stripped to the declared fields (extra keys are dropped, never
/// stored), or a human-readable reason for rejection.
pub fn validate_row(fields: &[FieldDef], row: &Value) -> Result<Value, String> {
    let object = match row.as_object() {
        Some(object) => object,
        None => return Err("row is not a JSON object".to_string()),
    };
    let mut clean = Map::new();
    for field in fields {
        let Some(value) = object.get(&field.name) else {
            return Err(format!("missing required field {:?}", field.name));
        };
        if value.is_null() {
            return Err(format!("field {:?} must not be null", field.name));
        }
        let ok = match field.field_type {
            FieldType::String => value.is_string(),
            FieldType::Number => value.is_number(),
            FieldType::Boolean => value.is_boolean(),
            FieldType::Datetime => match value.as_str() {
                Some(text) => DateTime::parse_from_rfc3339(text).is_ok(),
                None => false,
            },
        };
        if !ok {
            return Err(format!(
                "field {:?} must be a {} value",
                field.name,
                field.field_type.as_str()
            ));
        }
        clean.insert(field.name.clone(), value.clone());
    }
    Ok(Value::Object(clean))
}

// ---------------------------------------------------------------------------
// Cron expressions (shape validation + minute matching; no daemon)
// ---------------------------------------------------------------------------

const CRON_RANGES: [(u32, u32); 5] = [(0, 59), (0, 23), (1, 31), (1, 12), (0, 7)];

fn parse_cron_field(part: &str, min: u32, max: u32) -> Result<HashSet<u32>, String> {
    let mut values = HashSet::new();
    for item in part.split(',') {
        if item.is_empty() {
            return Err("empty item in cron field".to_string());
        }
        let (base, step) = match item.split_once('/') {
            Some((base, step)) => {
                let step: u32 = step
                    .parse()
                    .map_err(|_| format!("invalid cron step {step:?}"))?;
                if step == 0 || step > (max - min + 1) {
                    return Err(format!("cron step {step} out of range"));
                }
                (base, step)
            }
            None => (item, 1),
        };
        let (lo, hi) = if base == "*" {
            (min, max)
        } else if let Some((a, b)) = base.split_once('-') {
            let a: u32 = a
                .parse()
                .map_err(|_| format!("invalid cron value {a:?}"))?;
            let b: u32 = b
                .parse()
                .map_err(|_| format!("invalid cron value {b:?}"))?;
            if a > b {
                return Err(format!("cron range {a}-{b} is reversed"));
            }
            (a, b)
        } else {
            let value: u32 = base
                .parse()
                .map_err(|_| format!("invalid cron value {base:?}"))?;
            (value, value)
        };
        if lo < min || hi > max {
            return Err(format!(
                "cron value {lo}-{hi} out of range {min}-{max}"
            ));
        }
        let mut value = lo;
        while value <= hi {
            values.insert(value);
            value += step;
        }
    }
    Ok(values)
}

/// Validate a 5-field cron expression (`minute hour day-of-month
/// month day-of-week`). Shape only: ranges and steps are checked, but
/// nothing is scheduled by this module.
pub fn validate_cron(expr: &str) -> Result<(), String> {
    let parts: Vec<&str> = expr.split_whitespace().collect();
    if parts.len() != 5 {
        return Err("cron expression must have exactly 5 fields".to_string());
    }
    for (index, part) in parts.iter().enumerate() {
        let (min, max) = CRON_RANGES[index];
        parse_cron_field(part, min, max)?;
    }
    Ok(())
}

fn cron_field_is_wildcard(part: &str) -> bool {
    part == "*"
}

/// True when `dt` matches the expression, using standard cron
/// semantics: when both day-of-month and day-of-week are restricted,
/// either one matching is enough.
pub fn cron_matches(expr: &str, dt: DateTime<Utc>) -> bool {
    let parts: Vec<&str> = expr.split_whitespace().collect();
    if parts.len() != 5 {
        return false;
    }
    let mut sets: Vec<HashSet<u32>> = Vec::with_capacity(5);
    for (index, part) in parts.iter().enumerate() {
        let (min, max) = CRON_RANGES[index];
        let Ok(set) = parse_cron_field(part, min, max) else {
            return false;
        };
        sets.push(set);
    }
    let dow = dt.weekday().num_days_from_sunday();
    if !sets[0].contains(&dt.minute()) || !sets[1].contains(&dt.hour()) {
        return false;
    }
    if !sets[3].contains(&dt.month()) {
        return false;
    }
    let dom_restricted = !cron_field_is_wildcard(parts[2]);
    let dow_restricted = !cron_field_is_wildcard(parts[4]);
    let dom_match = sets[2].contains(&dt.day());
    // Cron allows 7 as a second spelling of Sunday.
    let dow_match = sets[4].contains(&dow) || (dow == 0 && sets[4].contains(&7));
    match (dom_restricted, dow_restricted) {
        (true, true) => dom_match || dow_match,
        (true, false) => dom_match,
        (false, true) => dow_match,
        (false, false) => true,
    }
}
