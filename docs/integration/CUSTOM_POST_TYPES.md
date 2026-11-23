# Custom Post Types + Tags Taxonomy (task 356)

Admin-definable custom post types (CPT) and the tags taxonomy for
the content module (`src/modules/content/`). Everything about a
type is data on its definition row — there is no per-type code
anywhere in the backend or the frontend.

## Model

### Tags

`Tag` (`src/modules/content/types.rs`): `id`, `tenant_id`, `name`,
kebab-case `slug` (unique per tenant), optional `description`,
timestamps. Durable table `content_tags` (migration 000133).

Tags attach through the `content_post_tags` join, one row per
`(tenant_id, target_kind, target_id, tag_id)` where `target_kind`
is `post` (a built-in post) or `custom` (a custom entry). Tag ids
are never embedded on the post/entry row. Custom entries keep a
`tag_ids` read snapshot that is updated together with the join;
the join is the authority.

Deleting a tag that is still assigned is a 409. Deleting a post
or an entry detaches its tags with it.

### Post type definitions

`PostTypeDefinition`: `id`, `tenant_id`, kebab-case `slug` (unique
per tenant, 1–64 chars), `singular_label` / `plural_label`,
`icon` (a module sprite symbol name, default `document`),
optional `description`, plus:

| Setting | Meaning |
| --- | --- |
| `supports` | Built-in features, subset of `title`, `editor`, `excerpt`, `thumbnail`, `categories`, `tags`, `custom-fields`. Entry writes carrying values for unsupported features are rejected (400), not silently dropped. |
| `fields` | Dynamic field definitions (see below). |
| `visibility` | `public` or `private`. Default `private` (fail closed). |
| `has_archive` | The type has a public archive listing. |
| `show_in_menu` / `menu_position` | Admin menu entry (React nav renders enabled types with `show_in_menu`, ordered by `menu_position`). |
| `rest_exposed` | The generic REST routes serve this type. `false` hides the type from every non-manage caller (404, no existence leak); admins can still manage it. |
| `enabled` | Soft-disable switch. |

Durable table `content_post_types` (migration 000133).

### Custom entries

`CustomEntry`: `id`, `tenant_id`, `type` (the type slug),
`title`, `slug` (unique per `(tenant, type)`), `content`
(editor body), `excerpt`, `status` (`draft` / `published`),
`category_id`, `author_id`, `thumbnail` (a document id),
`fields` (validated dynamic field values, keyed by field key),
`tag_ids` (join snapshot), timestamps, `published_at`.

**All entries of every type live in ONE generic store**
(`content_custom_entries`, JSONB `fields`). There is no table
per type. Indexes: `(tenant_id, type_slug, slug)` unique,
`(tenant_id, type_slug, status)`, `(tenant_id, category_id)`,
GIN on `fields`.

### Dynamic field definitions

`CustomFieldDef`: `key` (snake_case, unique within the type),
`label`, `type`, `required`, `default`, `options`.

| Type | Stored value | Validation |
| --- | --- | --- |
| `text` | string | ≤ 500 chars |
| `textarea` | string | ≤ 100,000 chars |
| `richtext` | string | ≤ 1,000,000 chars |
| `number` | JSON number | finite |
| `date` | string | `YYYY-MM-DD`, real calendar date |
| `datetime` | string | RFC 3339 |
| `select` | string | must be one of `options` |
| `multiselect` | string array | each must be one of `options`, ≤ 100 items, de-duplicated |
| `checkbox` | boolean | — |
| `media` | string | a document id (an object `{"document_id": "..."}` is normalized to the id) |
| `relation` | string | a UUID that must reference an existing post, page, or custom entry of the tenant |

`options` is the allowlist for `select` / `multiselect` (at
least 1, at most 100, unique) and must be empty for every other
type. A `default` is validated against its own definition and
applied on entry create when the field is absent. Unknown field
keys in an entry body are rejected (400). A missing required
field with no default is rejected (400). `null` for an optional
field means "absent" (the default applies).

## Routes

All routes live under `/api/v1/content` and use the standard
envelope (`{"data": ...}` / `{"error": {"message", "code",
"metadata"}}`). Capabilities: reads need `content.view`,
tag/post-type creation needs `content.create`, updates,
deletes, (un)assignment, and type management need
`content.manage`. Every handler resolves the tenant first
(`require_tenant`), checks the caller company against it, and
filters on the tenant id — a tenant can never see or touch
another tenant's tags, types, or entries.

### Tags

| Method | Path | Notes |
| --- | --- | --- |
| GET | `/tags` | `?search=`, pagination, sort `name` / `created_at` |
| POST | `/tags` | `{name, slug?, description?}` |
| GET | `/tags/{id}` | |
| PUT | `/tags/{id}` | `{name?, slug?, description?}` |
| DELETE | `/tags/{id}` | 409 while assigned |
| GET | `/posts/{id}/tags` | |
| POST | `/posts/{id}/tags` | `{tag_id? , tag_ids?}` — merged, duplicates ignored |
| DELETE | `/posts/{id}/tags/{tag_id}` | |
| GET | `/posts?filters[tag]=<id-or-slug>` | list posts carrying a tag |

### Post type definitions

| Method | Path | Notes |
| --- | --- | --- |
| GET | `/post-types` | `?filters[enabled]=true&filters[visibility]=public`, `?search=` |
| POST | `/post-types` | full definition; `visibility` defaults to `private` |
| GET | `/post-types/{id}` | |
| PUT | `/post-types/{id}` | a slug rename re-keys the type's entries |
| DELETE | `/post-types/{id}` | **soft-disable**: `enabled=false`, entries preserved |
| POST | `/post-types/{id}/enable` | re-enables; entries reappear |
| POST | `/post-types/{id}/disable` | same as DELETE |

Deleting/disabling a type hides its routes and UI but KEEPS its
data. There is no hard delete for a type — deleting a type must
never destroy its entries.

### Custom entries (generic routes, generated per type)

| Method | Path | Notes |
| --- | --- | --- |
| GET | `/custom/{type_slug}` | filters: `status`, `category`, `tag` (id or slug), `search` (title, content, slug, string field values); pagination; sort `created_at` / `updated_at` / `title` |
| POST | `/custom/{type_slug}` | `{title?, slug?, content?, excerpt?, category_id?, author_id?, thumbnail?, status?, fields?, tag_ids?}` |
| GET | `/custom/{type_slug}/{id}` | |
| PUT | `/custom/{type_slug}/{id}` | `fields` / `tag_ids` replace wholesale when present |
| DELETE | `/custom/{type_slug}/{id}` | hard delete of the entry; its tag links go with it |
| GET | `/custom/{type_slug}/slug/{slug}` | public render: published entries of enabled public types only |
| GET | `/custom/{type_slug}/entries/{id}/tags` | |
| POST | `/custom/{type_slug}/entries/{id}/tags` | `{tag_id?, tag_ids?}` |
| DELETE | `/custom/{type_slug}/entries/{id}/tags/{tag_id}` | |

Visibility: a disabled type, a private type, or a
non-REST type is 404 for callers without `content.manage`.
Drafts are visible only to `content.manage` holders; published
entries of enabled public types are visible to any viewer.
Unknown type slugs are 404 either way (no existence leak).

### Hooks

* `content.post_type_registered` — after a definition is created.
* `content.post_type_enabled` / `content.post_type_disabled` —
  after enable / disable / soft-delete.
* `content.published` / `content.unpublished` — for custom
  entries too, with kind `custom:{type_slug}`, so the documents
  module flips attached media the same way as for posts/pages.

## Auto-appear

Registering a definition is the whole deployment step:

1. **API** — the generic `/custom/{type_slug}` routes serve the
   type immediately; no route is registered per type.
2. **Admin menu** — the React shell (`assets/react/src/App.tsx`)
   fetches `GET /post-types?filters[enabled]=true` and renders a
   nav link per enabled type with `show_in_menu`, ordered by
   `menu_position`.
3. **React screens** — `assets/react/src/pages/CustomEntriesPage.tsx`
   is ONE generic component: it loads the definition by slug and
   renders the list + form from `supports` and `fields` (one
   input per field type, select/multiselect from `options`).
   `ContentTypesPage.tsx` (`/content/types`) manages the
   definitions themselves (labels, supports, field editor) and
   the tag taxonomy.
4. **Public render** — `GET /custom/{type_slug}/slug/{slug}`
   (and the archive list `GET /custom/{type_slug}`) for enabled
   public types with `has_archive`.

## Taxonomy wiring

* Posts: `category_id` (built-in) + tags via the join.
* Custom entries: `category_id` (when the type supports
  `categories`) + tags via the same join (when the type supports
  `tags`).
* Deleting a category is refused (409) while any post OR custom
  entry of the tenant uses it.

## Tenant scoping

* Runtime: every tag, definition, entry, and join row carries
  `tenant_id` (the middleware-resolved tenant) and every handler
  filters on it. Writes additionally require a resolved company
  scope (`require_caller_company`).
* Durable tables: provisioned per tenant schema
  (`unified_core::tenancy::schema_name`); `tenant_id` is the same
  partition key inside the table, so the rows stay correct if a
  deployment shares one schema.
* The older posts/categories/pages stores predate tenant
  partitioning (their records carry no tenant id); the new
  stores do not repeat that gap.

## Examples

Create a type for events with a date, a venue selector, and a
ticket price:

```json
POST /api/v1/content/post-types
{
  "slug": "events",
  "singular_label": "Event",
  "plural_label": "Events",
  "icon": "calendar",
  "description": "Community events and meetups.",
  "supports": ["title", "editor", "excerpt", "thumbnail", "categories", "tags", "custom-fields"],
  "fields": [
    {"key": "event_date", "label": "Event date", "type": "date", "required": true},
    {"key": "venue", "label": "Venue", "type": "select", "required": true,
     "options": ["Main hall", "Annex", "Online"]},
    {"key": "ticket_price", "label": "Ticket price", "type": "number", "default": 0},
    {"key": "featured", "label": "Featured", "type": "checkbox", "default": false}
  ],
  "visibility": "public",
  "has_archive": true,
  "show_in_menu": true,
  "menu_position": 10,
  "rest_exposed": true
}
```

Create an entry (the `fields` values are validated per the
definition; `event_date` is required, `venue` must be one of
the options):

```json
POST /api/v1/content/custom/events
{
  "title": "Harvest Festival",
  "content": "Join us...",
  "status": "published",
  "fields": {"event_date": "2026-12-05", "venue": "Main hall", "ticket_price": 15000},
  "tag_ids": ["<tag uuid>"]
}
```

Tag a post:

```json
POST /api/v1/content/posts/<post uuid>/tags
{"tag_ids": ["<tag uuid>", "<tag uuid>"]}
```

## Files

* `src/modules/content/tags.rs` — tag CRUD + post/tag assignment
  + shared join helpers.
* `src/modules/content/custom_types.rs` — definition CRUD,
  field validation, generic entry CRUD, entry tags, public
  render.
* `src/modules/content/types.rs`, `state.rs`, `helpers.rs`,
  `module.rs`, `posts.rs`, `categories.rs` — types, stores,
  sorting/validation helpers, routes, post tag filter, category
  guard.
* `crates/unified-core/migrations/2026-11-23-000133_content_tags_custom_types/`
  — `up.sql` / `down.sql`.
* `crates/unified-db-platform/src/schema/content.rs` — Diesel
  schema mirroring the migration.
* `assets/react/src/api.ts` — `fetchContentTags`,
  `fetchPostTypes`, `fetchCustomEntries`, and friends.
* `assets/react/src/pages/ContentTypesPage.tsx`,
  `assets/react/src/pages/CustomEntriesPage.tsx` — admin
  definitions/tags manager + the generic definition-driven
  list/form screens.

## Caveats

* The runtime stores are in-memory (DashMap), like the rest of
  the content module — the migration + Diesel schema are the
  durable shape, and the persistence follow-up still has to wire
  the handlers to the pool (the widgets module is the pattern).
  Data does not survive a restart until then.
* `relation` fields validate existence only (post / page /
  custom entry id); they do not check the target's type or
  visibility.
* `media` fields validate shape only (a document id); they do
  not check that the document exists or is visible to the caller.
