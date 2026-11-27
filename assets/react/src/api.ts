// API client for the React track.
//
// Mirrors the proven contract in assets/js/dashboard.ts and
// assets/js/auth.ts: same endpoints, same standard envelope
// (success `{"data": ...}`, error `{"error": {"message", "code",
// "metadata"}}`), same cookie session (`credentials: "include"`),
// same CSRF flow (cached token from GET /api/v1/security/csrf-token,
// retry once on a 403 that mentions CSRF).
//
// Every endpoint used here is registered in the Rust route tables;
// see the per-function comments for the source. No endpoint is
// invented: when the server does not answer, callers surface an
// honest error instead of fabricated data.

export class ApiError extends Error {
  readonly status: number;
  readonly code?: string;

  constructor(status: number, message: string, code?: string) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.code = code;
  }
}

interface ApiEnvelope<T> {
  data?: T;
  error?: {
    message?: string;
    code?: string;
    metadata?: unknown;
  };
}

interface PageEnvelope<T> {
  items: T[];
  pagination: {
    page: number;
    per_page: number;
    total: number;
    total_pages: number;
  };
}

/** Plain array or a paginated `{"items": [...]}` payload. */
export function itemsFromData<T>(data: unknown): T[] {
  if (Array.isArray(data)) return data as T[];
  if (data !== null && typeof data === "object" && Array.isArray((data as { items?: unknown }).items)) {
    return (data as { items: T[] }).items;
  }
  return [];
}

function errorFromBody(status: number, body: unknown): ApiError {
  const envelope = body as ApiEnvelope<unknown> | null;
  const message = envelope?.error?.message;
  const code = envelope?.error?.code;
  return new ApiError(
    status,
    message || `Request failed with HTTP ${status}`,
    code,
  );
}

async function readBody(response: Response): Promise<unknown> {
  const text = await response.text();
  if (!text) return null;
  try {
    return JSON.parse(text) as unknown;
  } catch {
    return null;
  }
}

/* CSRF */
// Cookie-authenticated mutations require a valid `X-CSRF-Token`
// header. The token is bound to the session, fetched from
// GET /api/v1/security/csrf-token (src/modules/security/module.rs:275),
// and cached in memory; a failed fetch resolves to null and the
// request goes out without the header, letting the server decide.

let csrfToken: string | null = null;
let csrfTokenRequest: Promise<string | null> | null = null;

function fetchCsrfToken(): Promise<string | null> {
  if (csrfToken) return Promise.resolve(csrfToken);
  if (!csrfTokenRequest) {
    csrfTokenRequest = window
      .fetch("/api/v1/security/csrf-token", {
        headers: { Accept: "application/json" },
        credentials: "include",
      })
      .then((response) => (response.ok ? readBody(response) : null))
      .then((body) => {
        const envelope = body as { data?: { csrf_token?: unknown } } | null;
        const token = envelope?.data?.csrf_token;
        csrfToken = typeof token === "string" && token ? token : null;
        return csrfToken;
      })
      .catch(() => null);
    // A failed fetch must not poison the cache: the next mutation
    // retries the token endpoint.
    void csrfTokenRequest.then(() => {
      if (!csrfToken) csrfTokenRequest = null;
    });
  }
  return csrfTokenRequest;
}

async function isCsrfForbidden(response: Response): Promise<boolean> {
  if (response.status !== 403) return false;
  try {
    const body = await response.clone().json() as ApiEnvelope<unknown>;
    const message = body?.error?.message;
    return typeof message === "string" && message.includes("CSRF");
  } catch {
    return false;
  }
}

/* Core request */

interface RequestOptions {
  method?: string;
  body?: unknown;
  /** Attach the CSRF header (mutations only). Retries once with a fresh token. */
  withCsrf?: boolean;
}

async function request<T>(url: string, options: RequestOptions = {}): Promise<T> {
  const method = options.method ?? "GET";
  const headers: Record<string, string> = { Accept: "application/json" };
  if (options.body !== undefined) headers["Content-Type"] = "application/json";

  const doFetch = async (token: string | null): Promise<Response> => {
    const requestHeaders = { ...headers };
    if (token) requestHeaders["X-CSRF-Token"] = token;
    return window.fetch(url, {
      method,
      headers: requestHeaders,
      credentials: "include",
      body: options.body === undefined ? undefined : JSON.stringify(options.body),
    });
  };

  let response: Response;
  if (options.withCsrf) {
    const token = await fetchCsrfToken();
    response = await doFetch(token);
    if (token && (await isCsrfForbidden(response))) {
      csrfToken = null;
      csrfTokenRequest = null;
      const fresh = await fetchCsrfToken();
      if (fresh) response = await doFetch(fresh);
    }
  } else {
    response = await doFetch(null);
  }

  const body = await readBody(response);
  if (!response.ok) throw errorFromBody(response.status, body);
  return (body as ApiEnvelope<T>).data as T;
}

export function apiGet<T>(url: string): Promise<T> {
  return request<T>(url);
}

export function apiPost<T>(url: string, body?: unknown): Promise<T> {
  return request<T>(url, { method: "POST", body: body ?? {}, withCsrf: true });
}

export function apiPut<T>(url: string, body?: unknown): Promise<T> {
  return request<T>(url, { method: "PUT", body: body ?? {}, withCsrf: true });
}

/* Content: tags + custom post types  */
// Shapes follow the Rust serializers in src/modules/content/types.rs.
// The generic custom-entry screens are driven entirely by the
// definition: no per-type client code exists.

export type ContentStatus = "draft" | "published";

export interface ContentTag {
  id: string;
  tenant_id: string;
  name: string;
  slug: string;
  description?: string | null;
  created_at: string;
  updated_at: string;
}

export type CustomFieldType =
  | "text"
  | "textarea"
  | "richtext"
  | "number"
  | "date"
  | "datetime"
  | "select"
  | "multiselect"
  | "checkbox"
  | "media"
  | "relation";

export interface CustomFieldDef {
  key: string;
  label: string;
  type: CustomFieldType;
  required?: boolean;
  default?: unknown;
  options?: string[];
}

export interface PostTypeDefinition {
  id: string;
  tenant_id: string;
  slug: string;
  singular_label: string;
  plural_label: string;
  icon: string;
  description?: string | null;
  supports: string[];
  fields: CustomFieldDef[];
  visibility: "public" | "private";
  has_archive: boolean;
  show_in_menu: boolean;
  menu_position: number;
  rest_exposed: boolean;
  enabled: boolean;
  created_at: string;
  updated_at: string;
}

export interface CustomEntry {
  id: string;
  tenant_id: string;
  type: string;
  title: string;
  slug: string;
  content: string;
  excerpt: string;
  status: ContentStatus;
  category_id?: string | null;
  author_id?: string | null;
  thumbnail?: string | null;
  fields: Record<string, unknown>;
  tag_ids: string[];
  created_at: string;
  updated_at: string;
  published_at?: string | null;
}

export function fetchContentTags(search?: string): Promise<PageEnvelope<ContentTag>> {
  const query = search ? `?search=${encodeURIComponent(search)}` : "";
  return apiGet<PageEnvelope<ContentTag>>(`/api/v1/content/tags${query}`);
}

export function createContentTag(body: {
  name: string;
  slug?: string;
  description?: string;
}): Promise<ContentTag> {
  return apiPost<ContentTag>("/api/v1/content/tags", body);
}

export function deleteContentTag(id: string): Promise<{ id: string; deleted: boolean }> {
  return apiDelete<{ id: string; deleted: boolean }>(`/api/v1/content/tags/${id}`);
}

export function fetchPostTypes(): Promise<PageEnvelope<PostTypeDefinition>> {
  return apiGet<PageEnvelope<PostTypeDefinition>>("/api/v1/content/post-types?per_page=100");
}

export function fetchEnabledPostTypes(): Promise<PageEnvelope<PostTypeDefinition>> {
  return apiGet<PageEnvelope<PostTypeDefinition>>(
    "/api/v1/content/post-types?per_page=100&filters[enabled]=true",
  );
}

export function createPostType(body: Record<string, unknown>): Promise<PostTypeDefinition> {
  return apiPost<PostTypeDefinition>("/api/v1/content/post-types", body);
}

export function updatePostType(
  id: string,
  body: Record<string, unknown>,
): Promise<PostTypeDefinition> {
  return apiPut<PostTypeDefinition>(`/api/v1/content/post-types/${id}`, body);
}

export function deletePostType(id: string): Promise<PostTypeDefinition> {
  return apiDelete<PostTypeDefinition>(`/api/v1/content/post-types/${id}`);
}

export function enablePostType(id: string): Promise<PostTypeDefinition> {
  return apiPost<PostTypeDefinition>(`/api/v1/content/post-types/${id}/enable`);
}

export function disablePostType(id: string): Promise<PostTypeDefinition> {
  return apiPost<PostTypeDefinition>(`/api/v1/content/post-types/${id}/disable`);
}

export function fetchCustomEntries(
  typeSlug: string,
  params?: { status?: string; search?: string; page?: number },
): Promise<PageEnvelope<CustomEntry>> {
  const query = new URLSearchParams();
  if (params?.status) query.set("filters[status]", params.status);
  if (params?.search) query.set("search", params.search);
  if (params?.page) query.set("page", String(params.page));
  const suffix = query.toString();
  return apiGet<PageEnvelope<CustomEntry>>(
    `/api/v1/content/custom/${encodeURIComponent(typeSlug)}${suffix ? `?${suffix}` : ""}`,
  );
}

export function fetchCustomEntryBySlug(
  typeSlug: string,
  slug: string,
): Promise<CustomEntry> {
  return apiGet<CustomEntry>(
    `/api/v1/content/custom/${encodeURIComponent(typeSlug)}/slug/${encodeURIComponent(slug)}`,
  );
}

export function createCustomEntry(
  typeSlug: string,
  body: Record<string, unknown>,
): Promise<CustomEntry> {
  return apiPost<CustomEntry>(`/api/v1/content/custom/${encodeURIComponent(typeSlug)}`, body);
}

export function updateCustomEntry(
  typeSlug: string,
  id: string,
  body: Record<string, unknown>,
): Promise<CustomEntry> {
  return apiPut<CustomEntry>(
    `/api/v1/content/custom/${encodeURIComponent(typeSlug)}/${id}`,
    body,
  );
}

export function deleteCustomEntry(
  typeSlug: string,
  id: string,
): Promise<{ id: string; deleted: boolean }> {
  return apiDelete<{ id: string; deleted: boolean }>(
    `/api/v1/content/custom/${encodeURIComponent(typeSlug)}/${id}`,
  );
}

export function fetchCustomEntryTags(typeSlug: string, id: string): Promise<ContentTag[]> {
  return apiGet<ContentTag[]>(
    `/api/v1/content/custom/${encodeURIComponent(typeSlug)}/entries/${id}/tags`,
  );
}

export function assignCustomEntryTags(
  typeSlug: string,
  id: string,
  tagIds: string[],
): Promise<ContentTag[]> {
  return apiPost<ContentTag[]>(
    `/api/v1/content/custom/${encodeURIComponent(typeSlug)}/entries/${id}/tags`,
    { tag_ids: tagIds },
  );
}

export function apiPatch<T>(url: string, body?: unknown): Promise<T> {
  return request<T>(url, { method: "PATCH", body: body ?? {}, withCsrf: true });
}

export function apiDelete<T>(url: string): Promise<T> {
  return request<T>(url, { method: "DELETE", withCsrf: true });
}

/** Cached CSRF token for callers that cannot use `request` (the
 * XHR multi-file upload, which needs upload progress events). */
export function getCsrfToken(): Promise<string | null> {
  return fetchCsrfToken();
}

/* Response shapes */
// Field names follow the Rust serializers; optional fields stay
// optional here because the server omits them when unknown.

export interface RequiredModuleInfo {
  id: string;
  name?: string;
  /** "enabled" | "disabled" | "missing" */
  status: string;
  exists: boolean;
}

export interface DependencyWarning {
  /** "missing_dependency" | "disabled_dependency" */
  code: string;
  message: string;
  required_modules: string[];
  will_auto_activate: string[];
}

export interface LibraryNotice {
  library?: string;
  message?: string;
  [key: string]: unknown;
}

export interface ExtensionInfo {
  id: string;
  name: string;
  version: string;
  description: string;
  is_core: boolean;
  dependencies: string[];
  requires: RequiredModuleInfo[];
  requires_libraries: string[];
  requires_external_libraries: unknown[];
  library_notices: LibraryNotice[];
  missing_required: string[];
  warnings: DependencyWarning[];
  enabled: boolean;
}

export interface SystemStatus {
  status?: string;
  uptime_seconds?: number;
  version?: string;
  cpu_usage_percent?: number;
  cpu_count?: number;
  workers?: number;
  total_memory_bytes?: number;
  used_memory_bytes?: number;
}

export interface TaskItem {
  id?: string;
  status?: string;
  title?: string;
  priority?: string;
  due_date?: string;
  overdue?: boolean;
  project_name?: string;
}

export interface NotificationItem {
  id: string;
  title?: string;
  body?: string;
  kind?: string;
  read?: boolean;
  created_at?: string;
}

export interface UserOverview {
  db_available?: boolean;
  counts?: {
    licenses?: number;
    active_licenses?: number;
    orders?: number;
    entitlements?: number;
    downloads?: number;
  };
  recent_licenses?: unknown[];
  recent_orders?: unknown[];
}

export interface AdminOverview {
  tenant_scope?: string;
  system?: {
    uptime_seconds?: number;
    modules_total?: number;
    modules?: unknown[];
    dependencies_valid?: boolean;
    dependency_issues?: unknown[];
    db_available?: boolean;
    paused?: boolean;
    maintenance?: boolean;
  };
  counts?: {
    users?: number;
    roles?: number;
    licenses?: number;
    orders?: number;
  };
}

export interface AdminModulesData {
  modules?: ExtensionInfo[];
  dependencies_valid?: boolean;
}

export interface AdminUserItem {
  id?: string;
  name?: string;
  email?: string;
  is_active?: boolean;
  created_at?: string;
  roles?: string[];
}

/* Endpoint wrappers */
// Route registrations (src/modules/*/module.rs) are cited per call.

/** GET /api/v1/extensions - every page (per_page clamps to 100). */
export async function fetchAllExtensions(): Promise<ExtensionInfo[]> {
  const all: ExtensionInfo[] = [];
  let page = 1;
  let totalPages = 1;
  do {
    const data = await apiGet<PageEnvelope<ExtensionInfo>>(
      `/api/v1/extensions?page=${page}&per_page=100`,
    );
    all.push(...itemsFromData<ExtensionInfo>(data));
    const pagination = (data as unknown as PageEnvelope<ExtensionInfo> | null)?.pagination;
    totalPages = pagination?.total_pages ?? 1;
    page += 1;
    // Guard against a server that never advances the page count.
    if (page > 50) break;
  } while (page <= totalPages);
  return all;
}

/** GET /api/v1/extensions/{id} (src/modules/extensions/module.rs:91). */
export function fetchExtension(id: string): Promise<ExtensionInfo> {
  return apiGet<ExtensionInfo>(`/api/v1/extensions/${encodeURIComponent(id)}`);
}

/** POST /api/v1/extensions/{id}/enable (module.rs:95). Requires modules:manage. */
export function enableExtension(id: string): Promise<ExtensionInfo> {
  return apiPost<{ module: ExtensionInfo }>(
    `/api/v1/extensions/${encodeURIComponent(id)}/enable`,
  ).then((data) => data.module);
}

/** POST /api/v1/extensions/{id}/disable (module.rs:96). Requires modules:manage. */
export function disableExtension(id: string): Promise<ExtensionInfo> {
  return apiPost<{ module: ExtensionInfo }>(
    `/api/v1/extensions/${encodeURIComponent(id)}/disable`,
  ).then((data) => data.module);
}

/** GET /api/v1/service-monitor/status (src/modules/service_monitor/module.rs:50). */
export function fetchSystemStatus(): Promise<SystemStatus> {
  return apiGet<SystemStatus>("/api/v1/service-monitor/status");
}

/** GET /api/v1/tasks (src/modules/tasks/module.rs:58). */
export function fetchTasks(query = "per_page=8&sort=due_date&order=asc"): Promise<TaskItem[]> {
  return apiGet<unknown>(`/api/v1/tasks?${query}`).then(itemsFromData<TaskItem>);
}

/** GET /api/v1/notifications (src/modules/notifications/module.rs:185). */
export function fetchNotifications(): Promise<NotificationItem[]> {
  return apiGet<unknown>("/api/v1/notifications?per_page=8").then(
    itemsFromData<NotificationItem>,
  );
}

/** POST /api/v1/notifications/{id}/read (module.rs:208). */
export function markNotificationRead(id: string): Promise<unknown> {
  return apiPost(`/api/v1/notifications/${encodeURIComponent(id)}/read`);
}

/** GET /api/v1/dashboards/user/overview (src/modules/dashboards/module.rs:377). */
export function fetchUserOverview(): Promise<UserOverview> {
  return apiGet<UserOverview>("/api/v1/dashboards/user/overview");
}

/** GET /api/v1/dashboards/admin/overview (module.rs:369). */
export function fetchAdminOverview(): Promise<AdminOverview> {
  return apiGet<AdminOverview>("/api/v1/dashboards/admin/overview");
}

/** GET /api/v1/dashboards/admin/modules (module.rs:370). */
export function fetchAdminModules(): Promise<AdminModulesData> {
  return apiGet<AdminModulesData>("/api/v1/dashboards/admin/modules");
}

/** GET /api/v1/dashboards/admin/users (module.rs:371). */
export function fetchAdminUsers(): Promise<AdminUserItem[]> {
  return apiGet<unknown>("/api/v1/dashboards/admin/users?per_page=20").then(
    itemsFromData<AdminUserItem>,
  );
}

/* Documents / media library (src/modules/documents/module.rs) */
// Field names follow the Rust serializers (`DocumentRecord`,
// `DocumentFolder`, `DocumentFolderTree` in
// src/modules/documents/types.rs). `folder_id` is "" at the library
// root; `type` is the server-side coarse classifier (`image` |
// `video` | `audio` | `document` | `archive` | `other`).

export interface DocumentItem {
  id: string;
  name: string;
  title?: string;
  alt_text?: string | null;
  caption?: string | null;
  description?: string | null;
  custom_metadata?: Record<string, unknown>;
  module_id: string;
  company_id: string;
  owner_id: string;
  scope: string;
  mime: string;
  size: number;
  checksum?: string | null;
  storage_key: string;
  current_version: number;
  version_count: number;
  scan_status: string;
  scan_threat?: string | null;
  visibility: string;
  folder_id?: string;
  created_at: string;
  updated_at: string;
}

export interface DocumentFolder {
  id: string;
  name: string;
  parent_id?: string;
  company_id: string;
  owner_id: string;
  created_at: string;
  updated_at: string;
}

export interface DocumentFolderTree {
  folder: DocumentFolder;
  document_count: number;
  children: DocumentFolderTree[];
}

export interface PickerItem {
  document_id: string;
  name: string;
  mime: string;
  size: number;
  storage_key: string;
  url: string;
  visibility: string;
  created_at: string;
}

export interface DocumentListParams {
  page?: number;
  per_page?: number;
  search?: string;
  /** Exact-match filters: folder_id, visibility, type, mime, module_id. */
  folder_id?: string;
  visibility?: string;
  type?: string;
  mime?: string;
  module_id?: string;
}

function documentsQuery(params: DocumentListParams): string {
  const query = new URLSearchParams();
  query.set("page", String(params.page ?? 1));
  query.set("per_page", String(params.per_page ?? 50));
  if (params.search) query.set("search", params.search);
  if (params.folder_id !== undefined) query.set("folder_id", params.folder_id);
  if (params.visibility) query.set("visibility", params.visibility);
  if (params.type) query.set("type", params.type);
  if (params.mime) query.set("mime", params.mime);
  if (params.module_id) query.set("module_id", params.module_id);
  return query.toString();
}

/** GET /api/v1/documents - paginated, filtered document list. */
export function fetchDocuments(
  params: DocumentListParams = {},
): Promise<PageEnvelope<DocumentItem>> {
  return apiGet<PageEnvelope<DocumentItem>>(
    `/api/v1/documents?${documentsQuery(params)}`,
  );
}

/** GET /api/v1/documents/folders - the folder tree (nested, with
 * direct document counts). Returned whole, not paginated. */
export function fetchFolderTree(): Promise<DocumentFolderTree[]> {
  return apiGet<DocumentFolderTree[]>("/api/v1/documents/folders");
}

/** POST /api/v1/documents/folders - create a folder. */
export function createFolder(
  name: string,
  parentId = "",
): Promise<DocumentFolder> {
  return apiPost<DocumentFolder>("/api/v1/documents/folders", {
    name,
    parent_id: parentId || null,
  });
}

/** PATCH /api/v1/documents/folders/{id} - rename a folder. */
export function renameFolder(id: string, name: string): Promise<DocumentFolder> {
  return apiPatch<DocumentFolder>(
    `/api/v1/documents/folders/${encodeURIComponent(id)}`,
    { name },
  );
}

/** POST /api/v1/documents/folders/{id}/move - move a folder. */
export function moveFolder(id: string, parentId: string): Promise<DocumentFolder> {
  return apiPost<DocumentFolder>(
    `/api/v1/documents/folders/${encodeURIComponent(id)}/move`,
    { parent_id: parentId },
  );
}

/** DELETE /api/v1/documents/folders/{id} - delete an empty folder.
 * The server refuses (409) while the folder holds files or
 * subfolders; callers confirm first and surface that error. */
export function deleteFolder(id: string): Promise<{ deleted: boolean; id: string }> {
  return apiDelete<{ deleted: boolean; id: string }>(
    `/api/v1/documents/folders/${encodeURIComponent(id)}`,
  );
}

/** POST /api/v1/documents/{id}/move - move a document between
 * folders ("" = library root). */
export function moveDocument(id: string, folderId: string): Promise<DocumentItem> {
  return apiPost<DocumentItem>(
    `/api/v1/documents/${encodeURIComponent(id)}/move`,
    { folder_id: folderId },
  );
}

/** POST /api/v1/documents/{id}/rename - rename a document. */
export function renameDocument(id: string, name: string): Promise<DocumentItem> {
  return apiPost<DocumentItem>(
    `/api/v1/documents/${encodeURIComponent(id)}/rename`,
    { name },
  );
}

/** POST /api/v1/documents/{id}/flip - flip public/private. */
export function flipDocumentVisibility(
  id: string,
  visibility: "public" | "private",
): Promise<DocumentItem> {
  return apiPost<DocumentItem>(
    `/api/v1/documents/${encodeURIComponent(id)}/flip`,
    { visibility },
  );
}

/** DELETE /api/v1/documents/{id} - delete a document and all its
 * versions. Callers confirm first; the server enforces the
 * owner-or-admin rule. */
export function deleteDocument(id: string): Promise<{ deleted: boolean; id: string }> {
  return apiDelete<{ deleted: boolean; id: string }>(
    `/api/v1/documents/${encodeURIComponent(id)}`,
  );
}

/** Public URL for a document: public files serve from
 * /files/public/, private files need a signed URL (issue one via
 * POST /api/v1/documents/{id}/signed-url when the caller may not
 * see the file directly). */
export function documentUrl(document: DocumentItem): string {
  return document.visibility === "public"
    ? `/files/public/${document.storage_key}`
    : `/files/private/${document.storage_key}`;
}

/** GET /api/v1/documents/picker - public-only picker list. */
export function fetchPickerItems(
  params: {
    search?: string;
    type?: string;
    mime?: string;
    folder_id?: string;
    page?: number;
  } = {},
): Promise<PageEnvelope<PickerItem>> {
  const query = new URLSearchParams();
  query.set("page", String(params.page ?? 1));
  query.set("per_page", "50");
  if (params.search) query.set("search", params.search);
  if (params.type) query.set("type", params.type);
  if (params.mime) query.set("mime", params.mime);
  if (params.folder_id !== undefined) query.set("folder_id", params.folder_id);
  return apiGet<PageEnvelope<PickerItem>>(
    `/api/v1/documents/picker?${query.toString()}`,
  );
}

export interface MultiUploadOutcome {
  uploaded: DocumentItem[];
  failed: { name: string; error: string }[];
}

export interface UploadProgress {
  /** Index of the file currently uploading. */
  fileIndex: number;
  fileName: string;
  loaded: number;
  total: number;
}

/** POST /api/v1/documents/upload - multipart multi-file upload
 * with real per-file progress. Files upload one at a time (each
 * its own request) so `onProgress` reports the file actually in
 * flight; a failed file does not lose the rest of the batch.
 * `fetch` has no upload-progress events, so this uses XHR with
 * the same cookie session and CSRF header as `request`. */
export async function uploadDocuments(
  files: File[],
  options: {
    moduleId?: string;
    visibility?: "public" | "private";
    folderId?: string;
    onProgress?: (progress: UploadProgress) => void;
  } = {},
): Promise<MultiUploadOutcome> {
  const uploaded: DocumentItem[] = [];
  const failed: { name: string; error: string }[] = [];
  const csrfToken = await getCsrfToken();
  for (let index = 0; index < files.length; index += 1) {
    const file = files[index];
    const outcome = await uploadOneFile(file, index, csrfToken, options);
    uploaded.push(...outcome.uploaded);
    failed.push(...outcome.failed);
  }
  return { uploaded, failed };
}

function uploadOneFile(
  file: File,
  fileIndex: number,
  csrfToken: string | null,
  options: {
    moduleId?: string;
    visibility?: "public" | "private";
    folderId?: string;
    onProgress?: (progress: UploadProgress) => void;
  },
): Promise<MultiUploadOutcome> {
  return new Promise((resolve) => {
    const form = new FormData();
    form.append("module_id", options.moduleId ?? "documents");
    form.append("visibility", options.visibility ?? "private");
    form.append("folder_id", options.folderId ?? "");
    form.append("file", file, file.name);
    const xhr = new XMLHttpRequest();
    xhr.open("POST", "/api/v1/documents/upload", true);
    xhr.withCredentials = true;
    xhr.setRequestHeader("Accept", "application/json");
    if (csrfToken) xhr.setRequestHeader("X-CSRF-Token", csrfToken);
    xhr.upload.addEventListener("progress", (event) => {
      if (event.lengthComputable) {
        options.onProgress?.({
          fileIndex,
          fileName: file.name,
          loaded: event.loaded,
          total: event.total,
        });
      }
    });
    xhr.addEventListener("load", () => {
      try {
        const body = JSON.parse(xhr.responseText) as ApiEnvelope<MultiUploadOutcome>;
        if (xhr.status >= 200 && xhr.status < 300 && body.data) {
          resolve(body.data);
        } else {
          resolve({
            uploaded: [],
            failed: [
              {
                name: file.name,
                error:
                  body.error?.message ?? `Upload failed with HTTP ${xhr.status}`,
              },
            ],
          });
        }
      } catch {
        resolve({
          uploaded: [],
          failed: [{ name: file.name, error: "Upload returned an unreadable response" }],
        });
      }
    });
    xhr.addEventListener("error", () => {
      resolve({
        uploaded: [],
        failed: [{ name: file.name, error: "Upload failed (network error)" }],
      });
    });
    xhr.send(form);
  });
}

/* Widgets (src/modules/widgets/module.rs) */

export interface WidgetItem {
  widget_id: string;
  pinned?: boolean;
}

export interface WidgetLayout {
  context: string;
  scope_kind: string;
  scope_id: string;
  owner_id: string;
  items: WidgetItem[];
  is_default: boolean;
}

/** GET /api/v1/widgets/layouts/{context} - the caller's layout for
 * that context, or the built-in default when none is saved. */
export function fetchWidgetLayout(context: string): Promise<WidgetLayout> {
  return apiGet<WidgetLayout>(`/api/v1/widgets/layouts/${encodeURIComponent(context)}`);
}

/** PUT /api/v1/widgets/layouts/{context} - save a layout (pin, add,
 * remove, reorder). The server validates ids against the catalogue
 * for that zone. Pass `shared: true` to save the zone's shared
 * (team) default instead of the caller's own layout. */
export function saveWidgetLayout(
  context: string,
  items: WidgetItem[],
  shared = false,
): Promise<WidgetLayout> {
  return apiPut<WidgetLayout>(`/api/v1/widgets/layouts/${encodeURIComponent(context)}`, {
    items,
    shared,
  });
}

/** Order page sections by a widget layout: pinned items first (in
 * layout order), then the remaining layout items, then any section
 * the layout does not mention, in their fixed order. A failed or
 * missing layout leaves the fixed order untouched. Sections whose
 * id has no widget in the catalogue (tasks, notifications, the
 * admin users table) are never reordered away - they always render,
 * after the mapped widgets. */
export function orderWidgetSections<T extends { id: string }>(
  sections: T[],
  layout: WidgetLayout | null,
): T[] {
  if (!layout) return sections;
  const byId = new Map(sections.map((section) => [section.id, section]));
  const ordered: T[] = [];
  const pinned = layout.items.filter((item) => item.pinned === true);
  const rest = layout.items.filter((item) => item.pinned !== true);
  for (const item of [...pinned, ...rest]) {
    const section = byId.get(item.widget_id);
    if (section !== undefined && !ordered.includes(section)) {
      ordered.push(section);
      byId.delete(item.widget_id);
    }
  }
  for (const section of sections) {
    if (byId.has(section.id)) ordered.push(section);
  }
  return ordered;
}

/* Auth */
// POST /api/v1/auth-manager/jwt/issue - the same endpoint
// assets/js/auth.ts posts to (src/modules/auth_manager/module.rs:69).
// The session cookie name defaults to `vault`
// (crates/unified-core/src/config.rs:220); a deployment that renames
// it in `app.session_name` must sign in through the classic /login
// page instead, which reads the configured name from the server.

export interface LoginInput {
  email: string;
  password: string;
  totp_code?: string;
  recovery_code?: string;
}

const SESSION_COOKIE_NAME = "vault";

export async function login(input: LoginInput): Promise<void> {
  const data = await apiPost<{ token?: string }>(
    "/api/v1/auth-manager/jwt/issue",
    input,
  );
  const token = data?.token;
  if (!token) {
    throw new ApiError(0, "Sign-in succeeded but no token was returned.");
  }
  const secure = window.location.protocol === "https:" ? "; Secure" : "";
  document.cookie =
    `${SESSION_COOKIE_NAME}=${token}; Path=/; SameSite=Lax${secure}`;
}

/* Formatting (same rules as assets/js/dashboard.ts) */

export function formatBytes(bytes: unknown): string {
  let value = Number(bytes) || 0;
  const units = ["B", "KB", "MB", "GB", "TB"];
  let index = 0;
  while (value >= 1024 && index < units.length - 1) {
    value /= 1024;
    index += 1;
  }
  return (
    (value >= 100 || index === 0 ? Math.round(value) : value.toFixed(1)) +
    " " +
    units[index]
  );
}

export function formatUptime(totalSeconds: unknown): string {
  const seconds = Math.max(0, Math.floor(Number(totalSeconds) || 0));
  const days = Math.floor(seconds / 86400);
  const hours = Math.floor((seconds % 86400) / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  if (days > 0) return `${days}d ${hours}h`;
  if (hours > 0) return `${hours}h ${minutes}m`;
  if (minutes > 0) return `${minutes}m`;
  return `${seconds}s`;
}

export function formatRelativeTime(isoValue: unknown): string {
  const date = new Date(isoValue as string);
  if (Number.isNaN(date.getTime())) return String(isoValue || "");
  const diffMs = date.getTime() - Date.now();
  const absMinutes = Math.round(Math.abs(diffMs) / 60000);
  const past = diffMs < 0;
  if (absMinutes < 1) return "just now";
  if (absMinutes < 60) return past ? `${absMinutes}m ago` : `in ${absMinutes}m`;
  const absHours = Math.round(absMinutes / 60);
  if (absHours < 24) return past ? `${absHours}h ago` : `in ${absHours}h`;
  const absDays = Math.round(absHours / 24);
  if (absDays < 30) return past ? `${absDays}d ago` : `in ${absDays}d`;
  return date.toLocaleDateString(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
  });
}

export function formatDateTime(isoValue: unknown): string {
  const date = new Date(isoValue as string);
  if (Number.isNaN(date.getTime())) return String(isoValue ?? "");
  return date.toLocaleDateString(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
  });
}

/* Map & Tagging (src/modules/maps) */
// Shapes follow the Rust serializers in src/modules/maps/types.rs.
// The map renders from GET /api/v1/maps/settings (the resolved
// tile URL); every other call is plain CRUD over the same
// envelope. The detach call needs a JSON body on a DELETE, which
// apiDelete does not carry, so it has its own helper here.

export function apiDeleteWithBody<T>(url: string, body?: unknown): Promise<T> {
  return request<T>(url, { method: "DELETE", body: body ?? {}, withCsrf: true });
}

export interface MapLocation {
  id: string;
  tenant_id: string;
  layer_id: string | null;
  label: string;
  kind: string;
  latitude: number;
  longitude: number;
  address: string | null;
  metadata: Record<string, unknown>;
  created_by: string;
  created_at: string;
  updated_at: string;
}

export interface NearbyLocation extends MapLocation {
  distance_km: number;
}

export interface LocationLink {
  tenant_id: string;
  entity_type: string;
  entity_id: string;
  location_id: string;
  role: string;
  created_at: string;
}

export interface EntityLocation extends MapLocation {
  role: string;
}

export interface MapLayer {
  id: string;
  tenant_id: string;
  name: string;
  description: string | null;
  visible: boolean;
  style: Record<string, unknown>;
  created_by: string;
  created_at: string;
  updated_at: string;
}

export interface MapGeofence {
  id: string;
  tenant_id: string;
  name: string;
  description: string | null;
  geometry: unknown;
  created_by: string;
  created_at: string;
  updated_at: string;
}

export interface MapPosition {
  id: string;
  tenant_id: string;
  entity_type: string;
  entity_id: string;
  latitude: number;
  longitude: number;
  accuracy_m: number | null;
  speed_kmh: number | null;
  heading_deg: number | null;
  source: string;
  recorded_at: string;
  created_at: string;
}

export interface LocationKind {
  id: string | null;
  tenant_id: string | null;
  slug: string;
  label: string;
  icon: string;
  color: string;
  enabled: boolean;
  is_builtin?: boolean;
}

export interface LinkRole {
  id: string | null;
  tenant_id: string | null;
  slug: string;
  label: string;
  enabled: boolean;
  is_builtin?: boolean;
}

export interface MapProvider {
  id: string;
  tenant_id: string | null;
  slug: string;
  display_name: string;
  tile_url_template: string;
  attribution: string;
  requires_api_key: boolean;
  max_zoom: number;
  subdomains: string;
  options: Record<string, unknown>;
  enabled: boolean;
  created_at: string;
  updated_at: string;
}

export interface ResolvedMapSettings {
  active_provider: string;
  display_name: string;
  tile_url: string | null;
  attribution: string;
  requires_api_key: boolean;
  key_configured: boolean;
  max_zoom: number;
  subdomains: string[];
  options: Record<string, unknown>;
  default_latitude: number;
  default_longitude: number;
  default_zoom: number;
}

export interface MapSettingsView {
  tenant_id: string;
  active_provider: string;
  has_api_key: boolean;
  default_latitude: number;
  default_longitude: number;
  default_zoom: number;
}

/** GET /api/v1/maps/locations - page 1 by default; pass
 * `page`/`per_page` and optional `kind` / `layer_id`. */
export function fetchMapLocations(
  query = "per_page=100",
): Promise<PageEnvelope<MapLocation>> {
  return apiGet<PageEnvelope<MapLocation>>(`/api/v1/maps/locations?${query}`);
}

export function createMapLocation(input: {
  label: string;
  kind: string;
  latitude: number;
  longitude: number;
  address?: string | null;
  layer_id?: string | null;
  metadata?: Record<string, unknown>;
}): Promise<MapLocation> {
  return apiPost<MapLocation>("/api/v1/maps/locations", input);
}

export function deleteMapLocation(id: string): Promise<MapLocation> {
  return apiDelete<MapLocation>(`/api/v1/maps/locations/${encodeURIComponent(id)}`);
}

/** GET /api/v1/maps/nearby - locations within `radiusKm` of
 * the point, nearest first. */
export function fetchNearbyLocations(
  latitude: number,
  longitude: number,
  radiusKm = 10,
  kind?: string,
): Promise<NearbyLocation[]> {
  const params = new URLSearchParams({
    latitude: String(latitude),
    longitude: String(longitude),
    radius_km: String(radiusKm),
  });
  if (kind) params.set("kind", kind);
  return apiGet<NearbyLocation[]>(`/api/v1/maps/nearby?${params.toString()}`);
}

/** GET /api/v1/maps/entities/{type}/{id}/locations. */
export function fetchEntityLocations(
  entityType: string,
  entityId: string,
): Promise<EntityLocation[]> {
  return apiGet<EntityLocation[]>(
    `/api/v1/maps/entities/${encodeURIComponent(entityType)}/${encodeURIComponent(entityId)}/locations`,
  );
}

export function attachMapLocation(input: {
  entity_type: string;
  entity_id: string;
  location_id: string;
  role: string;
}): Promise<LocationLink> {
  return apiPost<LocationLink>("/api/v1/maps/links", input);
}

export function detachMapLocation(input: {
  entity_type: string;
  entity_id: string;
  location_id: string;
  role: string;
}): Promise<{ detached: boolean }> {
  return apiDeleteWithBody<{ detached: boolean }>("/api/v1/maps/links", input);
}

export function fetchMapLayers(): Promise<MapLayer[]> {
  return apiGet<MapLayer[]>("/api/v1/maps/layers");
}

export function createMapLayer(input: {
  name: string;
  description?: string | null;
  visible?: boolean;
  style?: Record<string, unknown>;
}): Promise<MapLayer> {
  return apiPost<MapLayer>("/api/v1/maps/layers", input);
}

export function deleteMapLayer(id: string): Promise<MapLayer> {
  return apiDelete<MapLayer>(`/api/v1/maps/layers/${encodeURIComponent(id)}`);
}

export function fetchMapGeofences(): Promise<MapGeofence[]> {
  return apiGet<MapGeofence[]>("/api/v1/maps/geofences");
}

export function createMapGeofence(input: {
  name: string;
  description?: string | null;
  geometry: unknown;
}): Promise<MapGeofence> {
  return apiPost<MapGeofence>("/api/v1/maps/geofences", input);
}

export function deleteMapGeofence(id: string): Promise<MapGeofence> {
  return apiDelete<MapGeofence>(`/api/v1/maps/geofences/${encodeURIComponent(id)}`);
}

export function testGeofenceContains(
  id: string,
  latitude: number,
  longitude: number,
): Promise<{ geofence_id: string; name: string; contains: boolean }> {
  return apiPost<{ geofence_id: string; name: string; contains: boolean }>(
    `/api/v1/maps/geofences/${encodeURIComponent(id)}/contains`,
    { latitude, longitude },
  );
}

export function fetchMapPositions(
  entityType: string,
  entityId: string,
  query = "per_page=50",
): Promise<PageEnvelope<MapPosition>> {
  return apiGet<PageEnvelope<MapPosition>>(
    `/api/v1/maps/positions/${encodeURIComponent(entityType)}/${encodeURIComponent(entityId)}?${query}`,
  );
}

export function recordMapPosition(input: {
  entity_type: string;
  entity_id: string;
  latitude: number;
  longitude: number;
  accuracy_m?: number | null;
  speed_kmh?: number | null;
  heading_deg?: number | null;
  source?: string;
  recorded_at?: string;
}): Promise<MapPosition> {
  return apiPost<MapPosition>("/api/v1/maps/positions", input);
}

export function fetchMapKinds(): Promise<LocationKind[]> {
  return apiGet<LocationKind[]>("/api/v1/maps/kinds");
}

export function createMapKind(input: {
  slug: string;
  label: string;
  icon?: string;
  color?: string;
  enabled?: boolean;
}): Promise<LocationKind> {
  return apiPost<LocationKind>("/api/v1/maps/kinds", input);
}

export function deleteMapKind(id: string): Promise<LocationKind> {
  return apiDelete<LocationKind>(`/api/v1/maps/kinds/${encodeURIComponent(id)}`);
}

export function fetchMapRoles(): Promise<LinkRole[]> {
  return apiGet<LinkRole[]>("/api/v1/maps/roles");
}

export function createMapRole(input: {
  slug: string;
  label: string;
  enabled?: boolean;
}): Promise<LinkRole> {
  return apiPost<LinkRole>("/api/v1/maps/roles", input);
}

export function deleteMapRole(id: string): Promise<LinkRole> {
  return apiDelete<LinkRole>(`/api/v1/maps/roles/${encodeURIComponent(id)}`);
}

export function fetchMapProviders(): Promise<MapProvider[]> {
  return apiGet<MapProvider[]>("/api/v1/maps/providers");
}

export function createMapProvider(input: {
  slug: string;
  display_name: string;
  tile_url_template: string;
  attribution?: string;
  requires_api_key?: boolean;
  max_zoom?: number;
  subdomains?: string;
  options?: Record<string, unknown>;
  enabled?: boolean;
}): Promise<MapProvider> {
  return apiPost<MapProvider>("/api/v1/maps/providers", input);
}

export function deleteMapProvider(id: string): Promise<MapProvider> {
  return apiDelete<MapProvider>(`/api/v1/maps/providers/${encodeURIComponent(id)}`);
}

/** GET /api/v1/maps/settings - the resolved tile URL for the
 * active provider (null when the provider needs a key and none
 * is stored). The key itself is never returned. */
export function fetchMapSettings(): Promise<ResolvedMapSettings> {
  return apiGet<ResolvedMapSettings>("/api/v1/maps/settings");
}

/** PUT /api/v1/maps/settings - `api_key` is write-only: omit
 * it to keep the stored key, send "" to clear it. */
export function saveMapSettings(input: {
  active_provider?: string;
  api_key?: string;
  default_latitude?: number;
  default_longitude?: number;
  default_zoom?: number;
}): Promise<MapSettingsView> {
  return apiPut<MapSettingsView>("/api/v1/maps/settings", input);
}
