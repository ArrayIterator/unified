// cloud module — hook contracts for "cloud panel" external modules.
//
// The "cloud panel" grouping is the category for external
// panel/provider connector modules (SolusVM, Virtualizor, cPanel,
// DirectAdmin, Plesk, CyberPanel, WHMCS, and the SSL providers).
// Each is self-contained ("self dependensi": its own dependencies,
// its own module/crate) and plugs into the cloud module through
// these hooks. The contracts follow the `widgets.widget_types`
// collect pattern: emit with `{}`, each answer is one definition
// object or an array of them; malformed answers are skipped;
// built-ins win on id collision; the catalogue is deterministic
// (built-ins first, then hook answers in emission order).
//
// A contributed provider definition is descriptive only: it still
// needs a registered adapter factory
// (`ProviderRegistry::register_factory`) to work — the factory
// registry is keyed by the `ProviderKind` enum today, so a future
// external kind needs a string-keyed factory or an enum addition.

use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::provider_registry::{CredentialFieldDef, ProviderDefinition};

use super::providers::catalog;

/// Hook event: a module contributes cloud provider definitions.
/// Answer shape: `{family: "cloud", id (kebab), display_name,
/// capabilities: {create, scale, delete, containers, start_stop,
/// suspend, renew, change_password, usage}, allowed_hosts?,
/// credentials_shape?}`.
pub const EVENT_CLOUD_PROVIDER_DEFINITIONS: &str = "cloud.provider.definitions";

/// Hook event: a module contributes cloud panel connector
/// definitions. Answer shape: `{id (kebab), display_name,
/// kinds?: [...]}` — the provider kinds this panel manages, if any.
pub const EVENT_CLOUD_PANEL_DEFINITIONS: &str = "cloud.panel.definitions";

/// Hook event: a module contributes SSL provider definitions.
/// Answer shape: `{id (kebab, e.g. "letsencrypt"), display_name,
/// acme_directory_url?}`.
pub const EVENT_CLOUD_SSL_PROVIDER_DEFINITIONS: &str = "cloud.ssl_provider.definitions";

/// One contributed cloud panel connector definition.
#[derive(Debug, Clone, Serialize)]
pub struct CloudPanelDefinition {
    pub id: String,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<String>,
}

/// One contributed SSL provider definition.
#[derive(Debug, Clone, Serialize)]
pub struct CloudSslProviderDefinition {
    pub id: String,
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acme_directory_url: Option<String>,
}

/// True when `id` is a valid definition id: kebab-case, 1-64 chars,
/// starts with a lowercase letter or digit. Same rule as the widget
/// registry.
fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
        && id
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
}

/// Parse one hook answer into a provider definition, or `None` when
/// it is malformed. The family is forced to `"cloud"` and the adapter
/// kind defaults to the id: a contributed definition still needs a
/// registered adapter factory to do anything.
fn provider_definition_from_value(value: &Value) -> Option<ProviderDefinition> {
    let id = value.get("id")?.as_str()?;
    if !is_valid_id(id) {
        return None;
    }
    let display_name = value.get("display_name")?.as_str()?;
    if display_name.is_empty() || display_name.len() > 120 {
        return None;
    }
    let mut definition = ProviderDefinition::new("cloud", id, display_name, id);
    if let Some(capabilities) = value.get("capabilities") {
        if !capabilities.is_object() {
            return None;
        }
        definition.capabilities = capabilities.clone();
    }
    if let Some(hosts) = value.get("allowed_hosts") {
        let hosts = hosts.as_array()?;
        let mut parsed = Vec::new();
        for host in hosts {
            let host = host.as_str()?;
            if host.is_empty() {
                return None;
            }
            parsed.push(host.to_string());
        }
        definition.allowed_hosts = parsed;
    }
    if let Some(shape) = value.get("credentials_shape") {
        let fields: Vec<CredentialFieldDef> = serde_json::from_value(shape.clone()).ok()?;
        if fields.iter().any(|field| field.name.is_empty()) {
            return None;
        }
        definition.credentials_shape = fields;
    }
    Some(definition)
}

fn panel_definition_from_value(value: &Value) -> Option<CloudPanelDefinition> {
    let id = value.get("id")?.as_str()?;
    if !is_valid_id(id) {
        return None;
    }
    let display_name = value.get("display_name")?.as_str()?;
    if display_name.is_empty() || display_name.len() > 120 {
        return None;
    }
    let mut kinds = Vec::new();
    if let Some(kinds_value) = value.get("kinds") {
        let kinds_array = kinds_value.as_array()?;
        for kind in kinds_array {
            let kind = kind.as_str()?;
            if !is_valid_id(kind) {
                return None;
            }
            kinds.push(kind.to_string());
        }
    }
    Some(CloudPanelDefinition {
        id: id.to_string(),
        display_name: display_name.to_string(),
        kinds,
    })
}

fn ssl_provider_definition_from_value(value: &Value) -> Option<CloudSslProviderDefinition> {
    let id = value.get("id")?.as_str()?;
    if !is_valid_id(id) {
        return None;
    }
    let display_name = value.get("display_name")?.as_str()?;
    if display_name.is_empty() || display_name.len() > 120 {
        return None;
    }
    let acme_directory_url = value
        .get("acme_directory_url")
        .and_then(|item| item.as_str())
        .filter(|url| !url.is_empty())
        .map(str::to_string);
    Some(CloudSslProviderDefinition {
        id: id.to_string(),
        display_name: display_name.to_string(),
        acme_directory_url,
    })
}

/// Collect the cloud provider catalogue: the registry built-ins plus
/// any provider a module contributed through
/// `cloud.provider.definitions`. Built-ins win on id collision.
pub fn collect_cloud_provider_definitions(ctx: &AppContext) -> Vec<Arc<ProviderDefinition>> {
    let mut definitions = catalog();
    let answers = ctx
        .hooks
        .emit(EVENT_CLOUD_PROVIDER_DEFINITIONS, serde_json::json!({}));
    for result in &answers.results {
        let candidates: Vec<&Value> = match result {
            Value::Array(items) => items.iter().collect(),
            single => vec![single],
        };
        for candidate in candidates {
            if let Some(definition) = provider_definition_from_value(candidate) {
                if !definitions.iter().any(|item| item.id == definition.id) {
                    definitions.push(Arc::new(definition));
                }
            }
        }
    }
    definitions
}

/// Collect the cloud panel connector definitions contributed through
/// `cloud.panel.definitions`. The in-tree `panels` module answers
/// with its registry contents; external "cloud panel" modules answer
/// the same way when they are adopted.
pub fn collect_cloud_panel_definitions(ctx: &AppContext) -> Vec<CloudPanelDefinition> {
    let mut definitions = Vec::new();
    let answers = ctx
        .hooks
        .emit(EVENT_CLOUD_PANEL_DEFINITIONS, serde_json::json!({}));
    for result in &answers.results {
        let candidates: Vec<&Value> = match result {
            Value::Array(items) => items.iter().collect(),
            single => vec![single],
        };
        for candidate in candidates {
            if let Some(definition) = panel_definition_from_value(candidate) {
                if !definitions.iter().any(|item| item.id == definition.id) {
                    definitions.push(definition);
                }
            }
        }
    }
    definitions
}

/// Collect the SSL provider definitions contributed through
/// `cloud.ssl_provider.definitions`. The in-tree `certificates`
/// module answers with its ACME modes; external SSL-provider modules
/// answer the same way when they are adopted.
pub fn collect_cloud_ssl_provider_definitions(ctx: &AppContext) -> Vec<CloudSslProviderDefinition> {
    let mut definitions = Vec::new();
    let answers = ctx
        .hooks
        .emit(EVENT_CLOUD_SSL_PROVIDER_DEFINITIONS, serde_json::json!({}));
    for result in &answers.results {
        let candidates: Vec<&Value> = match result {
            Value::Array(items) => items.iter().collect(),
            single => vec![single],
        };
        for candidate in candidates {
            if let Some(definition) = ssl_provider_definition_from_value(candidate) {
                if !definitions.iter().any(|item| item.id == definition.id) {
                    definitions.push(definition);
                }
            }
        }
    }
    definitions
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn provider_definition_rejects_bad_ids() {
        assert!(provider_definition_from_value(&json!({"id": "Bad_ID", "display_name": "X"})).is_none());
        assert!(provider_definition_from_value(&json!({"id": "", "display_name": "X"})).is_none());
        assert!(provider_definition_from_value(&json!({"id": "ok", "display_name": ""})).is_none());
    }

    #[test]
    fn provider_definition_accepts_full_shape() {
        let definition = provider_definition_from_value(&json!({
            "id": "acme-cloud",
            "display_name": "Acme Cloud",
            "capabilities": {"create": true, "suspend": true},
            "allowed_hosts": ["api.acme.example"],
            "credentials_shape": [{"name": "api_key", "label": "API key", "type": "password", "required": true, "secret": true}],
        }))
        .expect("well-formed answer parses");
        assert_eq!(definition.family, "cloud");
        assert_eq!(definition.adapter_kind, "acme-cloud");
        assert!(definition.capability("create"));
        assert!(definition.capability("suspend"));
        assert!(!definition.capability("delete"));
        assert_eq!(definition.allowed_hosts, vec!["api.acme.example"]);
        assert_eq!(definition.credentials_shape.len(), 1);
    }

    #[test]
    fn panel_and_ssl_definitions_parse() {
        let panel = panel_definition_from_value(&json!({
            "id": "cpanel", "display_name": "cPanel", "kinds": ["cpanel"],
        }))
        .expect("well-formed panel parses");
        assert_eq!(panel.kinds, vec!["cpanel"]);
        assert!(panel_definition_from_value(&json!({"id": "x", "display_name": "X", "kinds": "nope"})).is_none());
        let ssl = ssl_provider_definition_from_value(&json!({
            "id": "letsencrypt", "display_name": "Let's Encrypt",
            "acme_directory_url": "https://acme-v02.api.letsencrypt.org/directory",
        }))
        .expect("well-formed SSL provider parses");
        assert!(ssl.acme_directory_url.is_some());
    }
}
