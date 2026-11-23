# Theme Variables

Template variables are named values that Handlebars templates read with
plain `{{variable}}` / `{{variable.field}}` expressions. They are
registered in a `VariableRegistry` (`src/themes/variables.rs`) together
with a resolver closure, and the `ThemeEngine` resolves them at render
time against a `VariableContext` (site info, current user, tenant, public
settings, plus an extensible bag of module data) and merges the results
into the template context.

## Built-in variables

These are registered by default when a `ThemeEngine` is constructed
(`VariableRegistry::with_builtins`). No call site has to pass them.

### `site`

Public site identity, resolved from `VariableContext::site`.

| Field | Type | Description |
| --- | --- | --- |
| `name` | string | Site name. Defaults to `"Unified"` when no site is set. |
| `url` | string | Public site URL. Empty string when unset. |
| `description` | string | Short site description. Empty string when unset. |

Example:

```handlebars
<title>{{site.name}}{{#if site.description}} &mdash; {{site.description}}{{/if}}</title>
<a href="{{site.url}}">Visit the site</a>
```

### `user`

Current user, resolved from `VariableContext::user`.

| Field | Type | Description |
| --- | --- | --- |
| `id` | string \| null | User id, or `null` when anonymous. |
| `name` | string \| null | Display name, or `null` when anonymous. |
| `email` | string \| null | Email address, or `null` when anonymous. |
| `role` | string \| null | Role (for example `"admin"`), or `null` when anonymous. |
| `authenticated` | boolean | `true` when a user is present, `false` for anonymous renders. |

Example:

```handlebars
{{#if user.authenticated}}
  <p>Welcome back, {{user.name}} ({{user.role}}).</p>
{{else}}
  <p><a href="/login">Sign in</a></p>
{{/if}}
```

### `tenant`

Current tenant, resolved from `VariableContext::tenant`.

| Field | Type | Description |
| --- | --- | --- |
| `id` | string \| null | Tenant id, or `null` when no tenant context applies. |
| `name` | string \| null | Tenant name, or `null` when no tenant context applies. |

Example:

```handlebars
{{#if tenant.id}}<p>Tenant: {{tenant.name}}</p>{{/if}}
```

### `settings`

Curated public site settings (theme, locale, feature flags), resolved
from `VariableContext::settings`. The shape is whatever the call site
put into the context; it is an object, possibly empty.

Example:

```handlebars
<html data-theme="{{settings.theme}}" lang="{{settings.locale}}">
```

## Logic variables

Booleans (and two objects) for template branching with
`{{#if ...}}`. They are derived from `VariableContext::page` (which
page is being rendered), `user`, `tenant`, `settings`, and `extra`,
so a call site that supplies a page context gets branching for free.

### Page identity

Decided purely by the page kind on `VariableContext::page`
(`"home"`, `"admin"`, `"dashboard"`, `"custom"`, or any
module-defined type). Page identity says which page is being
rendered; it says nothing about the current user's role.

| Variable | True when |
| --- | --- |
| `is_home` | page kind is `"home"` |
| `is_admin` | page kind is `"admin"` |
| `is_dashboard` | page kind is `"dashboard"` |

Example:

```handlebars
{{#if is_admin}}
  <nav class="admin-nav">...</nav>
{{else if is_dashboard}}
  <nav class="dashboard-nav">...</nav>
{{else}}
  <nav class="public-nav">...</nav>
{{/if}}
```

### Auth and presence

| Variable | True when |
| --- | --- |
| `is_logged_in` | a user is present in the render context |
| `has_user` | same as `is_logged_in` (both names are provided) |
| `is_admin_user` | the current user's role is `"admin"` (about the **user**, not the page) |
| `has_tenant` | a tenant is present in the render context |
| `has_settings` | the render context carries at least one public setting |
| `has_extra` | the render context carries at least one entry in its extra data bag |
| `has_data` | the per-call template data object is non-empty (see below) |

`is_admin` and `is_admin_user` are deliberately different: an
admin-role user on the home page sees `is_admin` false and
`is_admin_user` true, while a non-admin user on an admin page sees
the opposite. Use `is_admin` to branch on the page, `is_admin_user`
to branch on who is viewing it.

Example:

```handlebars
{{#if is_logged_in}}
  <p>Welcome back, {{user.name}}.</p>
  {{#if is_admin_user}}<a href="/admin">Admin area</a>{{/if}}
{{else}}
  <p><a href="/login">Sign in</a></p>
{{/if}}
```

### `has_data`

`has_data` is true exactly when the per-call template data passed to
`render` / `render_with_context` is a non-empty JSON object, and
false for an empty object. It is **not** a registry variable: a
resolver runs before the merge and cannot see the per-call data, so
the engine injects `has_data` after merging, overriding any
per-call value under that name. (That override is the one deliberate
exception to the "per-call data wins" precedence rule.) When the
per-call data is not a JSON object, `has_data` is not injected at
all.

Example:

```handlebars
{{#if has_data}}
  <h1>{{title}}</h1>
{{else}}
  <p>Nothing to show yet.</p>
{{/if}}
```

Note that dashboard pages always pass a populated data object
(title, nav, user, ...), so `has_data` is true there; it is most
useful for templates whose call site may pass an empty object.

### `page` and `route`

Objects describing the page being rendered, resolved from
`VariableContext::page`.

| Field | Type | Description |
| --- | --- | --- |
| `page.kind` | string | Page type, for example `"home"`, `"admin"`, `"dashboard"`, `"custom"`. |
| `page.type` | string | Alias of `page.kind`, for templates that read more naturally with `type`. |
| `page.route_name` | string \| null | Name of the matched route, or `null` when unknown. |
| `page.path` | string | Request path, for example `"/admin/users"`. Empty when unset. |
| `page.url` | string | Request URI (path plus query string), for example `"/admin/users?page=2"`. Not an absolute URL. |
| `route.name` | string \| null | Same as `page.route_name`. |
| `route.path` | string | Same as `page.path`. |
| `route.url` | string | Same as `page.url`. |

Example:

```handlebars
<link rel="canonical" href="{{site.url}}{{page.path}}">
{{#if route.name}}<p>Route: {{route.name}}</p>{{/if}}
```

### `has_<thing>`: presence flags for module data

There is no pre-registered infinite set of `has_<thing>` names.
Instead, a module registers the flags it actually uses with
`VariableRegistry::register_presence_flag(name, key)`, which
registers a boolean variable that is true when the render context's
extra data bag contains `key` (presence means the key exists, even
with a JSON null value), and false otherwise. The module puts the
data into the context with `VariableContext::with_extra`.

```rust
// The module registers its flag once, for example at startup:
engine.variables().register_presence_flag("has_products", "products")?;

// Each render carries the data in the context's extra bag:
let ctx = VariableContext::new()
    .with_extra("products", serde_json::json!([/* ... */]));

let html = engine.render_with_context("home", &data, &ctx)?;
```

```handlebars
{{#if has_products}}
  <ul>{{#each products}}<li>{{this.name}}</li>{{/each}}</ul>
{{else}}
  <p>No products yet.</p>
{{/if}}
```

For anything more than a presence check, register a normal variable
with a resolver that reads `ctx.extra_value("key")` (see
"Registering a custom variable" below).

> **Security rule:** `settings` must only ever contain public values.
> Never put secrets, credential material, password hashes, or API keys
> into `VariableContext::settings` or `VariableContext::extra` — anything
> in the context can end up rendered into HTML.

## Rendering with a context

`ThemeEngine::render` resolves variables against an empty context
(default site, anonymous user, no tenant, no settings, page kind
`"custom"`). To supply render-time data, use `render_with_context`:

```rust
use crate::themes::variables::{PageInfo, SiteInfo, UserInfo, VariableContext};

let ctx = VariableContext::new()
    .with_site(SiteInfo::new("Acme Portal", "https://acme.example"))
    .with_user(UserInfo::new("u-1", "Ada Lovelace", "ada@example.com", "admin"))
    .with_setting("theme", serde_json::json!("dark"))
    .with_page(PageInfo::new("admin", "/admin/users", "/admin/users"));

let html = engine.render_with_context("home", &data, &ctx)?;
```

`with_page_kind("admin")` sets only the page type, and
`with_route("admin_users", "/admin/users", "/admin/users")` sets the
route name, path, and URL. The dashboard page renderer
(`render_page_response` in `src/modules/dashboards/service.rs`)
builds this context from the request: page kind `"admin"` for
admin templates and `"dashboard"` for user dashboard templates,
plus `req.path()` / `req.uri()` and the caller profile from the
page data.

## Precedence

Registry variables form the base of the template context; the per-call
JSON data is overlaid on top. **On a name collision, per-call data wins**,
so a call site can override a registered variable by passing the same
key:

```rust
// The template sees site.name == "Override", not the registry value.
engine.render("home", &serde_json::json!({ "site": { "name": "Override" } }))?;
```

When the per-call data is not a JSON object, the registered variables
are not merged and the data is rendered as-is.

## Registering a custom variable

Any module can register a variable through the engine (or directly on
the registry). The resolver receives the render-time context and returns
the JSON value the template sees.

```rust
use std::sync::Arc;
use serde_json::json;
use crate::themes::variables::VariableContext;

// Read module data from the context's extensible bag.
engine.register_variable(
    "cart_count",
    "Number of items in the current user's cart.",
    Arc::new(|ctx: &VariableContext| {
        Ok(ctx
            .extra_value("cart_count")
            .cloned()
            .unwrap_or(json!(0)))
    }),
)?;
```

```handlebars
{{#if cart_count}}<span class="cart-badge">{{cart_count}}</span>{{/if}}
```

Other registry operations:

```rust
let registry = engine.variables();

// Inspect what is available.
let names: Vec<String> = registry.names();
let catalog = registry.catalog(); // name, description, builtin flag

// Remove a variable again.
registry.unregister("cart_count");

// Deliberately overwrite a variable (register rejects duplicates).
registry.replace(
    "cart_count",
    "Number of items in the current user's cart.",
    Arc::new(|_: &VariableContext| Ok(json!(0))),
)?;
```

### Rules for custom variables

- **Name:** start with an ASCII letter or `_`, then ASCII letters,
  digits, and `_` only (for example `cart_count`). Names with dashes,
  dots, or spaces are rejected with HTTP 400 so the name is always a
  valid Handlebars identifier.
- **Duplicates:** `register` rejects an already-registered name with
  HTTP 409. A module can never silently shadow another module's
  variable; use `replace` when overwriting is intentional.
- **Failures:** a resolver that returns an error does **not** fail the
  render. The failure is logged, the variable is omitted from the
  template context, and every other variable still resolves. Use
  `resolve_all_detailed` when the caller needs the list of failures
  instead of only the log lines.
- **Thread safety:** the registry is `RwLock`-backed and resolvers must
  be `Send + Sync`. Resolvers should be cheap and side-effect free;
  they run on every render.
- **No secrets:** resolvers must only return data that is safe to
  render into HTML.

## API reference

Defined in `src/themes/variables.rs`, re-exported from `crate::themes`.

| Item | Description |
| --- | --- |
| `VariableRegistry` | Thread-safe registry: `register`, `replace`, `unregister`, `contains`, `names`, `catalog`, `description`, `resolve`, `resolve_all`, `resolve_all_detailed`, `register_builtins`, `with_builtins`, `register_presence_flag`. |
| `VariableResolver` | `Arc<dyn Fn(&VariableContext) -> UnifiedResult<serde_json::Value> + Send + Sync>`. |
| `VariableContext` | Render-time data: `site`, `user`, `tenant`, `settings`, `extra`, `page`; builder methods `with_site`, `with_user`, `with_tenant`, `with_setting`, `with_settings`, `with_extra`, `with_page`, `with_page_kind`, `with_route`; `extra_value` reader. |
| `SiteInfo` / `UserInfo` / `TenantInfo` / `PageInfo` | Plain data structs carried by the context. `PageInfo`: `kind` (free-form page type), `route_name`, `path`, `url`; builders `new`, `with_kind`, `with_route_name`, `with_path`, `with_url`. |
| `VariableDescriptor` | Catalog entry: `name`, `description`, `builtin`. |
| `VariableFailure` | One skipped resolver: `name`, `message`. |
| `validate_variable_name` | Name validation used by `register` / `replace`. |
| `ThemeEngine::variables` | Access the engine's registry. |
| `ThemeEngine::register_variable` | Register a variable on the engine. |
| `ThemeEngine::render_with_context` | Render with an explicit `VariableContext`. Injects `has_data` after the merge (see "Logic variables"). |
| `ThemeValidationReport` / `ThemeIssue` | Structured validity report: `valid`, `errors`, `warnings`, `templates_checked`, `module_templates_checked`; each issue has a machine-readable `code`, a `message`, and a `path`. |
| `ThemeEngine::validate_theme` | Validate a theme without activating it; returns the full report, never an error. |
| `ThemeEngine::set_active_theme` | Validates first and refuses (HTTP 400, all errors listed) to activate an invalid theme. |
| `ModuleTemplateRegistry` | Store of module template sources: `register`, `replace`, `unregister`, `contains`, `get_source`, `names`, `sources`. |
| `ThemeEngine::register_module_template` / `replace_module_template` / `unregister_module_template` | Register, overwrite, or remove a module template (`module:<module>/<name>`). |
| `ThemeEngine::render_module_template` / `render_module_template_with_context` | Render a module template; theme override wins over the module default, unknown template is a 404. |
| `module_template_key` / `parse_module_template_key` / `validate_module_template_name` | Key format `module:<module>/<name>`, parsing, and name validation for module templates. |

## Theme validity validation

`ThemeEngine::load_theme` fails on the first problem it finds, which
is fine for loading but useless for reporting. For a full picture,
`ThemeEngine::validate_theme(id)` (defined in
`src/themes/validity.rs`) checks everything it can and returns a
`ThemeValidationReport` instead of failing early:

```rust
let report = engine.validate_theme("dark");
assert!(report.valid); // false when report.errors is non-empty
for issue in &report.errors {
    println!("{}: {} ({:?})", issue.code, issue.message, issue.path);
}
```

```rust
pub struct ThemeValidationReport {
    pub id: String,
    pub valid: bool, // true exactly when errors is empty
    pub errors: Vec<ThemeIssue>,   // fatal: the theme cannot activate
    pub warnings: Vec<ThemeIssue>, // non-fatal: the theme still activates
    pub templates_checked: usize,
    pub module_templates_checked: usize,
}

pub struct ThemeIssue {
    pub code: String,    // machine-readable, see below
    pub message: String, // human-readable
    pub path: Option<String>, // file concerned, e.g. "templates/home.hbs"
}
```

The validator checks: the theme id, the theme directory, the
`theme.yaml` manifest (presence, YAML syntax, non-empty `name` and
`version`), the required `templates/layout.hbs`, every
manifest-declared template file (including `template_files` mapping
targets), and the Handlebars syntax of every `.hbs` file the theme
ships — templates, `partials/`, and per-module overrides — plus the
registered module templates (re-compiled defensively; they are
already compiled at registration time).

Error codes (each one makes `valid` false):

| Code | Meaning |
| --- | --- |
| `invalid-theme-id` | id fails theme-id validation |
| `missing-theme-dir` | `themes/<id>/` does not exist |
| `missing-manifest` | no `theme.yaml` |
| `unreadable-manifest` | `theme.yaml` cannot be read |
| `invalid-yaml` | `theme.yaml` is not valid YAML / not a mapping / has invalid field types |
| `invalid-manifest-field` | manifest `name` or `version` missing or empty |
| `missing-layout` | `templates/layout.hbs` does not exist |
| `missing-template` | a declared (or required) template file, or a `template_files` target, does not exist |
| `invalid-template-name` | a declared template name or `template_files` stem breaks the naming rules |
| `template-too-large` | a `.hbs` file exceeds `MAX_TEMPLATE_SIZE` (5 MiB) |
| `unreadable-template` | a `.hbs` file cannot be read |
| `invalid-template-syntax` | a `.hbs` file (or a registered module template) fails to parse as Handlebars |

Warning codes (never invalidate the theme):

| Code | Meaning |
| --- | --- |
| `missing-screenshot` | the theme ships no screenshot file |
| `unknown-manifest-field` | a top-level `theme.yaml` key the loader does not know |
| `undeclared-template` | a `.hbs` file directly under `templates/` that the manifest does not declare, so the loader ignores it (files in subdirectories are per-module overrides and are never warned about) |

### Activation refuses invalid themes

`set_active_theme` validates first and refuses to activate an
invalid theme: it returns HTTP 400 whose message lists every error
(`"missing-layout: …; invalid-template-syntax: …"`) and whose
metadata carries `theme_id` and `validation_errors` (the error
codes). The previously active theme is left untouched.
`load_theme` keeps its original behaviour (first-error `Err`, no
activation), so existing callers are unaffected.

## Per-module templates

Modules ship their own Handlebars templates, resolved by the theme
engine (`src/themes/module_templates.rs`). A module registers a
template from an in-code source:

```rust
engine.register_module_template(
    "billing",
    "invoice",
    "<h1>Invoice {{title}}</h1><p>{{site.name}}</p>",
)?;
```

Templates are addressed as `module:<module>/<name>` — for example
`module:billing/invoice` — and rendered with the dedicated method
(this is the only API for module templates; plain `render` does not
accept `module:` names):

```rust
let html = engine.render_module_template("billing", "invoice", &data)?;
// With an explicit variable context:
let html = engine.render_module_template_with_context(
    "billing", "invoice", &data, &ctx,
)?;
```

Rendering resolves registry variables (`{{site.name}}`,
`{{#if is_admin}}`, …) exactly like `render`, with per-call data
overlaid on top. Other operations:

```rust
engine.replace_module_template("billing", "invoice", "…")?; // overwrite
engine.unregister_module_template("billing", "invoice");     // remove
engine.has_module_template("billing", "invoice");             // renderable?
engine.module_template_names(); // ["module:billing/invoice", …]
engine.module_template_source("billing", "invoice"); // the module default
```

### Resolution order

1. The active theme's override, when the theme ships
   `templates/<module>/<name>.hbs` (for example
   `templates/billing/invoice.hbs`). Theme wins, so a theme can
   restyle module output.
2. The module's registered source.
3. Otherwise the render fails with a 404 `UnifiedError`.

A registered source whose key is currently overridden stays stored
but unrendered; switching to a theme without the override makes the
module default renderable again without re-registering, and
unregistering the stored default leaves the override in place.

### Rules

- **Names:** module and template names allow only ASCII letters,
  digits, `-` and `_`. `..`, `../evil`, `a/b`, and `a.b` are all
  rejected with HTTP 400, so a name can never escape its directory.
- **Syntax checked at registration:** the source is compiled when
  registered, so invalid Handlebars syntax fails `register` /
  `replace` with HTTP 400 and can never produce a broken render
  later. A failed `replace` leaves the previous source untouched.
- **Duplicates:** `register` rejects an already-registered template
  with HTTP 409; use `replace` to overwrite one deliberately.
- **Disk support:** module defaults come from in-code sources only —
  there is no `register_module_template_dir`, so a module's
  templates are always explicit and versioned with the module's
  code. The only filesystem-backed module templates are theme
  overrides (`templates/<module>/<name>.hbs` in the theme
  directory). Module templates can use theme partials
  (`{{> header}}`) because they share the live registry.
- **Validity:** the validity report re-compiles registered module
  templates (`module_templates_checked`) and records any syntax
  failure as `invalid-template-syntax`.

### Templates shipped by the default theme

The embedded default theme ships list + detail overrides for 18
user-facing modules under `templates/default/<module>/` (36 files).
Each file is registered at theme load as `module:<module>/<name>`
and wins over any module-registered default with the same key. The
column/field names follow the owning module's record types, so a
call site can pass its list/detail JSON straight through.

| Module | Templates | List columns |
| --- | --- | --- |
| `store` | `list`, `detail` | name, kind, price, licensed, slug |
| `billing` | `list`, `detail` | number, reference, customer, balance, due date, paid |
| `tickets` | `list`, `detail` | title, priority, status, category, department |
| `crm` | `list`, `detail` | name, email, company, phone, created |
| `users` | `list`, `detail` | name, email, active, roles, created |
| `licensing` | `list`, `detail` | license key, product, status, max activations, expires |
| `projects` | `list`, `detail` | name, methodology, status, visibility, due |
| `pos` | `list`, `detail` | number, register, cashier, total, status, customer |
| `elearning` | `list`, `detail` | title, category, level, language, status, price |
| `exams` | `list`, `detail` | title, category, status, duration, opens, closes |
| `library` | `list`, `detail` | title, authors, ISBN, publisher, year, language |
| `academic` | `list`, `detail` | student number, user, program, enrolled, status |
| `hr` | `list`, `detail` | code, user, position, department, status, hired |
| `marketing` | `list`, `detail` | name, subject, status, sent, delivered, opened, clicked |
| `appointments` | `list`, `detail` | customer, service, staff, starts, ends |
| `events` | `list`, `detail` | title, slug, categories, starts, ends, timezone |
| `knowledgebase` | `list`, `detail` | title, category, status, tags, version, updated |
| `files` | `list`, `detail` | name, kind, size, type, folder |

Data contract for every `list` template:

```json
{
  "title": "Products",
  "items": [{ "id": "…", "name": "…", "detail_url": "/admin/store/…",
              "status": "active", "status_class": "badge-success" }],
  "pagination": { "page": 1, "per_page": 20, "total": 42, "total_pages": 3,
                  "has_prev": false, "has_next": true,
                  "prev_url": "", "next_url": "?page=2" },
  "create_url": "/admin/store/new",
  "empty_message": "Nothing here yet."
}
```

- `items` is the record array; an empty array renders the empty
  state, no `pagination` renders no pager.
- A row's `detail_url` turns the first column into a link; a row's
  `status_class` picks the status badge modifier (for example
  `badge-success`); without it the badge is `badge-neutral`.
- `create_url` renders the "New …" button only for admin-role users
  (`{{#if is_admin_user}}`).
- Money is printed as `{{price}} {{currency}}` with no formatting:
  JSON carries major units (see `unified_core::money::serde_major`).

Data contract for every `detail` template: `title`, one record as
`item`, and an optional `back_url` to the list. The panel is guarded
by `has_data` / `item`, so a missing record renders "Not found".
Sub-collections render under the field table when present:
`billing` renders `item.lines` (name, quantity, unit price, line
total, tax rate); `tickets` renders `item.comments` (author, body,
kind, created); `pos` renders `item.items` (product, SKU, quantity,
unit price, line total); `marketing` renders `item.stats` (queued,
sent, delivered, failed, skipped, opened, clicked, unsubscribed);
`appointments` renders `item.service` (name, duration, price,
capacity); `events` renders `item.venue` (name, address, capacity).

Status (2026-11-22): no module registers a default template in
code yet — `register_module_template` has zero production callers
and no route calls `render_module_template` — so these theme
overrides are currently the only server-side rendering of these
modules. Until a module registers a default or a route renders one,
module UIs keep rendering client-side from the dashboard JavaScript.
A theme author overrides one by shipping a file with the same path
in their own theme (`templates/<module>/<name>.hbs`); a module
author ships a default by registering the same key in code, and
the theme file still wins.

## Tests

Run on a machine with the Rust toolchain (the tests are standard
`#[test]` unit tests, no extra dev-dependencies):

```sh
cargo test -p unified themes::
```

This covers: register / unregister / duplicate rejection / replace /
name validation, a failing resolver not breaking `resolve_all`, built-in
variables from a populated and from an anonymous context, and engine
renders proving `{{site.name}}` renders from the registry, per-call
data winning on collision, and a custom variable rendering. The logic
variables add: `PageInfo` defaults and builders, page-identity
booleans per kind, `is_admin` (page) vs `is_admin_user` (role) kept
distinct, auth/presence booleans for populated vs empty contexts,
`page` / `route` object shapes, the presence-flag helper (including
null-valued keys counting as present), and engine renders proving
`{{#if is_admin}}` / `{{#if is_logged_in}}` branch correctly and
`{{#if has_data}}` follows the per-call data object. Validity adds:
a valid disk theme reporting valid with zero errors, missing
theme directory / manifest / layout, malformed YAML, empty
manifest name/version, declared-but-missing templates,
invalid Handlebars syntax (with several issues collected in one
report), unknown manifest keys / missing screenshots / undeclared
template files as warnings only, oversized templates, module
sources re-checked in the report, and `set_active_theme` refusing
an invalid theme while leaving the active theme at `default`.
Module templates add: key format and parsing, register / replace /
duplicate rejection / unregister, name validation rejecting
traversal, invalid syntax rejected at registration, and engine
renders proving a module template renders with registry and logic
variables, a theme override winning over the module default (in
both registration orders and surviving a switch back to the
default theme), and unknown module templates failing with 404.
