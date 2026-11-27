// providers module -- Validation helpers for inputs handled by this module.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use serde_json::Value;
use url::Url;

use super::*;

// Connection validation.

/// Required credential / field set per kind. `endpoint` and `region`
/// requirements are checked here too, so a bad connection is rejected
/// at creation time instead of failing its first API call.
pub(crate) fn validate_connection(input: &CreateConnectionRequest) -> UnifiedResult<()> {
    let kind = ProviderKind::from_str(&input.kind).ok_or_else(|| {
        UnifiedError::bad_request(format!("Unknown provider kind \"{}\"", input.kind))
    })?;
    if input.name.trim().is_empty() {
        return Err(UnifiedError::bad_request("Connection name is required"));
    }
    if input.name.trim().len() > MAX_CONNECTION_NAME_LEN {
        return Err(UnifiedError::bad_request(format!(
            "Connection name must be at most {MAX_CONNECTION_NAME_LEN} characters"
        )));
    }
    if let Some(region) = input.region.as_deref() {
        if region.trim().len() > MAX_REGION_LEN {
            return Err(UnifiedError::bad_request(format!(
                "Connection region must be at most {MAX_REGION_LEN} characters"
            )));
        }
    }
    if let Some(project_id) = input.project_id.as_deref() {
        if project_id.trim().len() > MAX_PROJECT_ID_LEN {
            return Err(UnifiedError::bad_request(format!(
                "Connection project_id must be at most {MAX_PROJECT_ID_LEN} characters"
            )));
        }
    }
    if let Some(endpoint) = input.endpoint.as_deref() {
        if endpoint.trim().len() > MAX_ENDPOINT_LEN {
            return Err(UnifiedError::bad_request(format!(
                "Connection endpoint must be at most {MAX_ENDPOINT_LEN} characters"
            )));
        }
    }
    let creds = &input.credentials;
    let has = |value: &Option<String>| {
        value.as_deref().map(|v| !v.trim().is_empty()).unwrap_or(false)
    };
    let missing = |field: &str| {
        UnifiedError::bad_request(format!(
            "Provider \"{}\" requires credential field \"{field}\"",
            kind.as_str()
        ))
    };
    match kind {
        ProviderKind::Hetzner => {
            if !has(&creds.api_token) {
                return Err(missing("api_token"));
            }
        }
        ProviderKind::Ovh => {
            if !has(&creds.application_key) {
                return Err(missing("application_key"));
            }
            if !has(&creds.application_secret) {
                return Err(missing("application_secret"));
            }
            if !has(&creds.consumer_key) {
                return Err(missing("consumer_key"));
            }
            if input.project_id.as_deref().map(str::is_empty).unwrap_or(true) {
                return Err(UnifiedError::bad_request(
                    "Provider \"ovh\" requires project_id (the cloud project service name)",
                ));
            }
        }
        ProviderKind::Tencent => {
            if !has(&creds.access_key_id) {
                return Err(missing("access_key_id"));
            }
            if !has(&creds.secret_access_key) {
                return Err(missing("secret_access_key"));
            }
            if input.region.as_deref().map(str::is_empty).unwrap_or(true) {
                return Err(UnifiedError::bad_request(
                    "Provider \"tencent\" requires region",
                ));
            }
        }
        ProviderKind::Huawei => {
            let by_keys = has(&creds.access_key_id) && has(&creds.secret_access_key);
            let by_password = has(&creds.username) && has(&creds.password);
            if !by_keys && !by_password {
                return Err(UnifiedError::bad_request(
                    "Provider \"huawei\" requires access_key_id + secret_access_key, or username + password",
                ));
            }
            if input.region.as_deref().map(str::is_empty).unwrap_or(true) {
                return Err(UnifiedError::bad_request(
                    "Provider \"huawei\" requires region",
                ));
            }
            if let Some(region) = input.region.as_deref() {
                if !is_valid_huawei_region(region.trim()) {
                    return Err(UnifiedError::bad_request(
                        "Provider \"huawei\" region must match ^[a-z0-9-]+$ (for example \"cn-north-4\")",
                    ));
                }
            }
        }
        ProviderKind::Aws => {
            if !has(&creds.access_key_id) {
                return Err(missing("access_key_id"));
            }
            if !has(&creds.secret_access_key) {
                return Err(missing("secret_access_key"));
            }
            if input.region.as_deref().map(str::is_empty).unwrap_or(true) {
                return Err(UnifiedError::bad_request(
                    "Provider \"aws\" requires region",
                ));
            }
        }
        ProviderKind::Google => {
            if !has(&creds.access_token) {
                return Err(missing("access_token"));
            }
            if input.project_id.as_deref().map(str::is_empty).unwrap_or(true) {
                return Err(UnifiedError::bad_request(
                    "Provider \"google\" requires project_id (the Google Cloud project id)",
                ));
            }
            if input.region.as_deref().map(str::is_empty).unwrap_or(true) {
                return Err(UnifiedError::bad_request(
                    "Provider \"google\" requires region (the zone, for example \"us-central1-a\")",
                ));
            }
        }
        ProviderKind::Virtualizor => {
            if !has(&creds.api_token) {
                return Err(missing("api_token"));
            }
            if !has(&creds.password) {
                return Err(missing("password"));
            }
            validate_user_endpoint(input.endpoint.as_deref(), "virtualizor")?;
        }
        ProviderKind::Docker => {
            validate_user_endpoint(input.endpoint.as_deref(), "docker")?;
        }
        ProviderKind::Kubernetes => {
            if !has(&creds.bearer_token) {
                return Err(missing("bearer_token"));
            }
            validate_user_endpoint(input.endpoint.as_deref(), "kubernetes")?;
        }
        ProviderKind::Redshift => {
            if !has(&creds.access_key_id) {
                return Err(missing("access_key_id"));
            }
            if !has(&creds.secret_access_key) {
                return Err(missing("secret_access_key"));
            }
            if input.region.as_deref().map(str::is_empty).unwrap_or(true) {
                return Err(UnifiedError::bad_request(
                    "Provider \"redshift\" requires region",
                ));
            }
        }
        ProviderKind::DigitalOcean => {
            if !has(&creds.api_token) {
                return Err(missing("api_token"));
            }
        }
        ProviderKind::Linode => {
            if !has(&creds.api_token) {
                return Err(missing("api_token"));
            }
        }
        ProviderKind::OpenStack => {
            if !has(&creds.username) {
                return Err(missing("username"));
            }
            if !has(&creds.password) {
                return Err(missing("password"));
            }
            if input.project_id.as_deref().map(str::is_empty).unwrap_or(true) {
                return Err(UnifiedError::bad_request(
                    "Provider \"openstack\" requires project_id (the Keystone project name)",
                ));
            }
            validate_user_endpoint(input.endpoint.as_deref(), "openstack")?;
        }
        ProviderKind::Proxmox => {
            if !has(&creds.username) {
                return Err(missing("username"));
            }
            if !has(&creds.password) {
                return Err(missing("password"));
            }
            validate_user_endpoint(input.endpoint.as_deref(), "proxmox")?;
        }
        ProviderKind::Kvm => {
            if !has(&creds.username) {
                return Err(missing("username"));
            }
            if !has(&creds.password) {
                return Err(missing("password"));
            }
            validate_user_endpoint(input.endpoint.as_deref(), "kvm")?;
        }
        ProviderKind::Xen => {
            if !has(&creds.username) {
                return Err(missing("username"));
            }
            if !has(&creds.password) {
                return Err(missing("password"));
            }
            validate_user_endpoint(input.endpoint.as_deref(), "xen")?;
        }
        ProviderKind::Custom => {
            validate_user_endpoint(input.endpoint.as_deref(), "custom")?;
            let Some(config) = &input.custom_config else {
                return Err(UnifiedError::bad_request(
                    "Provider \"custom\" requires custom_config (auth type, endpoint templates, and field mapping)",
                ));
            };
            validate_custom_config(config)?;
        }
    }
    // A fixed-host provider with a custom endpoint must still point at
    // one of its allow-listed hosts.
    if let Some(endpoint) = input.endpoint.as_deref() {
        if kind.has_fixed_hosts() {
            validate_fixed_endpoint(endpoint, kind)?;
        }
    }
    Ok(())
}

/// Whether a Huawei region is safe to interpolate into an API host:
/// lowercase letters, digits, and hyphens only (`^[a-z0-9-]+$`).
/// Anything else (dots, `@`, `/`, uppercase) could change the host
/// the request is sent to, so it is rejected before any credential
/// is used.
pub(crate) fn is_valid_huawei_region(region: &str) -> bool {
    !region.is_empty()
        && region
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Validate a user-supplied panel / API endpoint (Virtualizor, Docker,
/// Kubernetes): http(s), a host, no embedded user info. The host itself
/// is admin-configured, so private / loopback hosts are allowed here --
/// the operator chose the endpoint.
pub(crate) fn validate_user_endpoint(endpoint: Option<&str>, kind: &str) -> UnifiedResult<Url> {
    let Some(raw) = endpoint else {
        return Err(UnifiedError::bad_request(format!(
            "Provider \"{kind}\" requires endpoint (the panel / API server URL)"
        )));
    };
    let url = Url::parse(raw.trim()).map_err(|_| {
        UnifiedError::bad_request(format!("Invalid endpoint URL \"{raw}\""))
    })?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(UnifiedError::bad_request(
            "Endpoint URL must use http or https",
        ));
    }
    if url.host_str().is_none_or(|host| host.is_empty()) {
        return Err(UnifiedError::bad_request(
            "Endpoint URL must include a host",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UnifiedError::bad_request(
            "Endpoint URL must not embed credentials; use the credential fields",
        ));
    }
    Ok(url)
}

/// Validate an endpoint override for a fixed-host provider against its
/// host allow-list.
pub(crate) fn validate_fixed_endpoint(endpoint: &str, kind: ProviderKind) -> UnifiedResult<()> {
    let url = Url::parse(endpoint.trim()).map_err(|_| {
        UnifiedError::bad_request(format!("Invalid endpoint URL \"{endpoint}\""))
    })?;
    let host = url.host_str().unwrap_or("");
    if !kind.allowed_hosts().contains(&host) {
        return Err(UnifiedError::bad_request(format!(
            "Endpoint host \"{host}\" is not an allowed {} API host",
            kind.as_str()
        )));
    }
    Ok(())
}

/// Validate one endpoint template: relative, starting with `/`, no
/// scheme, no parent-directory segments.
pub(crate) fn validate_relative_path(path: &str, field: &str) -> UnifiedResult<()> {
    if !path.starts_with('/') {
        return Err(UnifiedError::bad_request(format!(
            "Custom provider {field} must be a relative path starting with \"/\""
        )));
    }
    if path.contains("://") || path.split('/').any(|segment| segment == "..") {
        return Err(UnifiedError::bad_request(format!(
            "Custom provider {field} must not contain a URL scheme or \"..\" segments"
        )));
    }
    Ok(())
}

/// Validate a custom provider config at connection creation time, so
/// a bad config is rejected before its first API call.
pub(crate) fn validate_custom_config(config: &CustomProviderConfig) -> UnifiedResult<()> {
    match config.auth_type.as_str() {
        "bearer" | "basic" | "api_key" | "none" => {}
        other => {
            return Err(UnifiedError::bad_request(format!(
                "Custom provider auth_type \"{other}\" is not supported (bearer, basic, api_key, or none)"
            )));
        }
    }
    if config.list_path.trim().is_empty() {
        return Err(UnifiedError::bad_request(
            "Custom provider requires list_path",
        ));
    }
    validate_relative_path(&config.list_path, "list_path")?;
    if config.status_path.trim().is_empty() {
        return Err(UnifiedError::bad_request(
            "Custom provider requires status_path",
        ));
    }
    validate_relative_path(&config.status_path, "status_path")?;
    if !config.status_path.contains("{id}") {
        return Err(UnifiedError::bad_request(
            "Custom provider status_path must contain an \"{id}\" placeholder",
        ));
    }
    if let Some(path) = &config.create_path {
        validate_relative_path(path, "create_path")?;
    }
    if let Some(path) = &config.delete_path {
        validate_relative_path(path, "delete_path")?;
        if !path.contains("{id}") {
            return Err(UnifiedError::bad_request(
                "Custom provider delete_path must contain an \"{id}\" placeholder",
            ));
        }
    }
    if let Some(path) = &config.power_path {
        validate_relative_path(path, "power_path")?;
        if !path.contains("{id}") || !path.contains("{action}") {
            return Err(UnifiedError::bad_request(
                "Custom provider power_path must contain \"{id}\" and \"{action}\" placeholders",
            ));
        }
    }
    if config.supports_create && config.create_path.is_none() {
        return Err(UnifiedError::bad_request(
            "Custom provider supports_create requires create_path",
        ));
    }
    if config.supports_delete && config.delete_path.is_none() {
        return Err(UnifiedError::bad_request(
            "Custom provider supports_delete requires delete_path",
        ));
    }
    if config.supports_start_stop {
        if config.power_path.is_none() {
            return Err(UnifiedError::bad_request(
                "Custom provider supports_start_stop requires power_path",
            ));
        }
        let actions = config.power_actions.as_ref();
        let missing = actions.is_none()
            || actions
                .map(|actions| actions.start.trim().is_empty() || actions.stop.trim().is_empty())
                .unwrap_or(true);
        if missing {
            return Err(UnifiedError::bad_request(
                "Custom provider supports_start_stop requires power_actions with non-empty start and stop names",
            ));
        }
    }
    for (template, field) in [
        (&config.create_body_template, "create_body_template"),
        (&config.power_body_template, "power_body_template"),
    ] {
        if let Some(template) = template {
            serde_json::from_str::<Value>(template).map_err(|_| {
                UnifiedError::bad_request(format!(
                    "Custom provider {field} is not valid JSON"
                ))
            })?;
        }
    }
    if config.fields.id.trim().is_empty()
        || config.fields.name.trim().is_empty()
        || config.fields.status.trim().is_empty()
    {
        return Err(UnifiedError::bad_request(
            "Custom provider fields requires non-empty id, name, and status paths",
        ));
    }
    Ok(())
}

/// Walk a dot path through JSON objects and arrays (`a.b.0.c`).
/// Array segments are numeric indexes; object segments are keys.
pub(crate) fn lookup_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = value;
    for segment in path.split('.') {
        if segment.is_empty() {
            return None;
        }
        if current.is_array() {
            let index = segment.parse::<usize>().ok()?;
            current = current.as_array()?.get(index)?;
        } else {
            current = current.get(segment)?;
        }
    }
    Some(current)
}

pub(crate) fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

/// JSON-escape one template value's content (without surrounding
/// quotes), so a value containing quotes cannot break the rendered
/// JSON body template.
pub(crate) fn json_escape_content(value: &str) -> String {
    let quoted = serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string());
    quoted
        .strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .unwrap_or("")
        .to_string()
}

/// Render a JSON body template by replacing `{key}` placeholders.
pub(crate) fn render_body_template(template: &str, vars: &[(&str, &str)]) -> String {
    let mut rendered = template.to_string();
    for (key, value) in vars {
        rendered = rendered.replace(&format!("{{{key}}}"), &json_escape_content(value));
    }
    rendered
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_path_accepts_leading_slash() {
        assert!(validate_relative_path("/api/v1/servers", "list_path").is_ok());
    }

    #[test]
    fn relative_path_rejects_missing_leading_slash() {
        assert!(validate_relative_path("api/v1/servers", "list_path").is_err());
    }

    #[test]
    fn relative_path_rejects_dotdot_and_scheme() {
        assert!(validate_relative_path("/a/../b", "list_path").is_err());
        assert!(validate_relative_path("/a://b", "list_path").is_err());
    }

    #[test]
    fn json_escape_handles_quotes_and_newlines() {
        assert_eq!(json_escape_content("plain"), "plain");
        assert_eq!(json_escape_content("a\"b"), "a\\\"b");
        assert_eq!(json_escape_content("line\nbreak"), "line\\nbreak");
    }

    #[test]
    fn render_body_template_substitutes_and_escapes() {
        assert_eq!(
            render_body_template("Hello {{name}}!", &[("name", "Ada")]),
            "Hello Ada!"
        );
        assert_eq!(
            render_body_template("{{v}}", &[("v", "a\"b")]),
            "a\\\"b"
        );
        assert_eq!(render_body_template("{{missing}}", &[]), "{{missing}}");
    }

    #[test]
    fn value_to_string_covers_scalar_json() {
        assert_eq!(
            value_to_string(&serde_json::json!("text")),
            Some("text".to_string())
        );
        assert_eq!(
            value_to_string(&serde_json::json!(42)),
            Some("42".to_string())
        );
        assert_eq!(
            value_to_string(&serde_json::json!(true)),
            Some("true".to_string())
        );
        assert_eq!(value_to_string(&serde_json::json!(null)), None);
        assert_eq!(value_to_string(&serde_json::json!([1])), None);
    }

    #[test]
    fn lookup_path_walks_objects_and_arrays() {
        let value = serde_json::json!({"a": {"b": [10, 20]}});
        assert_eq!(lookup_path(&value, "a.b.1"), Some(&serde_json::json!(20)));
        assert_eq!(lookup_path(&value, "a"), Some(&serde_json::json!({"b": [10, 20]})));
    }

    #[test]
    fn lookup_path_rejects_missing_and_empty_segments() {
        let value = serde_json::json!({"a": {"b": [10, 20]}});
        assert_eq!(lookup_path(&value, "a.b.5"), None);
        assert_eq!(lookup_path(&value, "a.x"), None);
        assert_eq!(lookup_path(&value, "a..b"), None);
    }

    fn huawei_request(region: Option<&str>) -> CreateConnectionRequest {
        CreateConnectionRequest {
            name: "huawei".to_string(),
            kind: "huawei".to_string(),
            endpoint: None,
            region: region.map(str::to_string),
            project_id: None,
            credentials: ProviderCredentials {
                username: Some("user".to_string()),
                password: Some("secret".to_string()),
                ..Default::default()
            },
            custom_config: None,
            enabled: true,
        }
    }

    #[test]
    fn huawei_region_accepts_lowercase_digits_and_hyphens() {
        assert!(is_valid_huawei_region("cn-north-4"));
        assert!(is_valid_huawei_region("ap-southeast-1"));
        assert!(validate_connection(&huawei_request(Some("cn-north-4"))).is_ok());
    }

    #[test]
    fn huawei_region_rejects_host_injection() {
        // Dots plus `@` would smuggle a user-info part into the URL
        // and redirect the request (and the IAM password) elsewhere.
        assert!(!is_valid_huawei_region("x.myhuaweicloud.com@evil.example"));
        assert!(validate_connection(&huawei_request(Some("x.myhuaweicloud.com@evil.example"))).is_err());
    }

    #[test]
    fn huawei_region_rejects_uppercase_spaces_and_slashes() {
        for region in ["CN-North-4", "cn north-4", "cn-north-4/", "cn-north-4?x=1", ""] {
            assert!(!is_valid_huawei_region(region), "region {region:?} must be rejected");
            assert!(
                validate_connection(&huawei_request(Some(region))).is_err(),
                "region {region:?} must be rejected by validate_connection"
            );
        }
    }

    #[test]
    fn connection_name_length_is_capped() {
        let mut request = huawei_request(Some("cn-north-4"));
        request.name = "x".repeat(MAX_CONNECTION_NAME_LEN);
        assert!(validate_connection(&request).is_ok());
        request.name = "x".repeat(MAX_CONNECTION_NAME_LEN + 1);
        assert!(validate_connection(&request).is_err());
    }

    #[test]
    fn connection_region_and_project_id_lengths_are_capped() {
        let mut request = huawei_request(Some(&"a".repeat(MAX_REGION_LEN + 1)));
        assert!(validate_connection(&request).is_err());
        request = huawei_request(Some("cn-north-4"));
        request.project_id = Some("p".repeat(MAX_PROJECT_ID_LEN + 1));
        assert!(validate_connection(&request).is_err());
    }

    #[test]
    fn connection_endpoint_length_is_capped() {
        let mut request = huawei_request(Some("cn-north-4"));
        request.endpoint = Some(format!("https://example.invalid/{}", "a".repeat(MAX_ENDPOINT_LEN)));
        assert!(validate_connection(&request).is_err());
    }
}
