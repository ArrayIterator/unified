// Template variable registry: named, resolvable variables for Handlebars
// templates.
//
// Modules and themes register variables by name together with a resolver
// closure. At render time the engine resolves every registered variable
// against a `VariableContext` (site info, current user, tenant, public
// settings, plus an extensible bag of module data) and merges the results
// into the template context, so templates read dynamic data with plain
// `{{variable}}` / `{{variable.field}}` expressions instead of relying on
// each call site to build the same JSON by hand.
//
// Design decisions (see `docs/THEME_VARIABLES.md` for the full catalog):
//
//   * Duplicate names are rejected by `register` (HTTP 409). An explicit
//     `replace` call is required to overwrite an existing variable, so a
//     module can never silently shadow another module's variable.
//   * A failing resolver never breaks a render: `resolve_all` logs the
//     failure, omits that variable, and keeps resolving the rest.
//     Callers that need the failure list use `resolve_all_detailed`.
//   * Per-call template data wins over registered variables on a name
//     collision (see `ThemeEngine::render_with_context`).

use unified_core::error::{UnifiedError, UnifiedResult};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Public information about the site a template is rendered for.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SiteInfo {
    pub name: String,
    pub url: String,
    pub description: String,
}

impl Default for SiteInfo {
    fn default() -> Self {
        Self {
            name: "Unified".to_string(),
            url: String::new(),
            description: String::new(),
        }
    }
}

impl SiteInfo {
    pub fn new(name: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            url: url.into(),
            description: String::new(),
        }
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }
}

/// Public information about the user a template is rendered for.
/// `None` on `VariableContext::user` means the render is anonymous.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserInfo {
    pub id: String,
    pub name: String,
    pub email: String,
    pub role: String,
}

impl UserInfo {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        email: impl Into<String>,
        role: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            email: email.into(),
            role: role.into(),
        }
    }
}

/// Public information about the tenant a template is rendered for.
/// `None` on `VariableContext::tenant` means no tenant context applies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantInfo {
    pub id: String,
    pub name: String,
}

impl TenantInfo {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
        }
    }
}

/// Which page a template is rendered for: page identity and routing.
///
/// `kind` is a free-form page type (`"home"`, `"admin"`, `"dashboard"`,
/// `"custom"`, or any other string a module uses), deliberately not a
/// closed enum so themes and modules can add page types without
/// changing this struct. The logic variables (`is_home`, `is_admin`,
/// `is_dashboard`, `page`, `route`) are derived from this struct.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageInfo {
    /// Page type, for example `"home"`, `"admin"`, `"dashboard"`,
    /// or `"custom"`. Defaults to `"custom"`.
    pub kind: String,
    /// Name of the matched route, when the call site knows it.
    pub route_name: Option<String>,
    /// Request path, for example `"/admin/users"`.
    pub path: String,
    /// Request URI (path plus query string), for example
    /// `"/admin/users?page=2"`. Not an absolute URL.
    pub url: String,
}

impl Default for PageInfo {
    fn default() -> Self {
        Self {
            kind: "custom".to_string(),
            route_name: None,
            path: String::new(),
            url: String::new(),
        }
    }
}

impl PageInfo {
    pub fn new(
        kind: impl Into<String>,
        path: impl Into<String>,
        url: impl Into<String>,
    ) -> Self {
        Self {
            kind: kind.into(),
            route_name: None,
            path: path.into(),
            url: url.into(),
        }
    }

    pub fn with_kind(mut self, kind: impl Into<String>) -> Self {
        self.kind = kind.into();
        self
    }

    pub fn with_route_name(mut self, route_name: impl Into<String>) -> Self {
        self.route_name = Some(route_name.into());
        self
    }

    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = path.into();
        self
    }

    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.url = url.into();
        self
    }
}

/// Render-time data handed to every variable resolver.
///
/// Only public, template-safe data belongs here. Never put secrets,
/// credential material, password hashes, or API keys into `settings` or
/// `extra`: everything in this context can end up rendered into HTML.
#[derive(Debug, Clone, Default)]
pub struct VariableContext {
    /// Site identity; defaults to `SiteInfo::default()` ("Unified").
    pub site: SiteInfo,
    /// Current user, or `None` for an anonymous render.
    pub user: Option<UserInfo>,
    /// Current tenant, or `None` when no tenant context applies.
    pub tenant: Option<TenantInfo>,
    /// Curated public site settings (theme, locale, feature flags).
    /// The caller decides what is public; secrets must never be added.
    pub settings: Map<String, Value>,
    /// Extensible bag of module-provided data, keyed by name, for
    /// custom resolvers that need more than site/user/tenant/settings.
    pub extra: Map<String, Value>,
    /// Which page is being rendered: page type, route name, path, and
    /// URL. Defaults to kind `"custom"` with an empty path and URL.
    /// Drives the logic variables (`is_home`, `is_admin`,
    /// `is_dashboard`, `page`, `route`).
    pub page: PageInfo,
}

impl VariableContext {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_site(mut self, site: SiteInfo) -> Self {
        self.site = site;
        self
    }

    pub fn with_user(mut self, user: UserInfo) -> Self {
        self.user = Some(user);
        self
    }

    pub fn with_tenant(mut self, tenant: TenantInfo) -> Self {
        self.tenant = Some(tenant);
        self
    }

    /// Add one public setting. Values are stored as-is; the caller is
    /// responsible for keeping secrets out of this map.
    pub fn with_setting(mut self, key: impl Into<String>, value: Value) -> Self {
        self.settings.insert(key.into(), value);
        self
    }

    pub fn with_settings(mut self, settings: Map<String, Value>) -> Self {
        self.settings.extend(settings);
        self
    }

    /// Add one entry to the extensible module-data bag.
    pub fn with_extra(mut self, key: impl Into<String>, value: Value) -> Self {
        self.extra.insert(key.into(), value);
        self
    }

    /// Read one entry from the extensible module-data bag.
    pub fn extra_value(&self, key: &str) -> Option<&Value> {
        self.extra.get(key)
    }

    /// Set the page being rendered, wholesale.
    pub fn with_page(mut self, page: PageInfo) -> Self {
        self.page = page;
        self
    }

    /// Set only the page type (`"home"`, `"admin"`, `"dashboard"`,
    /// `"custom"`, or any module-defined type), keeping any route
    /// name, path, and URL already set.
    pub fn with_page_kind(mut self, kind: impl Into<String>) -> Self {
        self.page.kind = kind.into();
        self
    }

    /// Set the page's route name, path, and URL in one call. The page
    /// kind is left untouched; use `with_page_kind` to set it.
    pub fn with_route(
        mut self,
        route_name: impl Into<String>,
        path: impl Into<String>,
        url: impl Into<String>,
    ) -> Self {
        self.page.route_name = Some(route_name.into());
        self.page.path = path.into();
        self.page.url = url.into();
        self
    }
}

/// A variable resolver: given the render-time context, produce the JSON
/// value the template sees under the variable's name. Returning an error
/// omits the variable from the render (see `VariableRegistry::resolve_all`)
/// instead of failing the whole render.
pub type VariableResolver = Arc<dyn Fn(&VariableContext) -> UnifiedResult<Value> + Send + Sync>;

/// Catalog entry describing one registered variable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariableDescriptor {
    pub name: String,
    pub description: String,
    /// True for the built-in variables (`site`, `user`, `tenant`,
    /// `settings`, the logic variables, `page`, and `route`); false
    /// for module-registered variables.
    pub builtin: bool,
}

/// One resolver failure recorded by `resolve_all_detailed`: the variable
/// was omitted from the rendered context because its resolver errored.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariableFailure {
    pub name: String,
    pub message: String,
}

struct VariableEntry {
    description: String,
    builtin: bool,
    resolver: VariableResolver,
}

/// Thread-safe registry of named template variables.
///
/// A variable name must start with an ASCII letter or `_` and contain
/// only ASCII letters, digits, and `_`, so it is always a valid
/// Handlebars identifier (`{{my_variable}}`).
#[derive(Default)]
pub struct VariableRegistry {
    entries: RwLock<HashMap<String, VariableEntry>>,
}

impl VariableRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// A registry with the built-in variables already registered
    /// (`site`, `user`, `tenant`, `settings`, the logic variables
    /// `is_home` / `is_admin` / `is_dashboard` / `is_admin_user` /
    /// `is_logged_in` / `has_user` / `has_tenant` / `has_settings` /
    /// `has_extra`, and the `page` / `route` objects). Note that
    /// `has_data` is not a registry variable: the engine injects it
    /// at render time from the per-call data.
    pub fn with_builtins() -> Self {
        let registry = Self::new();
        // Registration cannot fail on a fresh registry: the built-in
        // names are valid and unique by construction.
        let _ = registry.register_builtins();
        registry
    }

    /// Register the built-in variables. Safe to call on a registry that
    /// already has them: existing built-ins are left untouched and the
    /// call still succeeds.
    pub fn register_builtins(&self) -> UnifiedResult<()> {
        self.register_builtin(
            "site",
            "Public site identity: name, url, and description.",
            Arc::new(|ctx: &VariableContext| {
                Ok(json!({
                    "name": ctx.site.name,
                    "url": ctx.site.url,
                    "description": ctx.site.description,
                }))
            }),
        )?;
        self.register_builtin(
            "user",
            "Current user: id, name, email, role, and whether the render is authenticated. All identity fields are null when anonymous.",
            Arc::new(|ctx: &VariableContext| {
                Ok(match &ctx.user {
                    Some(user) => json!({
                        "id": user.id,
                        "name": user.name,
                        "email": user.email,
                        "role": user.role,
                        "authenticated": true,
                    }),
                    None => json!({
                        "id": Value::Null,
                        "name": Value::Null,
                        "email": Value::Null,
                        "role": Value::Null,
                        "authenticated": false,
                    }),
                })
            }),
        )?;
        self.register_builtin(
            "tenant",
            "Current tenant: id and name. Both are null when no tenant context applies.",
            Arc::new(|ctx: &VariableContext| {
                Ok(match &ctx.tenant {
                    Some(tenant) => json!({
                        "id": tenant.id,
                        "name": tenant.name,
                    }),
                    None => json!({
                        "id": Value::Null,
                        "name": Value::Null,
                    }),
                })
            }),
        )?;
        self.register_builtin(
            "settings",
            "Curated public site settings (theme, locale, feature flags). Never contains secrets.",
            Arc::new(|ctx: &VariableContext| Ok(Value::Object(ctx.settings.clone()))),
        )?;
        // Logic variables: booleans and objects for template
        // branching (`{{#if is_admin}}...{{/if}}`). Page identity
        // (`is_home` / `is_admin` / `is_dashboard`) is decided purely
        // by the page kind on `VariableContext::page`; it says nothing
        // about the current user's role. The role-based variant is
        // `is_admin_user`. `has_data` is deliberately not registered
        // here: its value depends on the per-call template data, which
        // resolvers cannot see, so the engine injects it after the
        // merge instead.
        self.register_builtin(
            "is_home",
            "True when the rendered page is the home page (page kind \"home\").",
            Arc::new(|ctx: &VariableContext| Ok(json!(ctx.page.kind == "home"))),
        )?;
        self.register_builtin(
            "is_admin",
            "True when the rendered page is an admin page (page kind \"admin\"). Page identity only: it does not check the current user's role; use is_admin_user for that.",
            Arc::new(|ctx: &VariableContext| Ok(json!(ctx.page.kind == "admin"))),
        )?;
        self.register_builtin(
            "is_dashboard",
            "True when the rendered page is a dashboard page (page kind \"dashboard\").",
            Arc::new(|ctx: &VariableContext| Ok(json!(ctx.page.kind == "dashboard"))),
        )?;
        self.register_builtin(
            "is_admin_user",
            "True when the current user's role is \"admin\". Unlike is_admin, this is about the user, not the page.",
            Arc::new(|ctx: &VariableContext| {
                Ok(json!(
                    ctx.user
                        .as_ref()
                        .is_some_and(|user| user.role == "admin")
                ))
            }),
        )?;
        self.register_builtin(
            "is_logged_in",
            "True when a user is present in the render context (the render is authenticated).",
            Arc::new(|ctx: &VariableContext| Ok(json!(ctx.user.is_some()))),
        )?;
        self.register_builtin(
            "has_user",
            "True when a user is present in the render context. Same value as is_logged_in; both names are provided for template convenience.",
            Arc::new(|ctx: &VariableContext| Ok(json!(ctx.user.is_some()))),
        )?;
        self.register_builtin(
            "has_tenant",
            "True when a tenant is present in the render context.",
            Arc::new(|ctx: &VariableContext| Ok(json!(ctx.tenant.is_some()))),
        )?;
        self.register_builtin(
            "has_settings",
            "True when the render context carries at least one public setting.",
            Arc::new(|ctx: &VariableContext| Ok(json!(!ctx.settings.is_empty()))),
        )?;
        self.register_builtin(
            "has_extra",
            "True when the render context carries at least one entry in its extra module-data bag.",
            Arc::new(|ctx: &VariableContext| Ok(json!(!ctx.extra.is_empty()))),
        )?;
        self.register_builtin(
            "page",
            "The page being rendered: kind (page type), type (alias of kind), route_name (null when unknown), path, and url.",
            Arc::new(|ctx: &VariableContext| {
                Ok(json!({
                    "kind": ctx.page.kind,
                    "type": ctx.page.kind,
                    "route_name": ctx.page.route_name,
                    "path": ctx.page.path,
                    "url": ctx.page.url,
                }))
            }),
        )?;
        self.register_builtin(
            "route",
            "Routing info for the page being rendered: name (null when unknown), path, and url.",
            Arc::new(|ctx: &VariableContext| {
                Ok(json!({
                    "name": ctx.page.route_name,
                    "path": ctx.page.path,
                    "url": ctx.page.url,
                }))
            }),
        )?;
        Ok(())
    }

    /// Register a variable. Fails with HTTP 409 when a variable with
    /// this name already exists; use `replace` to overwrite one
    /// deliberately. Fails with HTTP 400 for an invalid name.
    pub fn register(
        &self,
        name: &str,
        description: &str,
        resolver: VariableResolver,
    ) -> UnifiedResult<()> {
        validate_variable_name(name)?;
        let mut entries = self.entries.write().expect("variable registry lock poisoned");
        if entries.contains_key(name) {
            return Err(UnifiedError::conflict(format!(
                "Template variable \"{name}\" is already registered; use replace to overwrite it"
            )));
        }
        entries.insert(
            name.to_string(),
            VariableEntry {
                description: description.to_string(),
                builtin: false,
                resolver,
            },
        );
        Ok(())
    }

    /// Register a boolean presence flag: a variable that is true
    /// when the render context's extra data bag contains `key`, and
    /// false otherwise. This is the pattern for `has_<thing>` names a
    /// module needs (for example `has_products` for key `products`):
    /// instead of pre-registering an unbounded set of names, each
    /// module registers the flags it actually uses, reading its own
    /// module data from `ctx.extra`. Presence means the key exists in
    /// the bag, even when its value is JSON null. Fails with HTTP 409
    /// when `name` is already registered and with HTTP 400 for an
    /// invalid name, like `register`.
    ///
    /// ```ignore
    /// registry.register_presence_flag("has_products", "products")?;
    /// // Template: {{#if has_products}}...{{/if}}
    /// ```
    pub fn register_presence_flag(&self, name: &str, key: &str) -> UnifiedResult<()> {
        let key_owned = key.to_string();
        self.register(
            name,
            &format!("True when the render context carries \"{key}\" in its extra data bag."),
            Arc::new(move |ctx: &VariableContext| {
                Ok(json!(ctx.extra_value(&key_owned).is_some()))
            }),
        )
    }

    /// Overwrite a variable, registering it when it does not exist yet.
    /// Returns true when an existing variable was replaced. Built-in
    /// variables can be replaced this way, so a deployment can override
    /// e.g. `site` with data from its own source of truth.
    pub fn replace(
        &self,
        name: &str,
        description: &str,
        resolver: VariableResolver,
    ) -> UnifiedResult<bool> {
        validate_variable_name(name)?;
        let mut entries = self.entries.write().expect("variable registry lock poisoned");
        let replaced = entries.contains_key(name);
        let builtin = entries.get(name).is_some_and(|entry| entry.builtin);
        entries.insert(
            name.to_string(),
            VariableEntry {
                description: description.to_string(),
                builtin,
                resolver,
            },
        );
        Ok(replaced)
    }

    /// Remove a variable. Returns true when a variable was removed.
    pub fn unregister(&self, name: &str) -> bool {
        self.entries
            .write()
            .expect("variable registry lock poisoned")
            .remove(name)
            .is_some()
    }

    /// True when a variable with this name is registered.
    pub fn contains(&self, name: &str) -> bool {
        self.entries
            .read()
            .expect("variable registry lock poisoned")
            .contains_key(name)
    }

    /// Number of registered variables.
    pub fn len(&self) -> usize {
        self.entries
            .read()
            .expect("variable registry lock poisoned")
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Registered variable names, sorted alphabetically.
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .entries
            .read()
            .expect("variable registry lock poisoned")
            .keys()
            .cloned()
            .collect();
        names.sort();
        names
    }

    /// Catalog of every registered variable, sorted by name.
    pub fn catalog(&self) -> Vec<VariableDescriptor> {
        let mut descriptors: Vec<VariableDescriptor> = self
            .entries
            .read()
            .expect("variable registry lock poisoned")
            .iter()
            .map(|(name, entry)| VariableDescriptor {
                name: name.clone(),
                description: entry.description.clone(),
                builtin: entry.builtin,
            })
            .collect();
        descriptors.sort_by(|a, b| a.name.cmp(&b.name));
        descriptors
    }

    /// Description of one registered variable, or `None` when unknown.
    pub fn description(&self, name: &str) -> Option<String> {
        self.entries
            .read()
            .expect("variable registry lock poisoned")
            .get(name)
            .map(|entry| entry.description.clone())
    }

    /// Resolve one variable, or `Ok(None)` when it is not registered.
    /// A resolver error is returned to the caller (unlike `resolve_all`,
    /// which skips failures); the caller decides how to handle it.
    pub fn resolve(&self, name: &str, ctx: &VariableContext) -> UnifiedResult<Option<Value>> {
        let resolver = {
            let entries = self
                .entries
                .read()
                .expect("variable registry lock poisoned");
            match entries.get(name) {
                Some(entry) => Arc::clone(&entry.resolver),
                None => return Ok(None),
            }
        };
        resolver(ctx).map(Some)
    }

    /// Resolve every registered variable into a JSON object keyed by
    /// variable name. A resolver that fails is logged, omitted from the
    /// result, and never breaks the render or the other variables. Use
    /// `resolve_all_detailed` when the caller needs the failure list.
    pub fn resolve_all(&self, ctx: &VariableContext) -> UnifiedResult<Map<String, Value>> {
        let (values, failures) = self.resolve_all_detailed(ctx);
        for failure in &failures {
            tracing::warn!(
                "Template variable \"{}\" failed to resolve and was omitted from the render: {}",
                failure.name,
                failure.message
            );
        }
        Ok(values)
    }

    /// Like `resolve_all`, but also returns the per-variable failures
    /// instead of only logging them.
    pub fn resolve_all_detailed(
        &self,
        ctx: &VariableContext,
    ) -> (Map<String, Value>, Vec<VariableFailure>) {
        // Snapshot the resolvers first so a resolver that registers or
        // unregisters variables cannot deadlock on the registry lock.
        let snapshot: Vec<(String, VariableResolver)> = {
            let entries = self
                .entries
                .read()
                .expect("variable registry lock poisoned");
            entries
                .iter()
                .map(|(name, entry)| (name.clone(), Arc::clone(&entry.resolver)))
                .collect()
        };
        let mut values = Map::new();
        let mut failures = Vec::new();
        for (name, resolver) in snapshot {
            match resolver(ctx) {
                Ok(value) => {
                    values.insert(name, value);
                }
                Err(error) => {
                    failures.push(VariableFailure {
                        name,
                        message: error.message.clone(),
                    });
                }
            }
        }
        (values, failures)
    }

    /// Register one built-in variable. When the name already exists the
    /// existing entry is kept and the call succeeds, so
    /// `register_builtins` is idempotent.
    fn register_builtin(
        &self,
        name: &str,
        description: &str,
        resolver: VariableResolver,
    ) -> UnifiedResult<()> {
        validate_variable_name(name)?;
        let mut entries = self.entries.write().expect("variable registry lock poisoned");
        entries.entry(name.to_string()).or_insert(VariableEntry {
            description: description.to_string(),
            builtin: true,
            resolver,
        });
        Ok(())
    }
}

/// A variable name must start with an ASCII letter or `_` and contain
/// only ASCII letters, digits, and `_`, so it is always a valid
/// Handlebars identifier.
pub fn validate_variable_name(name: &str) -> UnifiedResult<()> {
    let mut chars = name.chars();
    let valid_start = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    let valid_rest = chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if name.is_empty() || !valid_start || !valid_rest {
        return Err(UnifiedError::bad_request(format!(
            "Invalid template variable name \"{name}\": use letters, digits, and '_' only, starting with a letter or '_'"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolver(value: Value) -> VariableResolver {
        Arc::new(move |_: &VariableContext| Ok(value.clone()))
    }

    fn failing_resolver() -> VariableResolver {
        Arc::new(|_: &VariableContext| Err(UnifiedError::internal("resolver blew up")))
    }

    fn populated_context() -> VariableContext {
        VariableContext::new()
            .with_site(
                SiteInfo::new("Acme Portal", "https://acme.example")
                    .with_description("Acme customer portal"),
            )
            .with_user(UserInfo::new("u-1", "Ada Lovelace", "ada@example.com", "admin"))
            .with_tenant(TenantInfo::new("t-1", "Acme Ltd"))
            .with_setting("theme", json!("dark"))
            .with_setting("locale", json!("en"))
    }

    #[test]
    fn register_and_resolve_round_trip() {
        let registry = VariableRegistry::new();
        registry
            .register("greeting", "A greeting.", resolver(json!("hello")))
            .unwrap();
        assert!(registry.contains("greeting"));
        assert_eq!(
            registry.resolve("greeting", &VariableContext::new()).unwrap(),
            Some(json!("hello"))
        );
        assert_eq!(
            registry.resolve("missing", &VariableContext::new()).unwrap(),
            None
        );
    }

    #[test]
    fn unregister_removes_variable() {
        let registry = VariableRegistry::new();
        registry
            .register("temp", "Temporary.", resolver(json!(1)))
            .unwrap();
        assert!(registry.unregister("temp"));
        assert!(!registry.contains("temp"));
        assert!(!registry.unregister("temp"));
    }

    #[test]
    fn duplicate_register_is_rejected() {
        let registry = VariableRegistry::new();
        registry
            .register("dup", "First.", resolver(json!(1)))
            .unwrap();
        let error = registry
            .register("dup", "Second.", resolver(json!(2)))
            .unwrap_err();
        assert_eq!(error.code, 409);
        // The original resolver is untouched by the rejected duplicate.
        assert_eq!(
            registry.resolve("dup", &VariableContext::new()).unwrap(),
            Some(json!(1))
        );
    }

    #[test]
    fn replace_overwrites_and_reports_previous_presence() {
        let registry = VariableRegistry::new();
        assert!(!registry
            .replace("item", "First.", resolver(json!(1)))
            .unwrap());
        assert!(registry
            .replace("item", "Second.", resolver(json!(2)))
            .unwrap());
        assert_eq!(
            registry.resolve("item", &VariableContext::new()).unwrap(),
            Some(json!(2))
        );
    }

    #[test]
    fn invalid_names_are_rejected() {
        let registry = VariableRegistry::new();
        for name in ["", "1abc", "has space", "has-dash", "has.dot", "with/slash"] {
            assert!(
                registry
                    .register(name, "Bad.", resolver(json!(null)))
                    .is_err(),
                "name {name:?} should be rejected"
            );
        }
        assert!(registry
            .register("_ok1", "Fine.", resolver(json!(null)))
            .is_ok());
    }

    #[test]
    fn names_and_catalog_are_sorted() {
        let registry = VariableRegistry::with_builtins();
        registry
            .register("zeta", "Last.", resolver(json!(null)))
            .unwrap();
        registry
            .register("alpha", "First.", resolver(json!(null)))
            .unwrap();
        assert_eq!(
            registry.names(),
            vec![
                "alpha",
                "has_extra",
                "has_settings",
                "has_tenant",
                "has_user",
                "is_admin",
                "is_admin_user",
                "is_dashboard",
                "is_home",
                "is_logged_in",
                "page",
                "route",
                "settings",
                "site",
                "tenant",
                "user",
                "zeta"
            ]
        );
        let catalog = registry.catalog();
        assert_eq!(catalog.len(), 17);
        assert!(catalog.iter().all(|item| !item.description.is_empty()));
        assert!(catalog
            .iter()
            .find(|item| item.name == "site")
            .is_some_and(|item| item.builtin));
        assert!(catalog
            .iter()
            .find(|item| item.name == "zeta")
            .is_some_and(|item| !item.builtin));
    }

    #[test]
    fn failing_resolver_does_not_break_resolve_all() {
        let registry = VariableRegistry::new();
        registry
            .register("good", "Works.", resolver(json!("ok")))
            .unwrap();
        registry
            .register("bad", "Fails.", failing_resolver())
            .unwrap();
        registry
            .register("also_good", "Works too.", resolver(json!(42)))
            .unwrap();
        let (values, failures) = registry.resolve_all_detailed(&VariableContext::new());
        assert_eq!(values.get("good"), Some(&json!("ok")));
        assert_eq!(values.get("also_good"), Some(&json!(42)));
        assert!(!values.contains_key("bad"));
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].name, "bad");
        // The lenient entry point also skips the failure and succeeds.
        let values = registry.resolve_all(&VariableContext::new()).unwrap();
        assert_eq!(values.len(), 2);
        assert!(!values.contains_key("bad"));
    }

    #[test]
    fn builtins_resolve_from_populated_context() {
        let registry = VariableRegistry::with_builtins();
        let ctx = populated_context();
        let values = registry.resolve_all(&ctx).unwrap();
        assert_eq!(
            values.get("site"),
            Some(&json!({
                "name": "Acme Portal",
                "url": "https://acme.example",
                "description": "Acme customer portal",
            }))
        );
        assert_eq!(
            values.get("user"),
            Some(&json!({
                "id": "u-1",
                "name": "Ada Lovelace",
                "email": "ada@example.com",
                "role": "admin",
                "authenticated": true,
            }))
        );
        assert_eq!(
            values.get("tenant"),
            Some(&json!({ "id": "t-1", "name": "Acme Ltd" }))
        );
        assert_eq!(
            values.get("settings"),
            Some(&json!({ "theme": "dark", "locale": "en" }))
        );
    }

    #[test]
    fn builtins_resolve_empty_for_anonymous_context() {
        let registry = VariableRegistry::with_builtins();
        let values = registry.resolve_all(&VariableContext::new()).unwrap();
        assert_eq!(
            values.get("site"),
            Some(&json!({
                "name": "Unified",
                "url": "",
                "description": "",
            }))
        );
        assert_eq!(
            values.get("user"),
            Some(&json!({
                "id": Value::Null,
                "name": Value::Null,
                "email": Value::Null,
                "role": Value::Null,
                "authenticated": false,
            }))
        );
        assert_eq!(
            values.get("tenant"),
            Some(&json!({ "id": Value::Null, "name": Value::Null }))
        );
        assert_eq!(values.get("settings"), Some(&json!({})));
    }

    #[test]
    fn register_builtins_is_idempotent() {
        let registry = VariableRegistry::with_builtins();
        registry.register_builtins().unwrap();
        assert_eq!(registry.len(), 15);
    }

    #[test]
    fn custom_resolver_reads_extra_bag() {
        let registry = VariableRegistry::new();
        registry
            .register(
                "cart_count",
                "Items in the cart, from module data.",
                Arc::new(|ctx: &VariableContext| {
                    Ok(ctx.extra_value("cart_count").cloned().unwrap_or(json!(0)))
                }),
            )
            .unwrap();
        let ctx = VariableContext::new().with_extra("cart_count", json!(3));
        assert_eq!(
            registry.resolve("cart_count", &ctx).unwrap(),
            Some(json!(3))
        );
        assert_eq!(
            registry
                .resolve("cart_count", &VariableContext::new())
                .unwrap(),
            Some(json!(0))
        );
    }

    #[test]
    fn page_info_defaults_to_custom_with_empty_location() {
        let page = PageInfo::default();
        assert_eq!(page.kind, "custom");
        assert_eq!(page.route_name, None);
        assert_eq!(page.path, "");
        assert_eq!(page.url, "");
        assert_eq!(VariableContext::new().page.kind, "custom");
    }

    #[test]
    fn page_info_builders_and_context_builders() {
        let page = PageInfo::new("admin", "/admin/users", "/admin/users?page=2")
            .with_route_name("admin_users");
        assert_eq!(page.kind, "admin");
        assert_eq!(page.route_name.as_deref(), Some("admin_users"));
        assert_eq!(page.path, "/admin/users");
        assert_eq!(page.url, "/admin/users?page=2");

        // with_page replaces the whole struct; with_page_kind and
        // with_route adjust parts of it.
        let ctx = VariableContext::new()
            .with_page(page.clone())
            .with_page_kind("dashboard");
        assert_eq!(ctx.page.kind, "dashboard");
        assert_eq!(ctx.page.route_name.as_deref(), Some("admin_users"));
        let ctx = ctx.with_route("home", "/", "/?ref=nav");
        assert_eq!(ctx.page.kind, "dashboard");
        assert_eq!(ctx.page.route_name.as_deref(), Some("home"));
        assert_eq!(ctx.page.path, "/");
        assert_eq!(ctx.page.url, "/?ref=nav");
    }

    fn logic_values(ctx: &VariableContext) -> Map<String, Value> {
        VariableRegistry::with_builtins()
            .resolve_all(ctx)
            .unwrap()
    }

    #[test]
    fn page_identity_booleans_follow_page_kind_only() {
        // Home page, anonymous: only is_home is true.
        let values = logic_values(&VariableContext::new().with_page_kind("home"));
        assert_eq!(values.get("is_home"), Some(&json!(true)));
        assert_eq!(values.get("is_admin"), Some(&json!(false)));
        assert_eq!(values.get("is_dashboard"), Some(&json!(false)));

        // Admin page, anonymous: only is_admin is true.
        let values = logic_values(&VariableContext::new().with_page_kind("admin"));
        assert_eq!(values.get("is_home"), Some(&json!(false)));
        assert_eq!(values.get("is_admin"), Some(&json!(true)));
        assert_eq!(values.get("is_dashboard"), Some(&json!(false)));

        // Dashboard page: only is_dashboard is true.
        let values = logic_values(&VariableContext::new().with_page_kind("dashboard"));
        assert_eq!(values.get("is_home"), Some(&json!(false)));
        assert_eq!(values.get("is_admin"), Some(&json!(false)));
        assert_eq!(values.get("is_dashboard"), Some(&json!(true)));

        // Default (custom) page: none of the three is true.
        let values = logic_values(&VariableContext::new());
        assert_eq!(values.get("is_home"), Some(&json!(false)));
        assert_eq!(values.get("is_admin"), Some(&json!(false)));
        assert_eq!(values.get("is_dashboard"), Some(&json!(false)));
    }

    #[test]
    fn is_admin_is_page_identity_not_user_role() {
        // An admin-role user on a non-admin page: is_admin stays
        // false, is_admin_user is true. The two must never be
        // conflated.
        let ctx = VariableContext::new()
            .with_page_kind("home")
            .with_user(UserInfo::new("u-1", "Ada", "ada@example.com", "admin"));
        let values = logic_values(&ctx);
        assert_eq!(values.get("is_admin"), Some(&json!(false)));
        assert_eq!(values.get("is_admin_user"), Some(&json!(true)));

        // A non-admin user on an admin page: is_admin is true,
        // is_admin_user is false.
        let ctx = VariableContext::new()
            .with_page_kind("admin")
            .with_user(UserInfo::new("u-2", "Bob", "bob@example.com", "member"));
        let values = logic_values(&ctx);
        assert_eq!(values.get("is_admin"), Some(&json!(true)));
        assert_eq!(values.get("is_admin_user"), Some(&json!(false)));

        // Anonymous render: both false.
        let values = logic_values(&VariableContext::new().with_page_kind("admin"));
        assert_eq!(values.get("is_admin_user"), Some(&json!(false)));
    }

    #[test]
    fn auth_and_presence_booleans() {
        // Anonymous, no tenant, no settings, no extra.
        let values = logic_values(&VariableContext::new());
        assert_eq!(values.get("is_logged_in"), Some(&json!(false)));
        assert_eq!(values.get("has_user"), Some(&json!(false)));
        assert_eq!(values.get("has_tenant"), Some(&json!(false)));
        assert_eq!(values.get("has_settings"), Some(&json!(false)));
        assert_eq!(values.get("has_extra"), Some(&json!(false)));

        // Logged in with tenant, settings, and extra data: all true.
        let ctx = VariableContext::new()
            .with_user(UserInfo::new("u-1", "Ada", "ada@example.com", "member"))
            .with_tenant(TenantInfo::new("t-1", "Acme Ltd"))
            .with_setting("theme", json!("dark"))
            .with_extra("cart_count", json!(3));
        let values = logic_values(&ctx);
        assert_eq!(values.get("is_logged_in"), Some(&json!(true)));
        assert_eq!(values.get("has_user"), Some(&json!(true)));
        assert_eq!(values.get("has_tenant"), Some(&json!(true)));
        assert_eq!(values.get("has_settings"), Some(&json!(true)));
        assert_eq!(values.get("has_extra"), Some(&json!(true)));
    }

    #[test]
    fn page_and_route_objects_expose_location() {
        let ctx = VariableContext::new().with_page(
            PageInfo::new("admin", "/admin/users", "/admin/users?page=2")
                .with_route_name("admin_users"),
        );
        let values = logic_values(&ctx);
        assert_eq!(
            values.get("page"),
            Some(&json!({
                "kind": "admin",
                "type": "admin",
                "route_name": "admin_users",
                "path": "/admin/users",
                "url": "/admin/users?page=2",
            }))
        );
        assert_eq!(
            values.get("route"),
            Some(&json!({
                "name": "admin_users",
                "path": "/admin/users",
                "url": "/admin/users?page=2",
            }))
        );

        // Default context: route name is null, path and url empty.
        let values = logic_values(&VariableContext::new());
        assert_eq!(
            values.get("page"),
            Some(&json!({
                "kind": "custom",
                "type": "custom",
                "route_name": Value::Null,
                "path": "",
                "url": "",
            }))
        );
        assert_eq!(
            values.get("route"),
            Some(&json!({
                "name": Value::Null,
                "path": "",
                "url": "",
            }))
        );
    }

    #[test]
    fn register_presence_flag_tracks_extra_key() {
        let registry = VariableRegistry::with_builtins();
        registry
            .register_presence_flag("has_products", "products")
            .unwrap();
        assert_eq!(
            registry
                .resolve("has_products", &VariableContext::new())
                .unwrap(),
            Some(json!(false))
        );
        let ctx = VariableContext::new().with_extra("products", json!([]));
        assert_eq!(
            registry.resolve("has_products", &ctx).unwrap(),
            Some(json!(true))
        );
        // A null value still counts as present: the key exists.
        let ctx = VariableContext::new().with_extra("products", Value::Null);
        assert_eq!(
            registry.resolve("has_products", &ctx).unwrap(),
            Some(json!(true))
        );
        // An unrelated extra key does not set the flag.
        let ctx = VariableContext::new().with_extra("notifications", json!(1));
        assert_eq!(
            registry.resolve("has_products", &ctx).unwrap(),
            Some(json!(false))
        );
        // Duplicate registration is rejected like register.
        assert!(registry
            .register_presence_flag("has_products", "products")
            .is_err());
        // Invalid names are rejected.
        assert!(registry
            .register_presence_flag("has-dash", "products")
            .is_err());
    }
}
