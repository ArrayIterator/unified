/* Site builder editor - GrapeJS.
 *
 * Admin page script for `/admin/site-builder` (content template
 * `admin-site-builder`). This replaces the old vanilla-TypeScript
 * editor (`assets/js/site-builder.ts`, deleted): the user agreed on
 * 2026-11-22 that the builder uses GrapeJS ("builder pakai grapejs
 * boleh sih!"), and the vanilla editor violated that agreement.
 *
 * GrapeJS 0.23.9 is bundled locally by Vite (see
 * `vite.site-builder.config.ts`); there is no runtime CDN. The
 * editor is the UI only: the server-side block model, the API
 * contract, rendering (`render_block`), and storage are unchanged.
 *
 *   GET    /api/v1/site-builder/pages?per_page=50
 *   POST   /api/v1/site-builder/pages                 {title, slug?, theme_id}
 *   GET    /api/v1/site-builder/pages/{id}
 *   DELETE /api/v1/site-builder/pages/{id}
 *   POST   /api/v1/site-builder/pages/{id}/publish
 *   POST   /api/v1/site-builder/pages/{id}/unpublish
 *   GET    /api/v1/site-builder/pages/{id}/render     (returns {html})
 *   GET/POST  /api/v1/site-builder/pages/{id}/blocks
 *   PUT/DELETE /api/v1/site-builder/pages/{id}/blocks/{block_id}
 *   PUT   /api/v1/site-builder/pages/{id}/blocks/order {ids}
 *   GET/POST  /api/v1/site-builder/pages/{id}/sections
 *   PUT/DELETE /api/v1/site-builder/pages/{id}/sections/{section_id}
 *   PUT   /api/v1/site-builder/pages/{id}/sections/order {ids}
 *
 * Block mapping (GrapeJS block/component <-> server block type):
 * every server block type is one GrapeJS component type (`sb-hero`,
 * `sb-text`, `sb-image`, `sb-gallery`, `sb-form`, `sb-custom`,
 * `sb-pricing-table`, `sb-marketplace-storefront`).
 * The wrapper component carries `data-block-id` (empty for a new
 * block) and `data-block-type`; its traits edit the block's content
 * fields, and the canvas preview rebuilds from the traits. Text
 * blocks edit in place (double-click the text).
 *
 * Content fields are the ones the server renderer reads (see
 * `src/modules/site_builder/rendering.rs`): hero reads heading,
 * subheading, image_url, cta_text, cta_url; text reads html (or
 * text); image reads url, alt, caption; gallery reads images (an
 * array of {url, alt}); form reads action, submit_label, fields
 * (array of {name, label, type, placeholder, required, options});
 * custom reads html; pricing_table reads pricing_table_id and plans;
 * marketplace_storefront reads vendor_slug.
 */

import grapesjs from "grapesjs";
import type { Component, ComponentDefinition, Editor, TraitProperties } from "grapesjs";
import "grapesjs/dist/css/grapes.min.css";
import "./editor.css";

// Small utilities

type JsonValue =
  | string
  | number
  | boolean
  | null
  | JsonValue[]
  | { [key: string]: JsonValue };

type StatusKind = "info" | "success" | "error" | "warning";

interface ApiErrorBody {
  error?: { message?: string };
}

interface SiteBuilderBlock {
  id: string;
  type: string;
  content: Record<string, unknown>;
  order: number;
}

interface SiteBuilderSection {
  id: string;
  name: string;
  layout: string;
  order: number;
}

interface SiteBuilderPage {
  id: string;
  title: string;
  slug: string;
  theme_id: string;
  status: string;
  blocks: SiteBuilderBlock[];
  sections: SiteBuilderSection[];
}

interface RenderResult {
  html: string;
}

const API = "/api/v1/site-builder/pages";

function byId(id: string): HTMLElement | null {
  return document.getElementById(id);
}

function escapeHtml(value: unknown): string {
  return String(value ?? "")
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

let statusBox: HTMLElement | null = null;

function showStatus(message: string, kind: StatusKind = "info"): void {
  if (!statusBox) {
    return;
  }
  statusBox.hidden = false;
  statusBox.textContent = message;
  statusBox.className = `flash flash-${kind}`;
}

function errorMessage(body: unknown, fallback: string): string {
  const envelope = body as ApiErrorBody | null;
  const message = envelope?.error?.message;
  return typeof message === "string" && message ? message : fallback;
}

// API client (same CSRF + envelope contract as the old editor)

let csrfToken: string | null = null;
let csrfTokenRequest: Promise<string | null> | null = null;

function fetchCsrfToken(): Promise<string | null> {
  if (csrfToken) {
    return Promise.resolve(csrfToken);
  }
  if (!csrfTokenRequest) {
    csrfTokenRequest = window
      .fetch("/api/v1/security/csrf-token", {
        headers: { Accept: "application/json" },
        credentials: "same-origin",
      })
      .then((response: Response): Promise<unknown> =>
        response.ok ? response.json() : Promise.resolve(null),
      )
      .then((body: unknown): string | null => {
        const envelope = body as { data?: { csrf_token?: unknown } } | null;
        const token = envelope?.data?.csrf_token;
        csrfToken = typeof token === "string" && token ? token : null;
        return csrfToken;
      })
      .catch((): null => null);
    void csrfTokenRequest.then(() => {
      // A failed fetch must not poison the cache: the next mutation
      // retries the token endpoint.
      if (!csrfToken) {
        csrfTokenRequest = null;
      }
    });
  }
  return csrfTokenRequest;
}

function isCsrfError(body: unknown): boolean {
  const envelope = body as ApiErrorBody | null;
  const message = envelope?.error?.message;
  return typeof message === "string" && message.indexOf("CSRF") !== -1;
}

function itemsFromEnvelope<T>(data: unknown): T[] {
  if (Array.isArray(data)) {
    return data as T[];
  }
  if (data !== null && typeof data === "object" && Array.isArray((data as { items?: unknown }).items)) {
    return (data as { items?: unknown }).items as T[];
  }
  return [];
}

async function requestOnce<T>(
  url: string,
  method: string,
  payload: unknown,
  token: string | null,
  retried: boolean,
): Promise<T> {
  const headers: Record<string, string> = {
    "Content-Type": "application/json",
    Accept: "application/json",
  };
  if (token) {
    headers["X-CSRF-Token"] = token;
  }
  const response = await window.fetch(url, {
    method,
    headers,
    credentials: "same-origin",
    body: payload === undefined ? undefined : JSON.stringify(payload),
  });
  const body: unknown = await response.json();
  if (!response.ok) {
    if (response.status === 403 && token && !retried && isCsrfError(body)) {
      // Token may have expired: drop it and retry once with a fresh token.
      csrfToken = null;
      csrfTokenRequest = null;
      const fresh = await fetchCsrfToken();
      if (!fresh) {
        throw new Error(errorMessage(body, `Request failed with HTTP ${response.status}`));
      }
      return requestOnce<T>(url, method, payload, fresh, true);
    }
    throw new Error(errorMessage(body, `Request failed with HTTP ${response.status}`));
  }
  const envelope = body as { data?: unknown } | null;
  return (envelope && envelope.data !== undefined ? envelope.data : body) as T;
}

async function request<T>(url: string, method?: string, payload?: unknown): Promise<T> {
  const verb = method || "GET";
  if (verb === "GET") {
    return requestOnce<T>(url, verb, payload, null, false);
  }
  const token = await fetchCsrfToken();
  return requestOnce<T>(url, verb, payload, token, false);
}

// Block definitions: server type <-> GrapeJS component

/** Content object for one block (free-form per type). */
type BlockContent = Record<string, unknown>;

function str(content: BlockContent, key: string): string {
  const value = content[key];
  return typeof value === "string" ? value : "";
}

function attrOf(comp: Component, key: string): string {
  const value = comp.getAttributes()[key];
  return typeof value === "string" ? value : "";
}

/** Non-empty fields only: the renderer defaults the rest. */
function compact(content: BlockContent): BlockContent {
  const out: BlockContent = {};
  for (const [key, value] of Object.entries(content)) {
    if (value === "" || value === null || value === undefined) {
      continue;
    }
    if (Array.isArray(value) && value.length === 0) {
      continue;
    }
    out[key] = value;
  }
  return out;
}

/** Parse a JSON textarea field; throws with a human message. */
function parseJsonField(raw: string, label: string): JsonValue {
  const trimmed = raw.trim();
  if (!trimmed) {
    return [];
  }
  try {
    return JSON.parse(trimmed) as JsonValue;
  } catch {
    throw new Error(`${label} is not valid JSON.`);
  }
}

/** Presentation children are inert: clicks select the wrapper. */
function inert(tagName: string, extra: Partial<ComponentDefinition> = {}): ComponentDefinition {
  return {
    tagName,
    draggable: false,
    droppable: false,
    selectable: false,
    hoverable: false,
    ...extra,
  } as ComponentDefinition;
}

interface BlockDef {
  /** Server block type, e.g. "hero" or "pricing_table". */
  type: string;
  /** GrapeJS component type, e.g. "sb-hero". */
  componentType: string;
  label: string;
  category: string;
  traits: TraitProperties[];
  defaultContent: () => BlockContent;
  /** Presentation children for the canvas, from stored content. */
  inner: (content: BlockContent) => ComponentDefinition[];
  /** Traits (wrapper attributes) for stored content. */
  traitAttributes: (content: BlockContent) => Record<string, string>;
  /** Stored content from a canvas component. */
  contentFrom: (comp: Component) => BlockContent;
}

function textTrait(name: string, label: string): TraitProperties {
  return { type: "text", name, label };
}

function areaTrait(name: string, label: string, placeholder?: string): TraitProperties {
  return { type: "textarea", name, label, placeholder };
}

/** The eight built-in block types. */
const BUILTIN_DEFS: BlockDef[] = [
  {
    type: "hero",
    componentType: "sb-hero",
    label: "Hero",
    category: "Blocks",
    traits: [
      textTrait("heading", "Heading"),
      areaTrait("subheading", "Subheading"),
      textTrait("image_url", "Image URL"),
      textTrait("cta_text", "Button text"),
      textTrait("cta_url", "Button URL"),
    ],
    defaultContent: () => ({ heading: "Welcome", subheading: "", image_url: "", cta_text: "", cta_url: "" }),
    inner: (content) => {
      const children: ComponentDefinition[] = [];
      const heading = str(content, "heading");
      if (heading) {
        children.push(inert("h1", { content: heading }));
      }
      const subheading = str(content, "subheading");
      if (subheading) {
        children.push(inert("p", { content: subheading, attributes: { class: "sb-hero-sub" } }));
      }
      const imageUrl = str(content, "image_url");
      if (imageUrl) {
        children.push(
          inert("img", { attributes: { src: imageUrl, alt: heading, class: "sb-hero-image" } }),
        );
      }
      const ctaText = str(content, "cta_text");
      if (ctaText) {
        children.push(
          inert("a", {
            content: ctaText,
            attributes: { href: str(content, "cta_url") || "#", class: "sb-hero-cta" },
          }),
        );
      }
      return children;
    },
    traitAttributes: (content) => ({
      heading: str(content, "heading"),
      subheading: str(content, "subheading"),
      image_url: str(content, "image_url"),
      cta_text: str(content, "cta_text"),
      cta_url: str(content, "cta_url"),
    }),
    contentFrom: (comp) =>
      compact({
        heading: attrOf(comp, "heading"),
        subheading: attrOf(comp, "subheading"),
        image_url: attrOf(comp, "image_url"),
        cta_text: attrOf(comp, "cta_text"),
        cta_url: attrOf(comp, "cta_url"),
      }),
  },
  {
    type: "text",
    componentType: "sb-text",
    label: "Text",
    category: "Blocks",
    // No traits: a text block edits in place (double-click the text).
    traits: [],
    defaultContent: () => ({ html: "Write something..." }),
    inner: (content) => [
      {
        type: "text",
        content: str(content, "html") || str(content, "text"),
      } as ComponentDefinition,
    ],
    traitAttributes: () => ({}),
    contentFrom: (comp) => {
      const inner = comp.components().at(0);
      const html = inner ? String(inner.get("content") ?? "") : "";
      return { html };
    },
  },
  {
    type: "image",
    componentType: "sb-image",
    label: "Image",
    category: "Blocks",
    traits: [
      textTrait("url", "Image URL"),
      textTrait("alt", "Alt text"),
      textTrait("caption", "Caption"),
    ],
    defaultContent: () => ({ url: "", alt: "", caption: "" }),
    inner: (content) => {
      const children: ComponentDefinition[] = [];
      const url = str(content, "url");
      if (url) {
        children.push(inert("img", { attributes: { src: url, alt: str(content, "alt") } }));
      }
      const caption = str(content, "caption");
      if (caption) {
        children.push(inert("p", { content: caption }));
      }
      return children;
    },
    traitAttributes: (content) => ({
      url: str(content, "url"),
      alt: str(content, "alt"),
      caption: str(content, "caption"),
    }),
    contentFrom: (comp) =>
      compact({
        url: attrOf(comp, "url"),
        alt: attrOf(comp, "alt"),
        caption: attrOf(comp, "caption"),
      }),
  },
  {
    type: "gallery",
    componentType: "sb-gallery",
    label: "Gallery",
    category: "Blocks",
    traits: [
      areaTrait("images", "Image URLs (one per line)", "https://.../one.jpg\nhttps://.../two.jpg"),
    ],
    defaultContent: () => ({ images: [] }),
    inner: (content) => {
      const images = Array.isArray(content.images) ? content.images : [];
      return images
        .map((image) => {
          const record = image as BlockContent;
          const url = typeof record === "string" ? record : str(record, "url");
          if (!url) {
            return null;
          }
          return inert("img", {
            attributes: { src: url, alt: typeof record === "string" ? "" : str(record, "alt") },
          });
        })
        .filter((item): item is ComponentDefinition => item !== null);
    },
    traitAttributes: (content) => {
      const images = Array.isArray(content.images) ? content.images : [];
      const lines = images
        .map((image) => {
          const record = image as BlockContent;
          return typeof record === "string" ? record : str(record, "url");
        })
        .filter((line) => line !== "");
      return { images: lines.join("\n") };
    },
    contentFrom: (comp) => {
      const lines = attrOf(comp, "images")
        .split("\n")
        .map((line) => line.trim())
        .filter((line) => line !== "");
      return { images: lines.map((url) => ({ url })) };
    },
  },
  {
    type: "form",
    componentType: "sb-form",
    label: "Form",
    category: "Blocks",
    traits: [
      textTrait("action", "Submit URL"),
      textTrait("submit_label", "Submit button label"),
      areaTrait(
        "fields",
        "Fields (JSON)",
        '[{"name": "email", "label": "Email", "type": "email", "required": true}]',
      ),
    ],
    defaultContent: () => ({ action: "", submit_label: "Submit", fields: [] }),
    inner: (content) => {
      const fields = Array.isArray(content.fields) ? content.fields : [];
      const rows = fields.map((field) => {
        const record = field as BlockContent;
        const label = str(record, "label") || str(record, "name");
        return `<div>${escapeHtml(label)}</div>`;
      });
      const submit = str(content, "submit_label") || "Submit";
      return [
        inert("div", {
          content: `${rows.join("")}<div><strong>${escapeHtml(submit)}</strong></div>`,
        }),
      ];
    },
    traitAttributes: (content) => ({
      action: str(content, "action"),
      submit_label: str(content, "submit_label") || "Submit",
      fields: JSON.stringify(content.fields ?? [], null, 2),
    }),
    contentFrom: (comp) =>
      compact({
        action: attrOf(comp, "action"),
        submit_label: attrOf(comp, "submit_label"),
        fields: parseJsonField(attrOf(comp, "fields"), "Fields"),
      }),
  },
  {
    type: "custom",
    componentType: "sb-custom",
    label: "Custom HTML",
    category: "Blocks",
    traits: [areaTrait("html", "HTML", "<p>Custom markup.</p>")],
    defaultContent: () => ({ html: "<p>Custom markup.</p>" }),
    inner: (content) => [
      inert("div", { content: str(content, "html") }),
    ],
    traitAttributes: (content) => ({ html: str(content, "html") }),
    contentFrom: (comp) => ({ html: attrOf(comp, "html") }),
  },
  {
    type: "pricing_table",
    componentType: "sb-pricing-table",
    label: "Pricing table",
    category: "Blocks",
    traits: [
      textTrait("pricing_table_id", "Pricing table ID"),
      areaTrait("plans", "Plans (JSON)", '[{"name": "Basic", "price": "$9", "features": ["One site"]}]'),
    ],
    defaultContent: () => ({ pricing_table_id: "", plans: [] }),
    inner: (content) => {
      const plans = Array.isArray(content.plans) ? content.plans : [];
      const cards = plans.map((plan) => {
        const record = plan as BlockContent;
        const name = str(record, "name");
        const price = str(record, "price");
        return `<div><strong>${escapeHtml(name)}</strong> ${escapeHtml(price)}</div>`;
      });
      const tableId = str(content, "pricing_table_id");
      return [
        inert("div", {
          content:
            cards.join("") ||
            (tableId ? `Pricing table: ${escapeHtml(tableId)}` : "Pricing table (no plans yet)."),
        }),
      ];
    },
    traitAttributes: (content) => ({
      pricing_table_id: str(content, "pricing_table_id"),
      plans: JSON.stringify(content.plans ?? [], null, 2),
    }),
    contentFrom: (comp) =>
      compact({
        pricing_table_id: attrOf(comp, "pricing_table_id"),
        plans: parseJsonField(attrOf(comp, "plans"), "Plans"),
      }),
  },
  {
    type: "marketplace_storefront",
    componentType: "sb-marketplace-storefront",
    label: "Marketplace storefront",
    category: "Blocks",
    traits: [textTrait("vendor_slug", "Vendor slug")],
    defaultContent: () => ({ vendor_slug: "" }),
    inner: (content) => {
      const slug = str(content, "vendor_slug");
      return [
        inert("div", {
          content: slug ? `Storefront: ${escapeHtml(slug)}` : "Marketplace storefront (no vendor yet).",
        }),
      ];
    },
    traitAttributes: (content) => ({ vendor_slug: str(content, "vendor_slug") }),
    contentFrom: (comp) => compact({ vendor_slug: attrOf(comp, "vendor_slug") }),
  },
];

/** Resolve a server block type to its definition. */
function defForType(serverType: string): BlockDef | null {
  const found = BUILTIN_DEFS.find((def) => def.type === serverType);
  return found ?? null;
}

/** The GrapeJS component type for a server block type. */
function componentTypeFor(serverType: string): string {
  return `sb-${serverType}`;
}

/** Rebuild a wrapper's presentation children from its traits. */
function refreshBlockInner(comp: Component): void {
  const serverType = String(comp.getAttributes()["data-block-type"] ?? "");
  const def = defForType(serverType);
  if (!def || serverType === "text") {
    // Text blocks own their content through the in-canvas editor.
    return;
  }
  // Rebuild from the serialized content (not the raw attributes):
  // gallery/form/plans store structured values, not trait strings.
  // A JSON error leaves the preview as-is; the save surfaces it.
  let content: BlockContent;
  try {
    content = def.contentFrom(comp);
  } catch {
    return;
  }
  comp.components(def.inner(content));
}

/** Component definition for one block (palette drag or page load). */
function blockComponent(
  serverType: string,
  blockId: string,
  content: BlockContent,
): ComponentDefinition {
  const def = defForType(serverType);
  const componentType = componentTypeFor(serverType);
  let inner: ComponentDefinition[];
  let traits: Record<string, string> = {};
  if (def) {
    inner = def.inner(content);
    traits = def.traitAttributes(content);
  } else {
    inner = [inert("div", { content: `Unknown block type: ${escapeHtml(serverType)}` })];
  }
  return {
    type: componentType,
    attributes: {
      class: "sb-block",
      "data-block-id": blockId,
      "data-block-type": serverType,
      ...traits,
    },
    components: inner,
  } as ComponentDefinition;
}

// Editor state

let editor: Editor | null = null;
let currentPage: SiteBuilderPage | null = null;
let pageList: HTMLElement | null = null;
let editorPanel: HTMLElement | null = null;
let editorMeta: HTMLElement | null = null;
let sectionList: HTMLElement | null = null;

function currentPageUrl(): string | null {
  return currentPage ? `${API}/${encodeURIComponent(currentPage.id)}` : null;
}

/** Serialize the canvas to blocks, in canvas order. */
function canvasBlocks(): { serverType: string; blockId: string; content: BlockContent }[] {
  if (!editor) {
    return [];
  }
  const out: { serverType: string; blockId: string; content: BlockContent }[] = [];
  for (const comp of editor.getComponents().toArray()) {
    const serverType = String(comp.getAttributes()["data-block-type"] ?? "");
    const blockId = String(comp.getAttributes()["data-block-id"] ?? "");
    if (!serverType) {
      continue;
    }
    const def = defForType(serverType);
    if (!def) {
      continue;
    }
    out.push({ serverType, blockId, content: def.contentFrom(comp) });
  }
  return out;
}

async function saveBlocks(): Promise<void> {
  const pageUrl = currentPageUrl();
  if (!pageUrl || !currentPage) {
    showStatus("Select a page first.", "error");
    return;
  }
  let blocks: { serverType: string; blockId: string; content: BlockContent }[];
  try {
    blocks = canvasBlocks();
  } catch (error) {
    showStatus(error instanceof Error ? error.message : "Could not read the canvas.", "error");
    return;
  }
  try {
    const ids: string[] = [];
    for (const block of blocks) {
      if (block.blockId) {
        await request<unknown>(`${pageUrl}/blocks/${encodeURIComponent(block.blockId)}`, "PUT", {
          type: block.serverType,
          content: block.content,
        });
        ids.push(block.blockId);
      } else {
        const created = await request<SiteBuilderBlock>(`${pageUrl}/blocks`, "POST", {
          type: block.serverType,
          content: block.content,
        });
        ids.push(created.id);
      }
    }
    await request<unknown>(`${pageUrl}/blocks/order`, "PUT", { ids });
    showStatus("Blocks saved.", "success");
    await loadPage(currentPage.id);
  } catch (error) {
    showStatus(
      `Could not save the blocks: ${error instanceof Error ? error.message : "unknown error"}`,
      "error",
    );
  }
}

function deleteSelectedBlock(): void {
  if (!editor || !currentPage) {
    return;
  }
  const selected = editor.getSelected();
  if (!selected) {
    showStatus("Select a block on the canvas first.", "error");
    return;
  }
  const blockId = String(selected.getAttributes()["data-block-id"] ?? "");
  const pageUrl = currentPageUrl();
  const remove = (): void => {
    selected.remove();
    showStatus("Block removed from the canvas. Save blocks to keep the change.", "info");
  };
  if (!blockId || !pageUrl) {
    remove();
    return;
  }
  void request<unknown>(`${pageUrl}/blocks/${encodeURIComponent(blockId)}`, "DELETE")
    .then(() => {
      remove();
    })
    .catch((error: Error) => {
      showStatus(`Could not delete the block: ${error.message}`, "error");
    });
}

// GrapeJS setup

function registerTextareaTrait(ed: Editor): void {
  // GrapeJS core ships no multi-line trait; this one renders a
  // <textarea> bound to the trait's attribute, like the built-in
  // text trait. The value commits on change (blur), which is also
  // when the canvas preview rebuilds.
  ed.TraitManager.addType("textarea", {
    createInput() {
      const el = document.createElement("textarea");
      el.rows = 4;
      return el;
    },
    onUpdate({ elInput, component, trait }) {
      const input = elInput as unknown as HTMLTextAreaElement;
      const name = String(trait.get("name") ?? "");
      const value = component.getAttributes()[name];
      input.value = typeof value === "string" ? value : "";
    },
    onEvent({ elInput, component, trait }) {
      const input = elInput as unknown as HTMLTextAreaElement;
      const name = String(trait.get("name") ?? "");
      component.addAttributes({ [name]: input.value });
    },
  });
}

function registerBlockTypes(ed: Editor): void {
  const defs: BlockDef[] = [...BUILTIN_DEFS];
  for (const def of defs) {
    ed.DomComponents.addType(def.componentType, {
      model: {
        defaults: {
          tagName: "div",
          attributes: { class: "sb-block", "data-block-type": def.type },
          traits: def.traits,
          draggable: true,
          droppable: false,
          copyable: true,
          removable: true,
        },
        init() {
          // Traits write attributes; rebuild the preview from them.
          this.on("change:attributes", () => {
            refreshBlockInner(this);
          });
        },
      },
    });
  }
}

function buildPalette(ed: Editor): void {
  for (const def of BUILTIN_DEFS) {
    ed.BlockManager.add(`sb-${def.type}`, {
      label: def.label,
      category: "Blocks",
      content: blockComponent(def.type, "", def.defaultContent()),
    });
  }
}

function initEditor(): Editor {
  const ed = grapesjs.init({
    container: "#gjs",
    fromElement: false,
    height: "600px",
    width: "auto",
    storageManager: false,
  });
  registerTextareaTrait(ed);
  registerBlockTypes(ed);
  return ed;
}

// Pages and sections (vanilla DOM, same IDs as the template)

function loadPages(): void {
  if (!pageList) {
    return;
  }
  const list = pageList;
  void request<unknown>(`${API}?per_page=50`)
    .then((data: unknown) => {
      const pages = itemsFromEnvelope<SiteBuilderPage>(data);
      if (!pages.length) {
        list.innerHTML = '<tr><td colspan="4">No pages yet. Create the first one above.</td></tr>';
        return;
      }
      list.innerHTML = pages
        .map(
          (page: SiteBuilderPage): string =>
            `<tr><td>${escapeHtml(page.title)}</td><td>${escapeHtml(page.slug)}</td><td>${escapeHtml(page.status)}</td><td>` +
            `<button class="button button-small" type="button" data-sb-edit-page="${escapeHtml(page.id)}">Edit</button> ` +
            `<button class="button button-small button-danger" type="button" data-sb-delete-page="${escapeHtml(page.id)}">Delete</button>` +
            `</td></tr>`,
        )
        .join("");
    })
    .catch((error: Error) => {
      list.innerHTML = `<tr><td colspan="4">Could not load pages: ${escapeHtml(error.message)}</td></tr>`;
    });
}

function renderSections(page: SiteBuilderPage): void {
  if (!sectionList) {
    return;
  }
  const sections = (page.sections || []).slice().sort((a, b) => a.order - b.order);
  if (!sections.length) {
    sectionList.innerHTML = '<tr><td colspan="4">No sections yet. Add the first one below.</td></tr>';
    return;
  }
  sectionList.innerHTML = sections
    .map(
      (section: SiteBuilderSection, index: number): string =>
        `<tr><td>${section.order}</td><td>${escapeHtml(section.name)}</td><td>${escapeHtml(section.layout || "")}</td><td>` +
        `<button class="button button-small" type="button" data-sb-edit-section="${escapeHtml(section.id)}">Edit</button> ` +
        `<button class="button button-small" type="button" data-sb-move-section="${escapeHtml(section.id)}" data-sb-direction="-1" ${index === 0 ? "disabled" : ""}>Up</button> ` +
        `<button class="button button-small" type="button" data-sb-move-section="${escapeHtml(section.id)}" data-sb-direction="1" ${index === sections.length - 1 ? "disabled" : ""}>Down</button> ` +
        `<button class="button button-small button-danger" type="button" data-sb-delete-section="${escapeHtml(section.id)}">Delete</button>` +
        `</td></tr>`,
    )
    .join("");
}

async function loadPage(id: string): Promise<void> {
  try {
    const page = await request<SiteBuilderPage>(`${API}/${encodeURIComponent(id)}`);
    currentPage = page;
    if (editorPanel) {
      editorPanel.hidden = false;
    }
    if (editorMeta) {
      editorMeta.textContent = `${page.title} - slug: ${page.slug} - status: ${page.status} - theme: ${page.theme_id}`;
    }
    if (!editor) {
      editor = initEditor();
      buildPalette(editor);
    }
    if (!editor) {
      return;
    }
    const blocks = (page.blocks || []).slice().sort((a, b) => a.order - b.order);
    editor.setComponents(
      blocks.map((block) => blockComponent(block.type, block.id, block.content)),
    );
    renderSections(page);
  } catch (error) {
    showStatus(
      `Could not load the page: ${error instanceof Error ? error.message : "unknown error"}`,
      "error",
    );
  }
}

function refreshCurrent(): void {
  if (currentPage) {
    void loadPage(currentPage.id);
  }
}

// Boot

document.addEventListener("DOMContentLoaded", () => {
  statusBox = byId("sb-status");
  pageList = byId("sb-page-list");
  editorPanel = byId("sb-editor");
  editorMeta = byId("sb-editor-meta");
  sectionList = byId("sb-section-list");
  if (!pageList) {
    return;
  }
  loadPages();

  const newPageForm = document.getElementById("sb-new-page") as HTMLFormElement | null;
  if (newPageForm) {
    newPageForm.addEventListener("submit", (event: SubmitEvent) => {
      event.preventDefault();
      const titleInput = byId("sb-page-title") as HTMLInputElement | null;
      const slugInput = byId("sb-page-slug") as HTMLInputElement | null;
      const themeInput = byId("sb-page-theme") as HTMLInputElement | null;
      if (!titleInput || !themeInput) {
        showStatus("Title and theme are both required.", "error");
        return;
      }
      const title = titleInput.value.trim();
      const slug = slugInput ? slugInput.value.trim() : "";
      const themeId = themeInput.value.trim();
      if (!title || !themeId) {
        showStatus("Title and theme are both required.", "error");
        return;
      }
      const payload: { title: string; theme_id: string; slug?: string } = {
        title,
        theme_id: themeId,
      };
      if (slug) {
        payload.slug = slug;
      }
      void request<SiteBuilderPage>(API, "POST", payload)
        .then((page) => {
          showStatus(`Page "${page.title}" created.`, "success");
          newPageForm.reset();
          themeInput.value = "default";
          loadPages();
          void loadPage(page.id);
        })
        .catch((error: Error) => {
          showStatus(`Could not create the page: ${error.message}`, "error");
        });
    });
  }

  const saveButton = byId("sb-save-blocks");
  if (saveButton) {
    saveButton.addEventListener("click", () => {
      void saveBlocks();
    });
  }

  const deleteBlockButton = byId("sb-delete-block");
  if (deleteBlockButton) {
    deleteBlockButton.addEventListener("click", () => {
      deleteSelectedBlock();
    });
  }

  // Delegated actions for the page table and the sections table.
  document.addEventListener("click", (event: MouseEvent) => {
    const target: Element | null =
      event.target instanceof Element
        ? event.target.closest(
            "[data-sb-edit-page],[data-sb-delete-page],[data-sb-edit-section],[data-sb-delete-section],[data-sb-move-section]",
          )
        : null;
    if (!target) {
      return;
    }
    const editPageId = target.getAttribute("data-sb-edit-page");
    if (editPageId !== null) {
      void loadPage(editPageId);
      return;
    }
    const deletePageId = target.getAttribute("data-sb-delete-page");
    if (deletePageId !== null) {
      if (!window.confirm("Delete this page and all its blocks and sections?")) {
        return;
      }
      void request<unknown>(`${API}/${encodeURIComponent(deletePageId)}`, "DELETE")
        .then(() => {
          showStatus("Page deleted.", "success");
          if (currentPage && currentPage.id === deletePageId) {
            currentPage = null;
            if (editorPanel) {
              editorPanel.hidden = true;
            }
          }
          loadPages();
        })
        .catch((error: Error) => {
          showStatus(`Could not delete the page: ${error.message}`, "error");
        });
      return;
    }
    if (!currentPage) {
      return;
    }
    const pageUrl = `${API}/${encodeURIComponent(currentPage.id)}`;
    const editSectionId = target.getAttribute("data-sb-edit-section");
    if (editSectionId !== null) {
      const section = (currentPage.sections || []).find((item) => item.id === editSectionId);
      if (!section) {
        return;
      }
      const nameField = byId("sb-section-name") as HTMLInputElement | null;
      const layoutField = byId("sb-section-layout") as HTMLInputElement | null;
      if (nameField) {
        nameField.value = section.name;
        nameField.focus();
      }
      if (layoutField) {
        layoutField.value = section.layout || "";
      }
      // Arm the form: its submit saves an update instead of a create.
      const form = document.getElementById("sb-new-section") as HTMLFormElement | null;
      if (form) {
        form.dataset.editingId = editSectionId;
      }
      showStatus(`Editing section "${section.name}". Save it with Add section.`, "info");
      return;
    }
    const deleteSectionId = target.getAttribute("data-sb-delete-section");
    if (deleteSectionId !== null) {
      void request<unknown>(`${pageUrl}/sections/${encodeURIComponent(deleteSectionId)}`, "DELETE")
        .then(() => {
          showStatus("Section deleted.", "success");
          refreshCurrent();
        })
        .catch((error: Error) => {
          showStatus(`Could not delete the section: ${error.message}`, "error");
        });
      return;
    }
    const moveSectionId = target.getAttribute("data-sb-move-section");
    if (moveSectionId !== null) {
      const sections = (currentPage.sections || []).slice().sort((a, b) => a.order - b.order);
      const index = sections.findIndex((item) => item.id === moveSectionId);
      const next = index + Number(target.getAttribute("data-sb-direction"));
      if (index < 0 || next < 0 || next >= sections.length) {
        return;
      }
      const moved = sections.splice(index, 1)[0];
      sections.splice(next, 0, moved);
      void request<unknown>(`${pageUrl}/sections/order`, "PUT", {
        ids: sections.map((item) => item.id),
      })
        .then(refreshCurrent)
        .catch((error: Error) => {
          showStatus(`Could not reorder sections: ${error.message}`, "error");
        });
    }
  });

  const newSectionForm = document.getElementById("sb-new-section") as HTMLFormElement | null;
  if (newSectionForm) {
    newSectionForm.addEventListener("submit", (event: SubmitEvent) => {
      event.preventDefault();
      if (!currentPage) {
        showStatus("Select a page first.", "error");
        return;
      }
      const nameField = byId("sb-section-name") as HTMLInputElement | null;
      const layoutField = byId("sb-section-layout") as HTMLInputElement | null;
      if (!nameField || !layoutField) {
        return;
      }
      const name = nameField.value.trim();
      const layout = layoutField.value.trim();
      if (!name) {
        showStatus("Section name is required.", "error");
        return;
      }
      const payload: { name: string; layout?: string } = { name };
      if (layout) {
        payload.layout = layout;
      }
      const editingId = newSectionForm.dataset.editingId;
      const done = (): void => {
        showStatus(editingId ? "Section updated." : "Section added.", "success");
        delete newSectionForm.dataset.editingId;
        newSectionForm.reset();
        refreshCurrent();
      };
      const failed = (error: Error): void => {
        showStatus(`Could not save the section: ${error.message}`, "error");
      };
      if (editingId) {
        void request<unknown>(
          `${API}/${encodeURIComponent(currentPage.id)}/sections/${encodeURIComponent(editingId)}`,
          "PUT",
          payload,
        ).then(done, failed);
      } else {
        void request<unknown>(
          `${API}/${encodeURIComponent(currentPage.id)}/sections`,
          "POST",
          payload,
        ).then(done, failed);
      }
    });
  }

  // Section edit state: the Edit button arms the form (see the
  // delegated handler above); the submit reads it back.
  function pageAction(action: "publish" | "unpublish", verb: string): void {
    const button = byId(`sb-${action}`);
    if (!button) {
      return;
    }
    button.addEventListener("click", () => {
      if (!currentPage) {
        return;
      }
      void request<SiteBuilderPage>(
        `${API}/${encodeURIComponent(currentPage.id)}/${action}`,
        "POST",
      )
        .then((page) => {
          currentPage = page;
          showStatus(`Page ${verb}.`, "success");
          loadPages();
          if (editorMeta) {
            editorMeta.textContent = `${page.title} - slug: ${page.slug} - status: ${page.status} - theme: ${page.theme_id}`;
          }
        })
        .catch((error: Error) => {
          showStatus(`Could not ${verb} the page: ${error.message}`, "error");
        });
    });
  }
  pageAction("publish", "published");
  pageAction("unpublish", "unpublished");

  const previewButton = byId("sb-preview");
  if (previewButton) {
    previewButton.addEventListener("click", () => {
      if (!currentPage) {
        return;
      }
      void request<RenderResult>(`${API}/${encodeURIComponent(currentPage.id)}/render`)
        .then((data) => {
          const win = window.open("", "_blank");
          if (!win) {
            showStatus("The preview was blocked by a pop-up blocker.", "warning");
            return;
          }
          win.document.open();
          win.document.write(data.html || "");
          win.document.close();
        })
        .catch((error: Error) => {
          showStatus(`Could not preview the page: ${error.message}`, "error");
        });
    });
  }

  const deletePageButton = byId("sb-delete-page");
  if (deletePageButton) {
    deletePageButton.addEventListener("click", () => {
      if (!currentPage) {
        return;
      }
      if (!window.confirm("Delete this page and all its blocks and sections?")) {
        return;
      }
      const id = currentPage.id;
      void request<unknown>(`${API}/${encodeURIComponent(id)}`, "DELETE")
        .then(() => {
          showStatus("Page deleted.", "success");
          currentPage = null;
          if (editorPanel) {
            editorPanel.hidden = true;
          }
          loadPages();
        })
        .catch((error: Error) => {
          showStatus(`Could not delete the page: ${error.message}`, "error");
        });
    });
  }

  // Deep link: /admin/site-builder/<page-id> opens that page's editor.
  const parts: string[] = window.location.pathname.split("/").filter(Boolean);
  if (parts.length >= 3 && parts[0] === "admin" && parts[1] === "site-builder") {
    void loadPage(decodeURIComponent(parts.slice(2).join("/")));
  }
});
