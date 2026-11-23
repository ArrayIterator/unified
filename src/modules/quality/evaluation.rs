//! quality module - Criterion validation, measurement evaluation and transitions.

use unified_core::error::{UnifiedError, UnifiedResult};

use super::constants::ALERT_ACKNOWLEDGED;
use super::constants::ALERT_OPEN;
use super::constants::ALERT_RESOLVED;
use super::constants::CAPA_CLOSED;
use super::constants::CAPA_IN_PROGRESS;
use super::constants::CAPA_OPEN;
use super::constants::CAPA_VERIFIED;
use super::constants::INSP_CANCELLED;
use super::constants::INSP_COMPLETED;
use super::constants::INSP_REQUESTED;
use super::constants::MAX_NOTE_LEN;
use super::helpers::optional_text;
use super::types::Measurement;
use super::types::MeasurementInput;
use super::types::QualityCriterion;
use super::types::QualitySettings;

/// Validate one criterion definition against the live settings.
/// The tolerance shape is checked per kind: numeric criteria need
/// at least one bound and no boolean/text expectation; boolean
/// criteria need `expected_bool`; text criteria need no numeric
/// bounds. A min above a max is rejected.
pub(crate) fn validate_criterion_shape(
    kind: &str,
    min_value: Option<f64>,
    max_value: Option<f64>,
    expected_bool: Option<bool>,
    expected_text: &Option<String>,
    weight: f64,
    settings: &QualitySettings,
) -> UnifiedResult<()> {
    match kind {
        "numeric" => {
            if min_value.is_none() && max_value.is_none() {
                return Err(UnifiedError::bad_request(
                    "A numeric criterion needs at least one of min_value / max_value",
                ));
            }
            if let (Some(min), Some(max)) = (min_value, max_value) {
                if min > max {
                    return Err(UnifiedError::bad_request(
                        "min_value must not be greater than max_value",
                    ));
                }
            }
            if expected_bool.is_some() || expected_text.is_some() {
                return Err(UnifiedError::bad_request(
                    "A numeric criterion must not carry expected_bool / expected_text",
                ));
            }
        }
        "boolean" => {
            if expected_bool.is_none() {
                return Err(UnifiedError::bad_request(
                    "A boolean criterion needs expected_bool",
                ));
            }
            if min_value.is_some() || max_value.is_some() || expected_text.is_some() {
                return Err(UnifiedError::bad_request(
                    "A boolean criterion must not carry min_value / max_value / expected_text",
                ));
            }
        }
        "text" => {
            if min_value.is_some() || max_value.is_some() || expected_bool.is_some() {
                return Err(UnifiedError::bad_request(
                    "A text criterion must not carry min_value / max_value / expected_bool",
                ));
            }
        }
        _ => {
            return Err(UnifiedError::bad_request(format!(
                "Unknown criterion kind \"{kind}\"; allowed kinds: {}",
                settings.criterion_kinds.join(", ")
            )));
        }
    }
    if !weight.is_finite() || weight <= 0.0 {
        return Err(UnifiedError::bad_request(
            "weight must be a positive number",
        ));
    }
    Ok(())
}

/// Evaluate one measurement against its criterion. Returns the
/// evaluated measurement plus the failure cause to record when
/// the criterion failed (the criterion's own configured cause).
/// A missing measurement for a required criterion fails; a
/// missing measurement for an optional criterion is skipped
/// (returns None).
pub(crate) fn evaluate_measurement(
    criterion: &QualityCriterion,
    input: Option<&MeasurementInput>,
) -> UnifiedResult<Option<(Measurement, Option<String>)>> {
    let Some(input) = input else {
        if criterion.required {
            let measurement = Measurement {
                criterion_id: criterion.id,
                criterion_name: criterion.name.clone(),
                value_number: None,
                value_bool: None,
                value_text: None,
                passed: false,
                deviation: None,
                note: Some("Missing required measurement".to_string()),
            };
            return Ok(Some((measurement, criterion.failure_cause.clone())));
        }
        return Ok(None);
    };
    let note = optional_text(&input.note, "measurement note", MAX_NOTE_LEN)?;
    let (passed, deviation) = match criterion.kind.as_str() {
        "numeric" => {
            let value = input.value_number.ok_or_else(|| {
                UnifiedError::bad_request(format!(
                    "Criterion \"{}\" needs a numeric value",
                    criterion.name
                ))
            })?;
            if !value.is_finite() {
                return Err(UnifiedError::bad_request(format!(
                    "Criterion \"{}\" needs a finite numeric value",
                    criterion.name
                )));
            }
            let below = criterion.min_value.map_or(false, |min| value < min);
            let above = criterion.max_value.map_or(false, |max| value > max);
            let deviation = if below {
                Some(value - criterion.min_value.unwrap_or(value))
            } else if above {
                Some(value - criterion.max_value.unwrap_or(value))
            } else {
                None
            };
            (!below && !above, deviation)
        }
        "boolean" => {
            let value = input.value_bool.ok_or_else(|| {
                UnifiedError::bad_request(format!(
                    "Criterion \"{}\" needs a boolean value",
                    criterion.name
                ))
            })?;
            let expected = criterion.expected_bool.unwrap_or(false);
            (value == expected, None)
        }
        "text" => {
            let raw = input.value_text.as_deref().unwrap_or("").trim();
            if raw.is_empty() {
                return Err(UnifiedError::bad_request(format!(
                    "Criterion \"{}\" needs a text value",
                    criterion.name
                )));
            }
            let passed = match &criterion.expected_text {
                Some(expected) => raw.eq_ignore_ascii_case(expected.trim()),
                None => true,
            };
            (passed, None)
        }
        other => {
            return Err(UnifiedError::bad_request(format!(
                "Unknown criterion kind \"{other}\""
            )));
        }
    };
    let measurement = Measurement {
        criterion_id: criterion.id,
        criterion_name: criterion.name.clone(),
        value_number: input.value_number,
        value_bool: input.value_bool,
        value_text: input
            .value_text
            .as_ref()
            .map(|value| value.trim().to_string()),
        passed,
        deviation,
        note,
    };
    let cause = if passed {
        None
    } else {
        criterion.failure_cause.clone()
    };
    Ok(Some((measurement, cause)))
}

/// Allowed inspection status transitions. Anything not listed is
/// rejected with a 422 so an inspection can never jump backwards.
pub(crate) fn valid_inspection_transition(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        (INSP_REQUESTED, INSP_COMPLETED) | (INSP_REQUESTED, INSP_CANCELLED)
    )
}

/// Allowed alert status transitions.
pub(crate) fn valid_alert_transition(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        (ALERT_OPEN, ALERT_ACKNOWLEDGED)
            | (ALERT_OPEN, ALERT_RESOLVED)
            | (ALERT_ACKNOWLEDGED, ALERT_RESOLVED)
    )
}

/// Allowed CAPA state transitions. When verification is required,
/// `in_progress` may only move to `verified`; when it is not
/// required, `in_progress` may also close directly.
pub(crate) fn valid_capa_transition(from: &str, to: &str, verification_required: bool) -> bool {
    match (from, to) {
        (CAPA_OPEN, CAPA_IN_PROGRESS) => true,
        (CAPA_IN_PROGRESS, CAPA_VERIFIED) => true,
        (CAPA_VERIFIED, CAPA_CLOSED) => true,
        (CAPA_IN_PROGRESS, CAPA_CLOSED) => !verification_required,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::evaluate_measurement;
    use super::valid_alert_transition;
    use super::valid_capa_transition;
    use super::valid_inspection_transition;
    use super::validate_criterion_shape;
    use crate::modules::quality::{MeasurementInput, QualityCriterion, QualitySettings};
    use chrono::Utc;
    use uuid::Uuid;

    fn settings() -> QualitySettings {
        QualitySettings {
            criterion_kinds: vec![
                "numeric".to_string(),
                "boolean".to_string(),
                "text".to_string(),
            ],
            ..Default::default()
        }
    }

    fn criterion(kind: &str) -> QualityCriterion {
        QualityCriterion {
            id: Uuid::new_v4(),
            point_id: Uuid::new_v4(),
            name: "Thickness".to_string(),
            kind: kind.to_string(),
            unit: None,
            min_value: None,
            max_value: None,
            expected_bool: None,
            expected_text: None,
            failure_cause: Some("out_of_band".to_string()),
            weight: 1.0,
            required: true,
            tenant_id: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn numeric_criterion() -> QualityCriterion {
        let mut item = criterion("numeric");
        item.min_value = Some(0.0);
        item.max_value = Some(10.0);
        item
    }

    fn input(number: Option<f64>, boolean: Option<bool>, text: Option<&str>) -> MeasurementInput {
        MeasurementInput {
            criterion_id: Uuid::new_v4().to_string(),
            value_number: number,
            value_bool: boolean,
            value_text: text.map(str::to_string),
            note: None,
        }
    }

    #[test]
    fn inspection_transition_allows_requested_only() {
        assert!(valid_inspection_transition("requested", "completed"));
        assert!(valid_inspection_transition("requested", "cancelled"));
        assert!(!valid_inspection_transition("requested", "requested"));
        assert!(!valid_inspection_transition("completed", "cancelled"));
    }

    #[test]
    fn alert_transition_rules() {
        assert!(valid_alert_transition("open", "acknowledged"));
        assert!(valid_alert_transition("open", "resolved"));
        assert!(valid_alert_transition("acknowledged", "resolved"));
        assert!(!valid_alert_transition("acknowledged", "open"));
        assert!(!valid_alert_transition("resolved", "open"));
    }

    #[test]
    fn capa_transition_needs_verification_when_required() {
        assert!(valid_capa_transition("open", "in_progress", true));
        assert!(valid_capa_transition("in_progress", "verified", true));
        assert!(valid_capa_transition("verified", "closed", true));
        assert!(!valid_capa_transition("in_progress", "closed", true));
        assert!(valid_capa_transition("in_progress", "closed", false));
        assert!(!valid_capa_transition("closed", "open", false));
    }

    #[test]
    fn criterion_shape_accepts_valid_numeric() {
        assert!(
            validate_criterion_shape(
                "numeric",
                Some(0.0),
                Some(10.0),
                None,
                &None,
                1.0,
                &settings()
            )
            .is_ok()
        );
    }

    #[test]
    fn criterion_shape_rejects_numeric_without_bounds() {
        assert!(
            validate_criterion_shape("numeric", None, None, None, &None, 1.0, &settings()).is_err()
        );
    }

    #[test]
    fn criterion_shape_rejects_min_above_max() {
        assert!(
            validate_criterion_shape(
                "numeric",
                Some(10.0),
                Some(0.0),
                None,
                &None,
                1.0,
                &settings()
            )
            .is_err()
        );
    }

    #[test]
    fn criterion_shape_rejects_boolean_without_expectation() {
        assert!(
            validate_criterion_shape("boolean", None, None, None, &None, 1.0, &settings()).is_err()
        );
    }

    #[test]
    fn criterion_shape_rejects_unknown_kind() {
        assert!(
            validate_criterion_shape("colour", None, None, None, &None, 1.0, &settings()).is_err()
        );
    }

    #[test]
    fn numeric_in_band_passes() {
        let item = numeric_criterion();
        let (measurement, cause) = evaluate_measurement(&item, Some(&input(Some(5.0), None, None)))
            .unwrap()
            .unwrap();
        assert!(measurement.passed);
        assert_eq!(measurement.deviation, None);
        assert_eq!(cause, None);
    }

    #[test]
    fn numeric_above_band_fails_with_deviation() {
        let item = numeric_criterion();
        let (measurement, cause) =
            evaluate_measurement(&item, Some(&input(Some(15.0), None, None)))
                .unwrap()
                .unwrap();
        assert!(!measurement.passed);
        assert_eq!(measurement.deviation, Some(5.0));
        assert_eq!(cause, Some("out_of_band".to_string()));
    }

    #[test]
    fn missing_required_measurement_fails() {
        let item = numeric_criterion();
        let (measurement, cause) = evaluate_measurement(&item, None).unwrap().unwrap();
        assert!(!measurement.passed);
        assert_eq!(cause, Some("out_of_band".to_string()));
    }

    #[test]
    fn missing_optional_measurement_skips() {
        let mut item = numeric_criterion();
        item.required = false;
        assert!(evaluate_measurement(&item, None).unwrap().is_none());
    }

    #[test]
    fn boolean_matches_expectation() {
        let mut item = criterion("boolean");
        item.expected_bool = Some(true);
        let (yes, _) = evaluate_measurement(&item, Some(&input(None, Some(true), None)))
            .unwrap()
            .unwrap();
        assert!(yes.passed);
        let (no, _) = evaluate_measurement(&item, Some(&input(None, Some(false), None)))
            .unwrap()
            .unwrap();
        assert!(!no.passed);
    }

    #[test]
    fn text_matches_case_insensitively() {
        let mut item = criterion("text");
        item.expected_text = Some("Hello".to_string());
        let (measurement, _) = evaluate_measurement(&item, Some(&input(None, None, Some("hello"))))
            .unwrap()
            .unwrap();
        assert!(measurement.passed);
    }

    #[test]
    fn text_rejects_empty_value() {
        let item = criterion("text");
        assert!(evaluate_measurement(&item, Some(&input(None, None, Some("   ")))).is_err());
    }
}
