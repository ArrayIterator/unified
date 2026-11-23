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

/* ---------- CSRF ---------- */
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

/* ---------- Core request ---------- */

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

/* ---------- Response shapes ---------- */
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

/* ---------- Endpoint wrappers ---------- */
// Route registrations (src/modules/*/module.rs) are cited per call.

/** GET /api/v1/extensions — every page (per_page clamps to 100). */
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

/* ---------- Widgets (src/modules/widgets/module.rs) ---------- */

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

/** GET /api/v1/widgets/layouts/{context} — the caller's layout for
 * that context, or the built-in default when none is saved. */
export function fetchWidgetLayout(context: string): Promise<WidgetLayout> {
  return apiGet<WidgetLayout>(`/api/v1/widgets/layouts/${encodeURIComponent(context)}`);
}

/** PUT /api/v1/widgets/layouts/{context} — save a layout (pin, add,
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
 * admin users table) are never reordered away — they always render,
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

/* ---------- Auth ---------- */
// POST /api/v1/auth-manager/jwt/issue — the same endpoint
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

/* ---------- Formatting (same rules as assets/js/dashboard.ts) ---------- */

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
