//! C12 provider registries (task 269).
//!
//! Provider definitions are DATA, not code. One registry table
//! (`provider_registry_definitions`, migration 000127) holds every
//! provider for every family — id, display name, base URLs, allowed
//! hosts, capabilities, currencies, credentials shape, models — and
//! this in-memory registry is the runtime view of that table.
//!
//! The built-in definitions ship as an embedded JSON seed document
//! (`crates/unified-core/provider_registry_seed.json`, the single
//! source of truth; the migration seed SQL is generated from the same
//! file). At runtime an admin adds a provider by registering a
//! definition — through the JSON config document
//! (`ProviderRegistry::load_json` / `load_json_file`), through the
//! future DB-row loader (`load_from_rows`), or directly with
//! `register` — and the factories resolve it by string id. No code
//! change, no new enum variant, no new `match` arm.
//!
//! The per-family enums (`PaymentProviderKind`, `EmailProviderKind`,
//! ...) still exist as stable identities for the providers that have
//! native adapter code, but every attribute table that used to be a
//! `match` on them (display name, allowed hosts, capabilities,
//! currencies, settings fields, base URLs, default models) now reads
//! from this registry. A provider with no native adapter runs through
//! the family's generic adapter (`adapter_kind` `"custom"` for
//! payments/cloud, `"smtp"`-style generic paths elsewhere); see
//! `docs/integration/INTEGRATION_NOTES_C12.md` for the per-family
//! status and the remaining work.
//!
//! Security: definitions never carry credential *values* — only the
//! *shape* of the credentials (field names, types, required/secret
//! flags). `register` validates ids (lowercase ASCII, digits, `-`,
//! `_`) so a definition id can never smuggle a path, URL, or SQL
//! fragment into a lookup. Built-in definitions cannot be removed,
//! only disabled.

use crate::error::{UnifiedError, UnifiedResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, OnceLock, RwLock};

/// Registry families. New families are just new strings — the
/// registry never closes over a family enum.
pub const FAMILY_PAYMENTS: &str = "payments";
pub const FAMILY_EMAIL: &str = "email";
pub const FAMILY_STORAGE: &str = "storage";
pub const FAMILY_VCS: &str = "vcs";
pub const FAMILY_AI: &str = "ai";
pub const FAMILY_CLOUD: &str = "cloud";

/// Families shipped in the seed document, in program order.
pub const KNOWN_FAMILIES: [&str; 6] = [
    FAMILY_PAYMENTS,
    FAMILY_EMAIL,
    FAMILY_STORAGE,
    FAMILY_VCS,
    FAMILY_AI,
    FAMILY_CLOUD,
];

/// The embedded seed document (single source of truth for built-ins).
pub fn seed_json() -> &'static str {
    include_str!("../provider_registry_seed.json")
}

fn default_true() -> bool {
    true
}

/// One field of a provider's settings page: name, type, whether it is
/// required, whether it is a secret (write-only, masked on read), and
/// a short description. The union of the old per-family field shapes
/// (payments `SettingField`, email `SettingsFieldDef`); missing keys
/// default to the safe value.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CredentialFieldDef {
    pub name: String,
    pub label: String,
    #[serde(rename = "type")]
    pub field_type: String,
    pub required: bool,
    pub secret: bool,
    pub applies_to: String,
    pub description: String,
    pub placeholder: String,
    pub help: String,
}

impl Default for CredentialFieldDef {
    fn default() -> Self {
        Self {
            name: String::new(),
            label: String::new(),
            field_type: "text".to_string(),
            required: false,
            secret: false,
            applies_to: "both".to_string(),
            description: String::new(),
            placeholder: String::new(),
            help: String::new(),
        }
    }
}

/// One provider definition: everything a factory, a catalog endpoint,
/// or an admin settings page needs to know about a provider, as data.
///
/// `capabilities` is a free-form JSON object whose keys differ per
/// family (payments: authorize/capture/charge/...; email:
/// transactional/bulk/templates/...; cloud: create/scale/delete/...;
/// storage: upload/download/delete/list/...; vcs: releases/tags/...;
/// ai: key_required/chat). Readers use `capability(name)` with a
/// default of `false`, so an unknown key is "not supported", never an
/// error.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderDefinition {
    pub family: String,
    pub id: String,
    pub display_name: String,
    pub base_url: Option<String>,
    pub sandbox_base_url: Option<String>,
    /// Protocol shape for families whose adapters differ by wire
    /// protocol (`"github"` / `"gitlab"` / `"bitbucket"` for VCS).
    pub api_style: Option<String>,
    /// Auth scheme (`"bearer"` / `"private-token"`, VCS).
    pub auth_scheme: Option<String>,
    /// Header carrying the webhook signature, when the provider has
    /// one (VCS).
    pub webhook_signature_header: Option<String>,
    pub allowed_hosts: Vec<String>,
    pub capabilities: Value,
    pub currencies: Vec<String>,
    pub credentials_shape: Vec<CredentialFieldDef>,
    /// Known models, in preference order (AI). The default model is
    /// `default_model`; the list is what the admin can pick from.
    pub models: Vec<String>,
    pub default_model: Option<String>,
    /// Which adapter serves this provider: a native adapter id
    /// (`"stripe"`, `"s3"`, `"github"`), or the family's generic
    /// adapter (`"custom"` for payments/cloud).
    pub adapter_kind: String,
    /// Whether connections need a user-supplied endpoint (payments).
    pub needs_endpoint: bool,
    /// Whether charges run through a delegate gateway connection
    /// (payment wallets).
    pub needs_delegate: bool,
    pub notes: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub builtin: bool,
}

impl Default for ProviderDefinition {
    fn default() -> Self {
        Self {
            family: String::new(),
            id: String::new(),
            display_name: String::new(),
            base_url: None,
            sandbox_base_url: None,
            api_style: None,
            auth_scheme: None,
            webhook_signature_header: None,
            allowed_hosts: Vec::new(),
            capabilities: Value::Object(Default::default()),
            currencies: Vec::new(),
            credentials_shape: Vec::new(),
            models: Vec::new(),
            default_model: None,
            adapter_kind: String::new(),
            needs_endpoint: false,
            needs_delegate: false,
            notes: None,
            enabled: true,
            builtin: false,
        }
    }
}

impl ProviderDefinition {
    /// Minimal definition for a new (admin-registered) provider.
    /// Capabilities, hosts, currencies, and credentials shape are
    /// filled in by the caller; everything else defaults safe
    /// (enabled, no hosts, no capabilities, no currencies).
    pub fn new(family: &str, id: &str, display_name: &str, adapter_kind: &str) -> Self {
        Self {
            family: family.to_string(),
            id: id.to_string(),
            display_name: display_name.to_string(),
            adapter_kind: adapter_kind.to_string(),
            ..Self::default()
        }
    }

    /// One capability flag; unknown keys read as `false` (unsupported).
    pub fn capability(&self, name: &str) -> bool {
        self.capabilities
            .get(name)
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    /// True when the API host is fixed (allow-listed) rather than
    /// user-supplied: exactly the providers with a non-empty host list.
    pub fn has_fixed_hosts(&self) -> bool {
        !self.allowed_hosts.is_empty()
    }

    /// True when `host` is on this provider's allow-list. An empty
    /// list means "no fixed hosts" and never matches.
    pub fn host_allowed(&self, host: &str) -> bool {
        self.allowed_hosts.iter().any(|allowed| allowed == host)
    }

    /// The credential field definition for `name`, if the shape has one.
    pub fn credential_field(&self, name: &str) -> Option<&CredentialFieldDef> {
        self.credentials_shape
            .iter()
            .find(|field| field.name == name)
    }

    /// Effective API base URL: the sandbox URL in sandbox mode when
    /// the provider has one, otherwise the live base URL. `None`
    /// means the provider has no fixed base URL (user-supplied
    /// endpoint, delegate wallet, or manual provider).
    pub fn effective_base_url(&self, sandbox: bool) -> Option<&str> {
        if sandbox {
            self.sandbox_base_url
                .as_deref()
                .or(self.base_url.as_deref())
        } else {
            self.base_url.as_deref()
        }
    }
}

/// A provider id is a lookup key, never free text: lowercase ASCII
/// letters, digits, `-`, and `_`, starting with a letter or digit,
/// at most 64 characters. Family names follow the same rule.
pub fn valid_provider_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.chars().enumerate().all(|(index, ch)| {
            ch.is_ascii_lowercase()
                || ch.is_ascii_digit()
                || (index > 0 && (ch == '-' || ch == '_'))
        })
}

fn validate_definition(def: &ProviderDefinition) -> UnifiedResult<()> {
    if !valid_provider_id(&def.family) {
        return Err(UnifiedError::bad_request(format!(
            "Invalid provider family \"{}\" (expected lowercase letters, digits, '-' or '_')",
            def.family
        )));
    }
    if !valid_provider_id(&def.id) {
        return Err(UnifiedError::bad_request(format!(
            "Invalid provider id \"{}\" (expected lowercase letters, digits, '-' or '_')",
            def.id
        )));
    }
    if def.display_name.trim().is_empty() {
        return Err(UnifiedError::bad_request(
            "Provider display_name is required",
        ));
    }
    if def.adapter_kind.trim().is_empty() {
        return Err(UnifiedError::bad_request(
            "Provider adapter_kind is required",
        ));
    }
    if !def.capabilities.is_object() && !def.capabilities.is_null() {
        return Err(UnifiedError::bad_request(
            "Provider capabilities must be a JSON object",
        ));
    }
    Ok(())
}

/// The runtime provider registry: definitions keyed by
/// `(family, id)`, interior-mutable so an admin (or a config load at
/// startup) can add providers without a code change.
pub struct ProviderRegistry {
    definitions: RwLock<HashMap<(String, String), Arc<ProviderDefinition>>>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self {
            definitions: RwLock::new(HashMap::new()),
        }
    }

    /// A registry seeded with the built-in definitions from the
    /// embedded seed document.
    pub fn with_builtins() -> Self {
        let registry = Self::new();
        for def in builtin_definitions() {
            // The seed document is checked in and covered by tests;
            // a parse/validation failure here is a programming error.
            registry
                .register(def)
                .expect("embedded provider registry seed is valid");
        }
        registry
    }

    /// Register (or replace) one definition. Replacing a built-in
    /// keeps the `builtin` flag, so a seed override can never turn a
    /// built-in into a removable custom entry.
    pub fn register(&self, mut def: ProviderDefinition) -> UnifiedResult<()> {
        validate_definition(&def)?;
        if def.capabilities.is_null() {
            def.capabilities = Value::Object(Default::default());
        }
        let key = (def.family.clone(), def.id.clone());
        let mut definitions = self
            .definitions
            .write()
            .expect("provider registry lock poisoned");
        if let Some(existing) = definitions.get(&key) {
            if existing.builtin {
                def.builtin = true;
            }
        }
        definitions.insert(key, Arc::new(def));
        Ok(())
    }

    /// Look a definition up by `(family, id)`. `None` means "no such
    /// provider" — callers must not fall back to a different provider.
    pub fn get(&self, family: &str, id: &str) -> Option<Arc<ProviderDefinition>> {
        self.definitions
            .read()
            .expect("provider registry lock poisoned")
            .get(&(family.to_string(), id.to_string()))
            .cloned()
    }

    /// All definitions for a family, sorted by id. Includes disabled
    /// entries; use `list_family_enabled` for the catalog.
    pub fn list_family(&self, family: &str) -> Vec<Arc<ProviderDefinition>> {
        let definitions = self
            .definitions
            .read()
            .expect("provider registry lock poisoned");
        let mut out: Vec<Arc<ProviderDefinition>> = definitions
            .values()
            .filter(|def| def.family == family)
            .cloned()
            .collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    /// Enabled definitions for a family, sorted by id: the catalog.
    pub fn list_family_enabled(&self, family: &str) -> Vec<Arc<ProviderDefinition>> {
        self.list_family(family)
            .into_iter()
            .filter(|def| def.enabled)
            .collect()
    }

    /// Families that have at least one definition, sorted.
    pub fn families(&self) -> Vec<String> {
        let definitions = self
            .definitions
            .read()
            .expect("provider registry lock poisoned");
        let mut out: Vec<String> = definitions.values().map(|def| def.family.clone()).collect();
        out.sort();
        out.dedup();
        out
    }

    /// Number of definitions for a family (enabled or not).
    pub fn count_family(&self, family: &str) -> usize {
        self.list_family(family).len()
    }

    /// Remove a definition. Built-ins cannot be removed — disable
    /// them with `set_enabled` instead — so an admin can never delete
    /// a provider other code depends on. Returns `false` when there
    /// was no such definition.
    pub fn remove(&self, family: &str, id: &str) -> UnifiedResult<bool> {
        let key = (family.to_string(), id.to_string());
        let mut definitions = self
            .definitions
            .write()
            .expect("provider registry lock poisoned");
        match definitions.get(&key) {
            Some(existing) if existing.builtin => Err(UnifiedError::bad_request(format!(
                "Built-in provider \"{family}/{id}\" cannot be removed; disable it instead"
            ))),
            Some(_) => Ok(definitions.remove(&key).is_some()),
            None => Ok(false),
        }
    }

    /// Enable or disable a definition. Returns `false` when there was
    /// no such definition.
    pub fn set_enabled(&self, family: &str, id: &str, enabled: bool) -> UnifiedResult<bool> {
        let key = (family.to_string(), id.to_string());
        let mut definitions = self
            .definitions
            .write()
            .expect("provider registry lock poisoned");
        let Some(existing) = definitions.get(&key) else {
            return Ok(false);
        };
        let mut updated = (**existing).clone();
        updated.enabled = enabled;
        definitions.insert(key, Arc::new(updated));
        Ok(true)
    }

    /// Register definitions from a JSON config document: either a bare
    /// array of definitions or an object with a `"providers"` array.
    /// This is the admin/config path for adding a provider without a
    /// code change or a database write. Returns the number registered.
    pub fn load_json(&self, json: &str) -> UnifiedResult<usize> {
        let value: Value = serde_json::from_str(json).map_err(|err| {
            UnifiedError::bad_request(format!("Invalid provider registry JSON: {err}"))
        })?;
        let entries: Vec<ProviderDefinition> = match &value {
            Value::Array(_) => serde_json::from_value(value).map_err(|err| {
                UnifiedError::bad_request(format!("Invalid provider registry JSON: {err}"))
            })?,
            Value::Object(_) => {
                #[derive(Deserialize)]
                struct Doc {
                    #[serde(default)]
                    providers: Vec<ProviderDefinition>,
                }
                let doc: Doc = serde_json::from_value(value).map_err(|err| {
                    UnifiedError::bad_request(format!("Invalid provider registry JSON: {err}"))
                })?;
                doc.providers
            }
            _ => {
                return Err(UnifiedError::bad_request(
                    "Provider registry JSON must be an array of definitions or an object with a \"providers\" array",
                ));
            }
        };
        self.load_from_rows(entries)
    }

    /// Register definitions from a JSON config file. The error names
    /// the path, never the file content.
    pub fn load_json_file(&self, path: &Path) -> UnifiedResult<usize> {
        let json = std::fs::read_to_string(path).map_err(|err| {
            UnifiedError::internal(format!(
                "Cannot read provider registry file \"{}\": {err}",
                path.display()
            ))
        })?;
        self.load_json(&json)
    }

    /// Register definitions from rows — the hook a startup loader uses
    /// to replace the embedded seeds with the rows of
    /// `provider_registry_definitions` (migration 000127). A row with
    /// the same `(family, id)` as a built-in overrides it (keeping the
    /// `builtin` flag). Returns the number registered.
    pub fn load_from_rows(&self, defs: Vec<ProviderDefinition>) -> UnifiedResult<usize> {
        let count = defs.len();
        for def in defs {
            self.register(def)?;
        }
        Ok(count)
    }
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Deserialize)]
struct SeedDoc {
    version: u32,
    #[serde(default)]
    providers: Vec<ProviderDefinition>,
}

/// The built-in definitions, parsed from the embedded seed document.
/// The seed is the single source of truth: the migration seed SQL is
/// generated from the same file.
pub fn builtin_definitions() -> Vec<ProviderDefinition> {
    let doc: SeedDoc =
        serde_json::from_str(seed_json()).expect("embedded provider registry seed is valid JSON");
    assert_eq!(doc.version, 1, "unsupported provider registry seed version");
    let mut out = doc.providers;
    for def in &mut out {
        def.builtin = true;
    }
    out
}

static REGISTRY: OnceLock<ProviderRegistry> = OnceLock::new();

/// The shared registry, seeded with the built-in definitions on first
/// use. Factories and catalog endpoints read from here; an admin adds
/// a provider by registering a definition on this instance.
pub fn provider_registry() -> &'static ProviderRegistry {
    REGISTRY.get_or_init(ProviderRegistry::with_builtins)
}

/// Leak a string once so registry DATA can back a pre-existing
/// `&'static str` API (display names, host lists) that predates the
/// registry. New code uses the owned `ProviderDefinition` directly and
/// never calls this.
pub fn leak_str(value: &str) -> &'static str {
    Box::leak(value.to_string().into_boxed_str())
}

/// Leak a string slice once; see `leak_str`.
pub fn leak_str_slice(values: &[String]) -> &'static [&'static str] {
    let leaked: Vec<&'static str> = values.iter().map(|value| leak_str(value)).collect();
    Box::leak(leaked.into_boxed_slice())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fresh() -> ProviderRegistry {
        ProviderRegistry::with_builtins()
    }

    fn custom(family: &str, id: &str) -> ProviderDefinition {
        let mut def = ProviderDefinition::new(family, id, "Acme Test", "custom");
        def.allowed_hosts = vec!["api.acme.test".to_string()];
        def.capabilities = json!({"charge": true, "refund": true, "create": true});
        def.currencies = vec!["USD".to_string(), "IDR".to_string()];
        def.credentials_shape = vec![CredentialFieldDef {
            name: "api_key".to_string(),
            label: "API key".to_string(),
            field_type: "password".to_string(),
            required: true,
            secret: true,
            ..CredentialFieldDef::default()
        }];
        def
    }

    #[test]
    fn builtin_catalog_counts_match_the_program_brief() {
        let registry = fresh();
        assert_eq!(registry.count_family(FAMILY_PAYMENTS), 12);
        assert_eq!(registry.count_family(FAMILY_EMAIL), 9);
        assert_eq!(registry.count_family(FAMILY_STORAGE), 5);
        assert_eq!(registry.count_family(FAMILY_VCS), 3);
        assert_eq!(registry.count_family(FAMILY_AI), 4);
        assert_eq!(registry.count_family(FAMILY_CLOUD), 17);
        assert_eq!(
            registry.families(),
            vec!["ai", "cloud", "email", "payments", "storage", "vcs"]
        );
    }

    #[test]
    fn builtin_ids_are_unique_and_valid() {
        let defs = builtin_definitions();
        assert_eq!(defs.len(), 50);
        let mut seen = std::collections::HashSet::new();
        for def in &defs {
            assert!(valid_provider_id(&def.id), "bad id {}", def.id);
            assert!(valid_provider_id(&def.family), "bad family {}", def.family);
            assert!(!def.display_name.trim().is_empty());
            assert!(!def.adapter_kind.trim().is_empty());
            assert!(seen.insert((def.family.clone(), def.id.clone())));
        }
    }

    #[test]
    fn builtin_baked_models_are_data_not_code() {
        let registry = fresh();
        let anthropic = registry.get(FAMILY_AI, "anthropic").unwrap();
        assert_eq!(
            anthropic.default_model.as_deref(),
            Some("claude-sonnet-4-20250514")
        );
        assert!(
            anthropic
                .models
                .contains(&"claude-sonnet-4-20250514".to_string())
        );
        let openai = registry.get(FAMILY_AI, "openai").unwrap();
        assert_eq!(openai.default_model.as_deref(), Some("gpt-4o-mini"));
        assert!(openai.models.contains(&"gpt-4o-mini".to_string()));
        // Compatible / local providers have no baked model at all.
        assert_eq!(
            registry.get(FAMILY_AI, "local").unwrap().default_model,
            None
        );
        assert!(
            registry
                .get(FAMILY_AI, "openai_compatible")
                .unwrap()
                .models
                .is_empty()
        );
    }

    #[test]
    fn add_one_payment_provider_without_code_change() {
        let registry = fresh();
        assert!(registry.get(FAMILY_PAYMENTS, "acme_pay").is_none());
        registry
            .register(custom(FAMILY_PAYMENTS, "acme_pay"))
            .unwrap();
        let def = registry.get(FAMILY_PAYMENTS, "acme_pay").unwrap();
        assert_eq!(def.display_name, "Acme Test");
        assert_eq!(def.adapter_kind, "custom");
        assert!(def.capability("charge"));
        assert!(def.capability("refund"));
        assert!(!def.capability("payouts"));
        assert!(def.host_allowed("api.acme.test"));
        assert!(!def.host_allowed("api.evil.test"));
        assert_eq!(def.currencies, vec!["USD", "IDR"]);
        assert_eq!(def.credential_field("api_key").unwrap().required, true);
        assert!(
            registry
                .list_family_enabled(FAMILY_PAYMENTS)
                .iter()
                .any(|entry| entry.id == "acme_pay")
        );
    }

    #[test]
    fn add_one_email_provider_without_code_change() {
        let registry = fresh();
        registry
            .register(custom(FAMILY_EMAIL, "acme_mail"))
            .unwrap();
        let def = registry.get(FAMILY_EMAIL, "acme_mail").unwrap();
        assert!(def.capability("charge"));
        assert!(def.host_allowed("api.acme.test"));
        assert_eq!(registry.count_family(FAMILY_EMAIL), 10);
    }

    #[test]
    fn add_one_storage_provider_without_code_change() {
        let registry = fresh();
        registry
            .register(custom(FAMILY_STORAGE, "acme_store"))
            .unwrap();
        let def = registry.get(FAMILY_STORAGE, "acme_store").unwrap();
        assert!(def.capability("create"));
        assert_eq!(def.adapter_kind, "custom");
        assert_eq!(registry.count_family(FAMILY_STORAGE), 6);
    }

    #[test]
    fn add_one_vcs_provider_without_code_change() {
        let registry = fresh();
        let mut def = custom(FAMILY_VCS, "gitea");
        def.base_url = Some("https://gitea.example.com/api/v1".to_string());
        def.api_style = Some("generic".to_string());
        def.auth_scheme = Some("bearer".to_string());
        def.webhook_signature_header = Some("x-webhook-secret".to_string());
        registry.register(def).unwrap();
        let found = registry.get(FAMILY_VCS, "gitea").unwrap();
        assert_eq!(
            found.base_url.as_deref(),
            Some("https://gitea.example.com/api/v1")
        );
        assert_eq!(found.api_style.as_deref(), Some("generic"));
        assert_eq!(found.auth_scheme.as_deref(), Some("bearer"));
        assert_eq!(
            found.webhook_signature_header.as_deref(),
            Some("x-webhook-secret")
        );
    }

    #[test]
    fn add_one_ai_provider_without_code_change() {
        let registry = fresh();
        let mut def = custom(FAMILY_AI, "acme_ai");
        def.base_url = Some("https://ai.acme.test/v1".to_string());
        def.models = vec!["acme-1".to_string()];
        def.default_model = Some("acme-1".to_string());
        registry.register(def).unwrap();
        let found = registry.get(FAMILY_AI, "acme_ai").unwrap();
        assert_eq!(found.default_model.as_deref(), Some("acme-1"));
        assert_eq!(found.models, vec!["acme-1"]);
        assert_eq!(
            found.effective_base_url(false).unwrap(),
            "https://ai.acme.test/v1"
        );
    }

    #[test]
    fn add_one_cloud_provider_without_code_change() {
        let registry = fresh();
        registry
            .register(custom(FAMILY_CLOUD, "acme_cloud"))
            .unwrap();
        let def = registry.get(FAMILY_CLOUD, "acme_cloud").unwrap();
        assert!(def.capability("create"));
        assert_eq!(registry.count_family(FAMILY_CLOUD), 18);
    }

    #[test]
    fn load_json_registers_a_provider_from_a_config_document() {
        let registry = ProviderRegistry::new();
        let count = registry
            .load_json(
                r#"[{"family": "payments", "id": "json_pay", "display_name": "JSON Pay",
                    "allowed_hosts": ["pay.example.com"], "capabilities": {"charge": true},
                    "currencies": ["USD"], "adapter_kind": "custom"}]"#,
            )
            .unwrap();
        assert_eq!(count, 1);
        let def = registry.get(FAMILY_PAYMENTS, "json_pay").unwrap();
        assert!(def.capability("charge"));
        assert!(def.host_allowed("pay.example.com"));
        assert!(!def.builtin);
        // The object form ({"providers": [...]}) works too.
        registry
            .load_json(
                r#"{"providers": [{"family": "email", "id": "json_mail", "display_name": "JSON Mail", "adapter_kind": "smtp"}]}"#,
            )
            .unwrap();
        assert!(registry.get(FAMILY_EMAIL, "json_mail").is_some());
    }

    #[test]
    fn builtin_definitions_cannot_be_removed_but_can_be_disabled() {
        let registry = fresh();
        assert!(
            registry
                .remove(FAMILY_PAYMENTS, "stripe")
                .unwrap_err()
                .to_string()
                .contains("cannot be removed")
        );
        assert!(registry.get(FAMILY_PAYMENTS, "stripe").is_some());
        assert!(
            registry
                .set_enabled(FAMILY_PAYMENTS, "stripe", false)
                .unwrap()
        );
        assert!(
            !registry
                .list_family_enabled(FAMILY_PAYMENTS)
                .iter()
                .any(|entry| entry.id == "stripe")
        );
        assert_eq!(registry.count_family(FAMILY_PAYMENTS), 12);
        // A custom entry can be removed again.
        registry
            .register(custom(FAMILY_PAYMENTS, "temp_pay"))
            .unwrap();
        assert!(registry.remove(FAMILY_PAYMENTS, "temp_pay").unwrap());
        assert!(registry.get(FAMILY_PAYMENTS, "temp_pay").is_none());
    }

    #[test]
    fn register_rejects_bad_ids() {
        let registry = fresh();
        let mut def = custom(FAMILY_PAYMENTS, "Acme Pay");
        assert!(registry.register(def.clone()).is_err());
        def.id = "../evil".to_string();
        assert!(registry.register(def.clone()).is_err());
        def.id = "ok_id-1".to_string();
        assert!(registry.register(def).is_ok());
    }

    #[test]
    fn load_from_rows_overrides_a_builtin_keeping_its_flag() {
        let registry = fresh();
        let mut row = registry
            .get(FAMILY_PAYMENTS, "stripe")
            .unwrap()
            .as_ref()
            .clone();
        row.display_name = "Stripe (DB row)".to_string();
        row.builtin = false; // a DB row must not be able to un-builtin itself
        registry.load_from_rows(vec![row]).unwrap();
        let found = registry.get(FAMILY_PAYMENTS, "stripe").unwrap();
        assert_eq!(found.display_name, "Stripe (DB row)");
        assert!(found.builtin);
        assert!(registry.remove(FAMILY_PAYMENTS, "stripe").is_err());
    }

    #[test]
    fn sandbox_base_url_wins_in_sandbox_mode() {
        let registry = fresh();
        let paypal = registry.get(FAMILY_PAYMENTS, "paypal").unwrap();
        assert_eq!(
            paypal.effective_base_url(true),
            Some("https://api-m.sandbox.paypal.com")
        );
        assert_eq!(
            paypal.effective_base_url(false),
            Some("https://api-m.paypal.com")
        );
        let stripe = registry.get(FAMILY_PAYMENTS, "stripe").unwrap();
        assert_eq!(
            stripe.effective_base_url(true),
            Some("https://api.stripe.com")
        );
    }
}
