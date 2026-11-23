//! mcp - catalog.

use serde_json::{json, Value};

/// Column-name fragments that are never exposed, in any table, for any
/// caller. Checked against every column a query would return, and
/// scrubbed out of every returned row as defence in depth. A column
/// is denied when its lower-cased name contains one of these
/// fragments as a substring: `password_hash` matches `password`,
/// `api_key` matches `key`, and separator-less names such as
/// `passwordhash` or `secretkey` match too. The longer entries
/// (`api_key`, `private_key`, ...) are kept explicit even though
/// the bare `key` fragment already covers them, so the config
/// endpoint lists them by name. Substring matching accepts false
/// positives on innocent names that merely contain a fragment
/// (`tokenized`, `monkey`): fail closed, never leak a credential.
pub const DENYLISTED_COLUMNS: &[&str] = &[
    "password",
    "secret",
    "token",
    "credential",
    "key",
    "private_key",
    "api_key",
    "key_hash",
    "license_key",
    "totp",
    "salt",
];

/// One table the MCP `db.query` tool (and only that tool) may read,
/// with the exact columns it may return. The list is deliberately
/// small: tables that hold secrets (`users`, `licenses`,
/// `store_entitlements`, `vcs_provider_settings`) are absent
/// entirely, and `store_orders` exposes no `user_id` column.
pub struct ReadOnlyTable {
    /// Table name as it appears in crates/unified-core/src/db/schema.rs`.
    pub name: &'static str,
    /// Columns the tool may select, in output order.
    pub columns: &'static [&'static str],
    /// Column the free-text `search` argument matches against.
    pub search_column: &'static str,
    /// What the table holds, for the config endpoint.
    pub description: &'static str,
}

pub const READ_ONLY_TABLES: &[ReadOnlyTable] = &[
    ReadOnlyTable {
        name: "companies",
        columns: &[
            "id",
            "name",
            "slug",
            "is_active",
            "created_at",
            "updated_at",
        ],
        search_column: "name",
        description: "Companies (tenants) the site belongs to.",
    },
    ReadOnlyTable {
        name: "company_sites",
        columns: &[
            "id",
            "company_id",
            "name",
            "domain",
            "is_primary",
            "is_active",
            "created_at",
            "updated_at",
        ],
        search_column: "name",
        description: "Sites belonging to a company.",
    },
    ReadOnlyTable {
        name: "store_products",
        columns: &[
            "id",
            "name",
            "description",
            "price",
            "currency",
            "kind",
            "status",
            "slug",
            "created_at",
            "updated_at",
        ],
        search_column: "name",
        description: "Sellable products in the store catalog.",
    },
    ReadOnlyTable {
        name: "store_orders",
        columns: &[
            "id",
            "subtotal",
            "discount_total",
            "total",
            "currency",
            "coupon_code",
            "company_id",
            "status",
            "provider",
            "created_at",
            "updated_at",
        ],
        search_column: "status",
        description: "Store orders (no customer user id is exposed).",
    },
    ReadOnlyTable {
        name: "store_order_items",
        columns: &[
            "id",
            "order_id",
            "product_id",
            "name",
            "unit_price",
            "quantity",
            "line_total",
            "tier_name",
            "created_at",
            "updated_at",
        ],
        search_column: "name",
        description: "Line items of store orders.",
    },
];

/// Context sources assembled through the hook bus. Each source maps
/// to one `mcp.context.<source>` event and, where one exists, to the
/// owning module's read/list route. `db_table` names the
/// `READ_ONLY_TABLES` entry the source falls back to when the owning
/// module does not answer the hook; `None` means there is no database
/// table for the source at all (the module keeps its data in memory)
/// and the only honest answer is the gap note.
pub struct ContextSource {
    pub name: &'static str,
    pub description: &'static str,
    pub owner_module: &'static str,
    pub db_table: Option<&'static str>,
}

pub const CONTEXT_SOURCES: &[ContextSource] = &[
    ContextSource {
        name: "site",
        description: "Shared site context (site facts plus knowledgebase grounding) from the AI module's single context source.",
        owner_module: "ai",
        db_table: None,
    },
    ContextSource {
        name: "content",
        description: "Posts, categories, and pages from the Content module.",
        owner_module: "content",
        db_table: None,
    },
    ContextSource {
        name: "products",
        description: "Sellable products from Product Management and the store catalog.",
        owner_module: "products",
        db_table: Some("store_products"),
    },
    ContextSource {
        name: "orders",
        description: "Store orders and their line items.",
        owner_module: "store",
        db_table: Some("store_orders"),
    },
    ContextSource {
        name: "tickets",
        description: "Support tickets and their status.",
        owner_module: "tickets",
        db_table: None,
    },
    ContextSource {
        name: "knowledgebase",
        description: "Knowledgebase articles, ranked by the knowledgebase search.",
        owner_module: "knowledgebase",
        db_table: None,
    },
    ContextSource {
        name: "settings",
        description: "Public site settings (never credentials or secrets).",
        owner_module: "settings",
        db_table: None,
    },
    ContextSource {
        name: "email_templates",
        description: "Email templates (subject and body, no sending).",
        owner_module: "email_templates",
        db_table: None,
    },
    ContextSource {
        name: "newsletter",
        description: "Newsletter campaigns and their content.",
        owner_module: "newsletter",
        db_table: None,
    },
    ContextSource {
        name: "site_builder",
        description: "Website-builder pages, blocks, and sections.",
        owner_module: "site_builder",
        db_table: None,
    },
];

/// The tools this module ships. Every tool is read-only; there is no
/// write-capable tool in this module by design.
pub struct ToolDefinition {
    pub name: &'static str,
    pub description: &'static str,
    /// JSON Schema (as a JSON value) for the tool's arguments.
    pub input_schema: Value,
}

/// Small helper: a JSON Schema object with the given properties and
/// required list. Keeps the tool table readable.
pub(crate) fn tool_schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

pub(crate) fn paging_properties() -> Value {
    json!({
        "page": { "type": "integer", "minimum": 1, "default": 1 },
        "per_page": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 },
        "search": { "type": "string" },
    })
}

pub fn builtin_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "site.search",
            description: "Search every context source at once (shared site context, content, products, orders, tickets, knowledgebase, settings, email templates, newsletter, site-builder pages). Read-only.",
            input_schema: tool_schema(
                json!({
                    "query": { "type": "string" },
                    "sources": { "type": "array", "items": { "type": "string" } },
                    "page": { "type": "integer", "minimum": 1, "default": 1 },
                    "per_page": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 },
                }),
                &["query"],
            ),
        },
        ToolDefinition {
            name: "site.context",
            description: "Shared site context (site facts plus knowledgebase grounding) from the AI module's single context source. Read-only.",
            input_schema: tool_schema(
                json!({
                    "query": { "type": "string" },
                }),
                &[],
            ),
        },
        ToolDefinition {
            name: "content.list",
            description: "List content posts and pages. Read-only; assembled from the Content module through the mcp.context.content hook.",
            input_schema: tool_schema(paging_properties(), &[]),
        },
        ToolDefinition {
            name: "content.get",
            description: "Get one content post or page by id. Read-only.",
            input_schema: tool_schema(
                json!({ "id": { "type": "string" } }),
                &["id"],
            ),
        },
        ToolDefinition {
            name: "products.list",
            description: "List sellable products. Read-only; assembled from Product Management through the mcp.context.products hook, with a read-only store_products query as fallback.",
            input_schema: tool_schema(paging_properties(), &[]),
        },
        ToolDefinition {
            name: "products.get",
            description: "Get one product by id. Read-only.",
            input_schema: tool_schema(
                json!({ "id": { "type": "string" } }),
                &["id"],
            ),
        },
        ToolDefinition {
            name: "orders.list",
            description: "List store orders. Read-only; assembled from the Store module through the mcp.context.orders hook, with a read-only store_orders query as fallback.",
            input_schema: tool_schema(paging_properties(), &[]),
        },
        ToolDefinition {
            name: "tickets.list",
            description: "List support tickets. Read-only; assembled from the Tickets module through the mcp.context.tickets hook.",
            input_schema: tool_schema(paging_properties(), &[]),
        },
        ToolDefinition {
            name: "knowledgebase.search",
            description: "Ranked full-text search over knowledgebase articles, answered by the knowledgebase.search hook. Read-only.",
            input_schema: tool_schema(
                json!({
                    "query": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 50, "default": 5 },
                }),
                &["query"],
            ),
        },
        ToolDefinition {
            name: "email_templates.list",
            description: "List email templates (subject and body). Read-only; sending is never exposed.",
            input_schema: tool_schema(paging_properties(), &[]),
        },
        ToolDefinition {
            name: "newsletter.list",
            description: "List newsletter campaigns and their content. Read-only.",
            input_schema: tool_schema(paging_properties(), &[]),
        },
        ToolDefinition {
            name: "site_builder.pages.list",
            description: "List website-builder pages. Read-only; assembled from the Site Builder module through the mcp.context.site_builder hook.",
            input_schema: tool_schema(paging_properties(), &[]),
        },
        ToolDefinition {
            name: "db.query",
            description: "Read-only query over the explicit allowlist of tables and columns (see the module config endpoint). Only bound parameters are accepted; no raw SQL, no writes, no DDL, no credential columns.",
            input_schema: tool_schema(
                json!({
                    "table": { "type": "string" },
                    "columns": { "type": "array", "items": { "type": "string" } },
                    "search": { "type": "string" },
                    "page": { "type": "integer", "minimum": 1, "default": 1 },
                    "per_page": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 },
                }),
                &["table"],
            ),
        },
    ]
}

/// Resources this module exposes. A resource URI is resolved through
/// the same hook context fetch as the matching tool.
pub const RESOURCES: &[(&str, &str, &str)] = &[
    ("unified://site/context", "Shared site context", "site"),
    ("unified://content/posts", "Content posts and pages", "content"),
    ("unified://products", "Sellable products", "products"),
    ("unified://orders", "Store orders", "orders"),
    ("unified://tickets", "Support tickets", "tickets"),
    ("unified://knowledgebase/articles", "Knowledgebase articles", "knowledgebase"),
    ("unified://settings/public", "Public site settings", "settings"),
    ("unified://email-templates", "Email templates", "email_templates"),
    ("unified://newsletter/campaigns", "Newsletter campaigns", "newsletter"),
    ("unified://site-builder/pages", "Website-builder pages", "site_builder"),
];

/// Prompts this module exposes. Arguments are substituted into the
/// prompt text only; a prompt never executes anything.
pub struct PromptDefinition {
    pub name: &'static str,
    pub description: &'static str,
    pub arguments: &'static [(&'static str, &'static str, bool)],
    pub template: &'static str,
}

pub const PROMPTS: &[PromptDefinition] = &[
    PromptDefinition {
        name: "site-brief",
        description: "Brief an assistant on this site: what it sells, what it publishes, and where its help content lives.",
        arguments: &[("focus", "What to emphasise (for example: products, support, content)", false)],
        template: "You are helping with the site served by this MCP server. Use only the context the tools return: products from products.list, published content from content.list, and help articles from knowledgebase.search. Focus: {{focus}}. Never invent prices, articles, or tickets that the tools did not return.",
    },
    PromptDefinition {
        name: "support-triage",
        description: "Triage open support tickets: summarise what is open and what the knowledgebase already covers.",
        arguments: &[("ticket_id", "One ticket to triage in depth (optional)", false)],
        template: "Triage the support tickets from tickets.list (or the single ticket {{ticket_id}} when one is given). For each ticket, check knowledgebase.search for an article that answers it and say so explicitly; do not claim an article exists when the search returned none.",
    },
    PromptDefinition {
        name: "product-announcement",
        description: "Draft a product announcement from the product catalog and the latest newsletter content.",
        arguments: &[("product_id", "The product to announce", true)],
        template: "Draft a short announcement for product {{product_id}} using only products.get and newsletter.list. Keep every price, name, and feature exactly as the tools returned it; if a detail is missing from the tools, leave it out instead of inventing it.",
    },
    PromptDefinition {
        name: "knowledgebase-answer",
        description: "Answer a question grounded in the knowledgebase, with citations to the articles used.",
        arguments: &[("question", "The question to answer", true)],
        template: "Answer this question using only knowledgebase.search: {{question}}. Cite the article id and title for every claim. When no article covers the question, say so plainly instead of answering from general knowledge.",
    },
];
