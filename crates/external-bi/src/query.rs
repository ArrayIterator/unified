//! BI external module.

use super::*;


// Query engine (small, honest: filters / grouping / aggregations over
// one snapshot's in-memory rows - never a SQL engine)

fn field_type_of<'a>(dataset: &'a Dataset, name: &str) -> Option<FieldType> {
    dataset
        .fields
        .iter()
        .find(|field| field.name == name)
        .map(|field| field.field_type)
}

fn compare_values(
    field_type: FieldType,
    op: &str,
    cell: &Value,
    filter_value: &Value,
) -> Result<bool, String> {
    match field_type {
        FieldType::String => {
            let left = cell
                .as_str()
                .ok_or_else(|| "filter cell is not a string".to_string())?;
            let right = filter_value
                .as_str()
                .ok_or_else(|| "filter value for a string field must be a string".to_string())?;
            Ok(match op {
                "=" => left == right,
                "!=" => left != right,
                ">" => left > right,
                "<" => left < right,
                ">=" => left >= right,
                "<=" => left <= right,
                "contains" => left.contains(right),
                _ => return Err(format!("unsupported operator {op:?}")),
            })
        }
        FieldType::Number => {
            let left = cell
                .as_f64()
                .ok_or_else(|| "filter cell is not a number".to_string())?;
            let right = filter_value
                .as_f64()
                .ok_or_else(|| "filter value for a number field must be a number".to_string())?;
            Ok(match op {
                "=" => left == right,
                "!=" => left != right,
                ">" => left > right,
                "<" => left < right,
                ">=" => left >= right,
                "<=" => left <= right,
                "contains" => {
                    return Err("operator \"contains\" only applies to string fields".to_string());
                }
                _ => return Err(format!("unsupported operator {op:?}")),
            })
        }
        FieldType::Boolean => {
            let left = cell
                .as_bool()
                .ok_or_else(|| "filter cell is not a boolean".to_string())?;
            let right = filter_value
                .as_bool()
                .ok_or_else(|| "filter value for a boolean field must be a boolean".to_string())?;
            Ok(match op {
                "=" => left == right,
                "!=" => left != right,
                _ => {
                    return Err(format!(
                        "operator {op:?} is not supported for boolean fields"
                    ));
                }
            })
        }
        FieldType::Datetime => {
            let parse = |value: &Value| -> Result<DateTime<Utc>, String> {
                value
                    .as_str()
                    .ok_or_else(|| "datetime value must be an RFC 3339 string".to_string())
                    .and_then(|text| {
                        DateTime::parse_from_rfc3339(text)
                            .map(|dt| dt.with_timezone(&Utc))
                            .map_err(|_| format!("invalid RFC 3339 datetime {text:?}"))
                    })
            };
            let left = parse(cell)?;
            let right = parse(filter_value)?;
            Ok(match op {
                "=" => left == right,
                "!=" => left != right,
                ">" => left > right,
                "<" => left < right,
                ">=" => left >= right,
                "<=" => left <= right,
                "contains" => {
                    return Err(
                        "operator \"contains\" only applies to string fields".to_string()
                    );
                }
                _ => return Err(format!("unsupported operator {op:?}")),
            })
        }
    }
}

fn group_key(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::String(text) => text.clone(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        _ => serde_json::to_string(value).unwrap_or_default(),
    }
}

fn agg_output_name(agg: &str, field: Option<&str>) -> String {
    match field {
        Some(name) => format!("{agg}_{name}"),
        None => agg.to_string(),
    }
}

fn sort_key(value: &Value) -> (u8, String, f64) {
    // Orderable key: nulls first, then numbers, then everything else
    // by its string form. Keeps mixed-type sorts total and stable.
    match value {
        Value::Null => (0, String::new(), 0.0),
        Value::Number(number) => (1, String::new(), number.as_f64().unwrap_or(0.0)),
        other => (2, group_key(other), 0.0),
    }
}

/// Run a query over one snapshot. Returns the computed result plus
/// the row count and the snapshot it ran against.
pub fn apply_query(
    dataset: &Dataset,
    snapshot: &Snapshot,
    request: &QueryRequest,
) -> Result<Value, UnifiedError> {
    const OPS: [&str; 7] = ["=", "!=", ">", "<", ">=", "<=", "contains"];
    for filter in request.filters.as_deref().unwrap_or_default() {
        let Some(field_type) = field_type_of(dataset, &filter.field) else {
            return Err(bad_request(format!(
                "unknown field {:?} in filter",
                filter.field
            )));
        };
        if !OPS.contains(&filter.op.as_str()) {
            return Err(bad_request(format!(
                "unsupported operator {:?}",
                filter.op
            )));
        }
        if filter.op == "contains" && field_type != FieldType::String {
            return Err(bad_request(
                "operator \"contains\" only applies to string fields",
            ));
        }
    }
    if let Some(group_by) = &request.group_by {
        if field_type_of(dataset, group_by).is_none() {
            return Err(bad_request(format!("unknown group_by field {group_by:?}")));
        }
    }
    let aggs = request.aggregations.as_deref().unwrap_or_default();
    let mut seen_aggs = HashSet::new();
    for agg in aggs {
        match agg.agg.as_str() {
            "count" => {
                if agg.field.is_some() {
                    return Err(bad_request(
                        "aggregation \"count\" takes no field",
                    ));
                }
            }
            "sum" | "avg" | "min" | "max" => {
                let Some(field) = &agg.field else {
                    return Err(bad_request(format!(
                        "aggregation {:?} requires a field",
                        agg.agg
                    )));
                };
                match field_type_of(dataset, field) {
                    Some(FieldType::Number) => {}
                    Some(_) => {
                        return Err(bad_request(format!(
                            "aggregation {:?} requires a number field, {:?} is not one",
                            agg.agg, field
                        )));
                    }
                    None => {
                        return Err(bad_request(format!(
                            "unknown field {:?} in aggregation",
                            field
                        )));
                    }
                }
            }
            other => {
                return Err(bad_request(format!("unsupported aggregation {other:?}")));
            }
        }
        if !seen_aggs.insert((agg.agg.clone(), agg.field.clone())) {
            return Err(bad_request(format!(
                "duplicate aggregation {:?} on {:?}",
                agg.agg, agg.field
            )));
        }
    }
    let limit = request.limit.unwrap_or(DEFAULT_QUERY_LIMIT);
    if limit == 0 || limit > MAX_QUERY_LIMIT {
        return Err(bad_request(format!(
            "limit must be between 1 and {MAX_QUERY_LIMIT}"
        )));
    }

    // Filter.
    let mut matched: Vec<&Value> = Vec::new();
    'rows: for row in &snapshot.rows {
        for filter in request.filters.as_deref().unwrap_or_default() {
            let field_type = field_type_of(dataset, &filter.field)
                .expect("filter fields validated above");
            let cell = row.get(&filter.field).unwrap_or(&Value::Null);
            match compare_values(field_type, &filter.op, cell, &filter.value) {
                Ok(true) => {}
                Ok(false) => continue 'rows,
                Err(reason) => return Err(bad_request(reason)),
            }
        }
        matched.push(row);
    }
    let rows_matched = matched.len();

    // Compute result rows: raw rows, or one row per group (or one row
    // total) with aggregations.
    let mut results: Vec<Value> = Vec::new();
    if aggs.is_empty() && request.group_by.is_none() {
        for row in &matched {
            results.push((*row).clone());
        }
    } else {
        // Group matched rows. Without group_by there is a single
        // group keyed by the empty string.
        let mut groups: Vec<(String, Value, Vec<&Value>)> = Vec::new();
        let mut index_by_key: HashMap<String, usize> = HashMap::new();
        for row in &matched {
            let (key, display) = match &request.group_by {
                Some(group_by) => {
                    let cell = row.get(group_by).unwrap_or(&Value::Null);
                    (group_key(cell), cell.clone())
                }
                None => (String::new(), Value::Null),
            };
            match index_by_key.get(&key) {
                Some(&index) => groups[index].2.push(row),
                None => {
                    index_by_key.insert(key.clone(), groups.len());
                    groups.push((key, display, vec![row]));
                }
            }
        }
        // An aggregation with no groups still yields one result row
        // (count 0, other aggregates null).
        if groups.is_empty() {
            groups.push((String::new(), Value::Null, Vec::new()));
        }
        for (_, display, rows) in &groups {
            let mut out = Map::new();
            if let Some(group_by) = &request.group_by {
                out.insert(group_by.clone(), display.clone());
            }
            for agg in aggs {
                let name = agg_output_name(&agg.agg, agg.field.as_deref());
                let value = match agg.agg.as_str() {
                    "count" => json!(rows.len()),
                    _ => {
                        let field = agg.field.as_deref().expect("validated above");
                        let numbers: Vec<f64> = rows
                            .iter()
                            .filter_map(|row| row.get(field).and_then(Value::as_f64))
                            .collect();
                        if numbers.is_empty() {
                            Value::Null
                        } else {
                            match agg.agg.as_str() {
                                "sum" => json!(numbers.iter().sum::<f64>()),
                                "avg" => {
                                    json!(numbers.iter().sum::<f64>() / numbers.len() as f64)
                                }
                                "min" => json!(numbers
                                    .iter()
                                    .cloned()
                                    .fold(f64::INFINITY, f64::min)),
                                "max" => json!(numbers
                                    .iter()
                                    .cloned()
                                    .fold(f64::NEG_INFINITY, f64::max)),
                                _ => unreachable!(),
                            }
                        }
                    }
                };
                out.insert(name, value);
            }
            // group_by with no aggregations: report the group size.
            if aggs.is_empty() {
                out.insert("count".to_string(), json!(rows.len()));
            }
            results.push(Value::Object(out));
        }
    }

    // Sort.
    if let Some(sort) = &request.sort {
        let direction = sort.direction.as_deref().unwrap_or("asc");
        if direction != "asc" && direction != "desc" {
            return Err(bad_request("sort direction must be \"asc\" or \"desc\""));
        }
        if !results.is_empty() && !results[0].get(&sort.field).is_some() {
            return Err(bad_request(format!(
                "unknown sort field {:?}",
                sort.field
            )));
        }
        results.sort_by(|a, b| {
            let left = a.get(&sort.field).unwrap_or(&Value::Null);
            let right = b.get(&sort.field).unwrap_or(&Value::Null);
            let (lk, ls, ln) = sort_key(left);
            let (rk, rs, rn) = sort_key(right);
            let ord = lk
                .cmp(&rk)
                .then_with(|| ls.cmp(&rs))
                .then_with(|| ln.partial_cmp(&rn).unwrap_or(std::cmp::Ordering::Equal));
            if direction == "desc" {
                ord.reverse()
            } else {
                ord
            }
        });
    }
    results.truncate(limit);

    Ok(json!({
        "dataset_id": &dataset.id,
        "snapshot_id": &snapshot.id,
        "schema_version": snapshot.schema_version,
        "extracted_at": &snapshot.extracted_at,
        "rows_scanned": snapshot.rows.len(),
        "rows_matched": rows_matched,
        "row_count": results.len(),
        "results": results,
    }))
}
