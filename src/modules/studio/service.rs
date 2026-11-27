// Studio - Field-type catalogue and definition validation.

use unified_core::error::{UnifiedError, UnifiedResult};
use chrono::Utc;
use uuid::Uuid;

use super::{CreateFieldDefinition, FIELD_TYPE_BOOLEAN, FIELD_TYPE_DATE, FIELD_TYPE_DECIMAL,
    FIELD_TYPE_NUMBER, FIELD_TYPE_SELECT, FIELD_TYPE_TEXT, FIELD_TYPE_USER_REFERENCE,
    FieldDefinition, FieldTypeInfo, MAX_HELP_LEN, MAX_LABEL_LEN, STATUS_ACTIVE, StudioState,
    normalize_company_id, optional_text, require_record_type, require_text, validate_field_key,
    validate_field_type, validate_options, validate_rules, validate_value};

pub(crate) fn field_type_infos() -> Vec<FieldTypeInfo> {
    vec![
        FieldTypeInfo {
            field_type: FIELD_TYPE_TEXT.to_string(),
            label: "Text".to_string(),
            supports_min_max: false,
            supports_length: true,
            supports_pattern: true,
            supports_options: false,
            supports_default: true,
        },
        FieldTypeInfo {
            field_type: FIELD_TYPE_NUMBER.to_string(),
            label: "Number".to_string(),
            supports_min_max: true,
            supports_length: false,
            supports_pattern: false,
            supports_options: false,
            supports_default: true,
        },
        FieldTypeInfo {
            field_type: FIELD_TYPE_DECIMAL.to_string(),
            label: "Decimal".to_string(),
            supports_min_max: true,
            supports_length: false,
            supports_pattern: false,
            supports_options: false,
            supports_default: true,
        },
        FieldTypeInfo {
            field_type: FIELD_TYPE_DATE.to_string(),
            label: "Date".to_string(),
            supports_min_max: false,
            supports_length: false,
            supports_pattern: false,
            supports_options: false,
            supports_default: true,
        },
        FieldTypeInfo {
            field_type: FIELD_TYPE_BOOLEAN.to_string(),
            label: "Boolean".to_string(),
            supports_min_max: false,
            supports_length: false,
            supports_pattern: false,
            supports_options: false,
            supports_default: true,
        },
        FieldTypeInfo {
            field_type: FIELD_TYPE_SELECT.to_string(),
            label: "Select".to_string(),
            supports_min_max: false,
            supports_length: false,
            supports_pattern: false,
            supports_options: true,
            supports_default: true,
        },
        FieldTypeInfo {
            field_type: FIELD_TYPE_USER_REFERENCE.to_string(),
            label: "User reference".to_string(),
            supports_min_max: false,
            supports_length: false,
            supports_pattern: false,
            supports_options: false,
            supports_default: false,
        },
    ]
}

// Handlers: field definitions.

pub(crate) fn validate_new_definition(
    state: &StudioState,
    input: &CreateFieldDefinition,
) -> UnifiedResult<FieldDefinition> {
    let record_type = require_record_type(state, &input.record_type)?;
    let company_id = normalize_company_id(&input.company_id);
    let field_key = validate_field_key(&input.field_key)?;
    let label = require_text(&input.label, "label", MAX_LABEL_LEN)?;
    let field_type = validate_field_type(&input.field_type)?;
    let rules = input.validation.clone().unwrap_or_default();
    validate_rules(&field_type, &rules)?;
    let options = validate_options(&field_type, &input.options.clone().unwrap_or_default())?;
    let help_text = optional_text(&input.help_text, "help_text", MAX_HELP_LEN)?;
    let required = input.required.unwrap_or(false);
    if let Some(default) = &input.default_value {
        let probe = FieldDefinition {
            id: String::new(),
            company_id: company_id.clone(),
            record_type: record_type.clone(),
            field_key: field_key.clone(),
            label: label.clone(),
            field_type: field_type.clone(),
            version: 1,
            status: STATUS_ACTIVE.to_string(),
            required,
            default_value: Some(default.clone()),
            help_text: help_text.clone(),
            options: options.clone(),
            validation: rules.clone(),
            created_by: String::new(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        validate_value(&probe, Some(default))?;
    }
    if state
        .definitions
        .iter()
        .any(|entry| {
            let definition = entry.value();
            definition.record_type == record_type
                && definition.company_id == company_id
                && definition.field_key == field_key
                && definition.status == STATUS_ACTIVE
        })
    {
        return Err(UnifiedError::conflict(format!(
            "Field \"{field_key}\" already exists for record type \"{record_type}\" in this company scope; update it to create a new version"
        )));
    }
    Ok(FieldDefinition {
        id: Uuid::new_v4().to_string(),
        company_id,
        record_type,
        field_key,
        label,
        field_type,
        version: 1,
        status: STATUS_ACTIVE.to_string(),
        required,
        default_value: input.default_value.clone(),
        help_text,
        options,
        validation: rules,
        created_by: String::new(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    })
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_type_infos_lists_all_seven_types() {
        let infos = field_type_infos();
        assert_eq!(infos.len(), 7);
        let keys: Vec<&str> = infos.iter().map(|info| info.field_type.as_str()).collect();
        assert!(keys.contains(&"text"));
        assert!(keys.contains(&"user_reference"));
    }

    #[test]
    fn field_type_infos_marks_which_types_need_options() {
        let infos = field_type_infos();
        let select = infos.iter().find(|info| info.field_type == "select").unwrap();
        assert!(select.supports_options);
        let text = infos.iter().find(|info| info.field_type == "text").unwrap();
        assert!(!text.supports_options);
    }

    #[test]
    fn field_type_infos_marks_which_types_support_length_and_pattern() {
        let infos = field_type_infos();
        let text = infos.iter().find(|info| info.field_type == "text").unwrap();
        assert!(text.supports_length);
        assert!(text.supports_pattern);
        assert!(!text.supports_min_max);
        let number = infos
            .iter()
            .find(|info| info.field_type == "number")
            .unwrap();
        assert!(number.supports_min_max);
        assert!(!number.supports_length);
        let user_reference = infos
            .iter()
            .find(|info| info.field_type == "user_reference")
            .unwrap();
        assert!(!user_reference.supports_default);
    }
}
