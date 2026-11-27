/* Unified dashboard behaviour.
 *
 * TypeScript, transpiled in-memory by the Rust server at startup; no framework:
 *   - dark / light theme toggle, persisted in localStorage; with no stored
 *     choice the system `prefers-color-scheme` setting is followed, and a
 *     later system change is picked up while no explicit choice exists;
 *   - sidebar: drawer on small screens, collapsible icon rail on desktop;
 *   - module list rendered from the Extensions API
 *     (`GET /api/v1/extensions`), which returns the standard envelope
 *     `{"data": ...}` (a plain array, or a paginated `{"items": [...]}`);
 *   - dashboard widgets fed by verified endpoints only:
 *       GET /api/v1/service-monitor/status  (system status)
 *       GET /api/v1/tasks                   (open tasks, due soonest first)
 *       GET /api/v1/notifications          (latest notifications)
 *       POST /api/v1/notifications/{id}/read (mark one notification read)
 *     Every widget degrades to an honest "unavailable" note when its
 *     endpoint does not answer; nothing here calls an unverified URL.
 *   - dashboard section tables: each section page (licenses,
 *     downloads, orders, modules, users) fills its table from the
 *     matching dashboards API endpoint; a failed fetch leaves an
 *     honest "could not load" note instead of inventing rows.
 *   - command palette (Ctrl/Command+K): searches the navigation links
 *     already on the page. There is no global search API in the tree
 *     (no search module exists yet), so the palette is local-only and
 *     says so in its footer.
 *   - toasts, accessible modals (Esc to close, overlay click, focus
 *     returned to the opener), and flash messages dismissed after a
 *     few seconds.
 */

(function () {
  "use strict";

  /* Types (compile-time only; erased by the transpiler) */

  type Theme = "dark" | "light";

  interface ApiEnvelope<T = unknown> {
    data?: T;
  }

  interface ModuleItem {
    id: string;
    name?: string;
    description?: string;
    version?: string;
    enabled?: boolean;
    is_enabled?: boolean;
  }

  interface SystemStatusData {
    status?: string;
    uptime_seconds?: unknown;
    version?: string;
    cpu_usage_percent?: unknown;
    cpu_count?: unknown;
    workers?: unknown;
    total_memory_bytes?: unknown;
    used_memory_bytes?: unknown;
  }

  interface TaskItem {
    status?: string;
    title?: string;
    priority?: string;
    due_date?: string;
    overdue?: boolean;
    project_name?: string;
  }

  interface NotificationItem {
    id: string;
    title?: string;
    body?: string;
    kind?: string;
    read?: boolean;
    created_at?: string;
  }

  interface PaletteItem {
    group: string;
    label: string;
    href?: string | null;
    action?: string;
  }

  interface LicenseItem {
    product?: unknown;
    status?: unknown;
    expires_at?: string | null;
    created_at?: unknown;
  }

  interface DownloadItem {
    software_slug?: string | null;
    downloaded_at?: unknown;
  }

  interface OrderItem {
    status?: unknown;
    total?: string | number;
    currency?: string;
    created_at?: unknown;
  }

  interface UserItem {
    name?: unknown;
    email?: unknown;
    is_active?: boolean;
    created_at?: unknown;
  }

  interface AdminModulesData {
    modules?: ModuleItem[];
    dependencies_valid?: boolean;
  }

  interface UnifiedDashboardApi {
    applyTheme: (theme: Theme, persist?: boolean) => void;
    showFlash: (message: string, type?: string) => void;
    showToast: (message: string, type?: string) => HTMLDivElement;
    openModal: (overlay: HTMLElement | null) => void;
    closeModal: (overlay: HTMLElement | null) => void;
    openPalette: () => void;
    closePalette: () => void;
    loadModules: () => void;
    loadSystemStatus: () => void;
    loadTasks: () => void;
    loadNotifications: () => void;
  }

  var THEME_KEY = "unified-theme";
  var SIDEBAR_KEY = "unified-sidebar-collapsed";
  var root = document.documentElement;

  /* Small helpers */

  function escapeHtml(value: unknown): string {
    return String(value)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;")
      .replace(/'/g, "&#39;");
  }

  function itemsFromEnvelope<T>(data: unknown): T[] {
    if (Array.isArray(data)) {
      return data as T[];
    }
    if (
      data !== null &&
      typeof data === "object" &&
      Array.isArray((data as { items?: unknown }).items)
    ) {
      return (data as { items: unknown }).items as T[];
    }
    return [];
  }

  function fetchJson(url: string): Promise<unknown> {
    return window
      .fetch(url, {
        headers: { Accept: "application/json" },
        credentials: "same-origin",
      })
      .then(function (response) {
        if (!response.ok) {
          throw new Error("Request to " + url + " returned HTTP " + response.status);
        }
        return response.json();
      });
  }

  /* CSRF protection */
  // Cookie-authenticated mutations require a valid `X-CSRF-Token`
  // header: the server answers 403 without one. The token is bound
  // to the session, fetched from `GET /api/v1/security/csrf-token`,
  // and cached in memory; a 403 whose error mentions CSRF clears
  // the cache so the next attempt refetches it. A failed fetch
  // resolves to `null` and the request goes out without the header;
  // the server decides (pre-auth calls need no token).
  var csrfToken: string | null = null;
  var csrfTokenRequest: Promise<string | null> | null = null;

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
        .then(function (response: Response): Promise<unknown> {
          return response.ok ? response.json() : Promise.resolve(null);
        })
        .then(function (body: unknown): string | null {
          var envelope = body as { data?: { csrf_token?: unknown } } | null;
          var token = envelope && envelope.data ? envelope.data.csrf_token : null;
          csrfToken = typeof token === "string" && token ? token : null;
          return csrfToken;
        })
        .catch(function (): null {
          return null;
        });
      csrfTokenRequest.then(function (): void {
        // A failed fetch must not poison the cache: the next
        // mutation retries the token endpoint.
        if (!csrfToken) {
          csrfTokenRequest = null;
        }
      });
    }
    return csrfTokenRequest;
  }

  function isCsrfForbidden(response: Response): Promise<boolean> {
    if (response.status !== 403) {
      return Promise.resolve(false);
    }
    return response
      .clone()
      .json()
      .then(function (body: unknown): boolean {
        var envelope = body as { error?: { message?: unknown } } | null;
        var message = envelope && envelope.error ? envelope.error.message : null;
        return typeof message === "string" && message.indexOf("CSRF") !== -1;
      })
      .catch(function (): boolean {
        return false;
      });
  }

  function sendWithCsrf(
    url: string,
    method: string,
    token: string | null,
    retried: boolean
  ): Promise<Response> {
    var headers: Record<string, string> = { Accept: "application/json" };
    if (token) {
      headers["X-CSRF-Token"] = token;
    }
    return window
      .fetch(url, { method: method, headers: headers, credentials: "same-origin" })
      .then(function (response: Response): Response | Promise<Response> {
        if (!token || retried) {
          return response;
        }
        return isCsrfForbidden(response).then(function (csrf): Response | Promise<Response> {
          if (!csrf) {
            return response;
          }
          csrfToken = null;
          csrfTokenRequest = null;
          return fetchCsrfToken().then(function (fresh: string | null): Response | Promise<Response> {
            if (!fresh) {
              return response;
            }
            return sendWithCsrf(url, method, fresh, true);
          });
        });
      });
  }

  // One POST/DELETE/etc. with the CSRF header attached; retries
  // once with a fresh token when the server says the token expired.
  function fetchWithCsrf(url: string, method: string): Promise<Response> {
    return fetchCsrfToken().then(function (token: string | null): Promise<Response> {
      return sendWithCsrf(url, method, token, false);
    });
  }

  function formatBytes(bytes: unknown): string {
    var value = Number(bytes) || 0;
    var units = ["B", "KB", "MB", "GB", "TB"];
    var index = 0;
    while (value >= 1024 && index < units.length - 1) {
      value /= 1024;
      index += 1;
    }
    return (value >= 100 || index === 0 ? Math.round(value) : value.toFixed(1)) + " " + units[index];
  }

  function formatUptime(totalSeconds: unknown): string {
    var seconds = Math.max(0, Math.floor(Number(totalSeconds) || 0));
    var days = Math.floor(seconds / 86400);
    var hours = Math.floor((seconds % 86400) / 3600);
    var minutes = Math.floor((seconds % 3600) / 60);
    if (days > 0) {
      return days + "d " + hours + "h";
    }
    if (hours > 0) {
      return hours + "h " + minutes + "m";
    }
    if (minutes > 0) {
      return minutes + "m";
    }
    return seconds + "s";
  }

  function formatRelativeTime(isoValue: unknown): string {
    var date = new Date(isoValue as string);
    if (isNaN(date.getTime())) {
      return String(isoValue || "");
    }
    var diffMs = date.getTime() - Date.now();
    var absMinutes = Math.round(Math.abs(diffMs) / 60000);
    var past = diffMs < 0;
    if (absMinutes < 1) {
      return "just now";
    }
    if (absMinutes < 60) {
      return past ? absMinutes + "m ago" : "in " + absMinutes + "m";
    }
    var absHours = Math.round(absMinutes / 60);
    if (absHours < 24) {
      return past ? absHours + "h ago" : "in " + absHours + "h";
    }
    var absDays = Math.round(absHours / 24);
    if (absDays < 30) {
      return past ? absDays + "d ago" : "in " + absDays + "d";
    }
    return date.toLocaleDateString(undefined, {
      year: "numeric",
      month: "short",
      day: "numeric",
    });
  }

  function icon(name: string, extraClass?: string): string {
    return (
      '<svg class="icon' +
      (extraClass ? " " + extraClass : "") +
      '" aria-hidden="true"><use href="#icon-' +
      name +
      '"></use></svg>'
    );
  }

  /* Module icons (sprite + placeholder fallback)
   * The module sprite is embedded in the page by the server
   * ({{{module_icons_sprite}}} in dashboard.hbs). When a module has no
   * symbol yet, the placeholder is shown instead of an empty <use>. */
  function moduleIcon(id: string): string {
    var safe = /^[a-z0-9-]+$/.test(id) ? id : "";
    var shown =
      safe !== "" && document.getElementById("module-icon-" + safe)
        ? safe
        : "placeholder";
    return (
      '<svg class="module-icon" data-icon="' +
      shown +
      '" aria-hidden="true"><use href="#module-icon-' +
      shown +
      '"></use></svg>'
    );
  }

  /* Theme: dark / light */

  function storedTheme(): Theme | null {
    try {
      var value = window.localStorage.getItem(THEME_KEY);
      return value === "dark" || value === "light" ? value : null;
    } catch (error) {
      return null;
    }
  }

  function systemTheme(): Theme {
    return window.matchMedia &&
      window.matchMedia("(prefers-color-scheme: dark)").matches
      ? "dark"
      : "light";
  }

  function currentTheme(): Theme {
    return root.getAttribute("data-theme") === "dark" ? "dark" : "light";
  }

  function applyTheme(theme: Theme, persist?: boolean): void {
    var value: Theme = theme === "dark" ? "dark" : "light";
    root.setAttribute("data-theme", value);
    root.classList.remove("theme-light", "theme-dark");
    root.classList.add("theme-" + value);
    if (persist !== false) {
      try {
        window.localStorage.setItem(THEME_KEY, value);
      } catch (error) {
        // Private browsing or blocked storage: the theme still applies
        // for this page load.
      }
    }
    updateToggleLabels(value);
  }

  function updateToggleLabels(theme: Theme): void {
    document.querySelectorAll("[data-theme-toggle]").forEach(function (button) {
      button.setAttribute(
        "aria-label",
        theme === "dark" ? "Switch to light theme" : "Switch to dark theme"
      );
      var label = button.querySelector(".theme-toggle-label");
      if (label) {
        label.textContent = theme === "dark" ? "Light" : "Dark";
      }
    });
    document.querySelectorAll("[data-theme-icon]").forEach(function (holder) {
      holder.innerHTML = theme === "dark" ? icon("sun") : icon("moon");
    });
  }

  function initTheme(): void {
    var stored = storedTheme();
    if (stored) {
      applyTheme(stored, false);
      return;
    }
    applyTheme(systemTheme(), false);
    // Follow the operating system while the user has not chosen.
    if (window.matchMedia) {
      var query = window.matchMedia("(prefers-color-scheme: dark)");
      var onSystemChange = function (event: MediaQueryListEvent) {
        if (!storedTheme()) {
          applyTheme(event.matches ? "dark" : "light", false);
        }
      };
      if (typeof query.addEventListener === "function") {
        query.addEventListener("change", onSystemChange);
      } else if (typeof query.addListener === "function") {
        query.addListener(onSystemChange);
      }
    }
  }

  document.querySelectorAll("[data-theme-toggle]").forEach(function (button) {
    button.addEventListener("click", function () {
      applyTheme(currentTheme() === "dark" ? "light" : "dark");
    });
  });

  /* Sidebar: drawer (small screens) + collapse (desktop) */

  var shell = document.querySelector(".dashboard-shell");
  var backdrop = document.querySelector("[data-sidebar-backdrop]") as HTMLElement | null;

  function setSidebarOpen(open: boolean): void {
    if (!shell) {
      return;
    }
    shell.classList.toggle("sidebar-open", open);
    if (backdrop) {
      backdrop.hidden = !open;
    }
    document.querySelectorAll("[data-sidebar-toggle]").forEach(function (button) {
      button.setAttribute("aria-expanded", open ? "true" : "false");
    });
  }

  document.querySelectorAll("[data-sidebar-toggle]").forEach(function (button) {
    button.addEventListener("click", function () {
      setSidebarOpen(!shell || !shell.classList.contains("sidebar-open"));
    });
  });

  if (backdrop) {
    backdrop.addEventListener("click", function () {
      setSidebarOpen(false);
    });
  }

  document.addEventListener("keydown", function (event) {
    if (event.key === "Escape" && shell && shell.classList.contains("sidebar-open")) {
      setSidebarOpen(false);
    }
  });

  function setSidebarCollapsed(collapsed: boolean): void {
    if (!shell) {
      return;
    }
    shell.classList.toggle("sidebar-collapsed", collapsed);
    try {
      window.localStorage.setItem(SIDEBAR_KEY, collapsed ? "collapsed" : "expanded");
    } catch (error) {
      // Storage unavailable: the collapse still applies for this load.
    }
    document.querySelectorAll("[data-sidebar-collapse]").forEach(function (button) {
      button.setAttribute("aria-expanded", collapsed ? "false" : "true");
      button.setAttribute(
        "aria-label",
        collapsed ? "Expand sidebar" : "Collapse sidebar"
      );
    });
  }

  document.querySelectorAll("[data-sidebar-collapse]").forEach(function (button) {
    button.addEventListener("click", function () {
      setSidebarCollapsed(!shell || !shell.classList.contains("sidebar-collapsed"));
    });
  });

  (function initSidebarCollapsed() {
    if (!shell) {
      return;
    }
    try {
      if (window.localStorage.getItem(SIDEBAR_KEY) === "collapsed") {
        setSidebarCollapsed(true);
      }
    } catch (error) {
      // Ignore: sidebar simply starts expanded.
    }
  })();

  /* Flash messages */

  document.querySelectorAll("[data-flash]").forEach(function (flash) {
    window.setTimeout(function () {
      flash.remove();
    }, 6000);
  });

  function showFlash(message: string, type?: string): void {
    var existing = document.querySelector("[data-flash]");
    if (existing) {
      existing.remove();
    }
    var flash = document.createElement("div");
    flash.className = "flash flash-" + (type || "info");
    flash.setAttribute("role", "alert");
    flash.setAttribute("data-flash", "");
    flash.textContent = message;
    var content = document.querySelector(".dashboard-content") || document.body;
    content.prepend(flash);
    window.setTimeout(function () {
      flash.remove();
    }, 6000);
  }

  /* Toasts */

  function toastRegion(): HTMLElement {
    var region = document.getElementById("toast-region");
    if (region) {
      return region;
    }
    region = document.createElement("div");
    region.id = "toast-region";
    region.className = "toast-region";
    region.setAttribute("aria-live", "polite");
    document.body.appendChild(region);
    return region;
  }

  function showToast(message: string, type?: string): HTMLDivElement {
    var toast = document.createElement("div");
    toast.className = "toast toast-" + (type || "info");
    toast.setAttribute("role", "status");
    var iconName =
      type === "success" ? "check-circle" : type === "error" ? "alert" : "info";
    toast.innerHTML =
      icon(iconName) +
      '<span class="toast-message"></span>' +
      '<button type="button" class="toast-close" aria-label="Dismiss notification">&times;</button>';
    toast.querySelector(".toast-message")!.textContent = message;
    toast.querySelector(".toast-close")!.addEventListener("click", function () {
      toast.remove();
    });
    toastRegion().appendChild(toast);
    window.setTimeout(function () {
      toast.remove();
    }, 6000);
    return toast;
  }

  /* Dropdown menus */

  document.querySelectorAll("[data-menu-toggle]").forEach(function (toggle) {
    const menu = document.getElementById(toggle.getAttribute("aria-controls") ?? "");
    if (!menu) {
      return;
    }
    toggle.addEventListener("click", function (event) {
      event.stopPropagation();
      var open = menu.hidden;
      document.querySelectorAll<HTMLElement>(".menu").forEach(function (other) {
        other.hidden = true;
      });
      document.querySelectorAll("[data-menu-toggle]").forEach(function (other) {
        other.setAttribute("aria-expanded", "false");
      });
      menu.hidden = !open;
      toggle.setAttribute("aria-expanded", open ? "true" : "false");
      if (open) {
        var first = menu.querySelector<HTMLElement>(".menu-item");
        if (first) {
          first.focus();
        }
      }
    });
  });

  document.addEventListener("click", function (event) {
    if (!(event.target as Element).closest(".menu-wrap")) {
      document.querySelectorAll<HTMLElement>(".menu").forEach(function (menu) {
        menu.hidden = true;
      });
      document.querySelectorAll("[data-menu-toggle]").forEach(function (toggle) {
        toggle.setAttribute("aria-expanded", "false");
      });
    }
  });

  document.addEventListener("keydown", function (event) {
    if (event.key === "Escape") {
      document.querySelectorAll<HTMLElement>(".menu").forEach(function (menu) {
        menu.hidden = true;
      });
      document.querySelectorAll("[data-menu-toggle]").forEach(function (toggle) {
        toggle.setAttribute("aria-expanded", "false");
      });
    }
  });

  /* Tabs */

  document.querySelectorAll("[data-tabs]").forEach(function (tablist) {
    var tabs = tablist.querySelectorAll("[data-tab]");
    tabs.forEach(function (tab) {
      tab.addEventListener("click", function () {
        var targetId = tab.getAttribute("data-tab");
        tabs.forEach(function (other) {
          var active = other === tab;
          other.classList.toggle("is-active", active);
          other.setAttribute("aria-selected", active ? "true" : "false");
        });
        document.querySelectorAll<HTMLElement>("[data-tab-panel]").forEach(function (panel) {
          panel.hidden = panel.id !== targetId;
        });
      });
    });
  });

  /* Modals (Esc, overlay click, focus return) */

  var lastModalOpener: Element | null = null;

  function focusableIn(element: Element): HTMLElement[] {
    return Array.prototype.slice
      .call(
        element.querySelectorAll(
          'a[href], button:not([disabled]), input, select, textarea, [tabindex]:not([tabindex="-1"])'
        )
      )
      .filter(function (node) {
        return node.offsetParent !== null;
      }) as HTMLElement[];
  }

  function openModal(overlay: HTMLElement | null): void {
    if (!overlay) {
      return;
    }
    lastModalOpener = document.activeElement;
    overlay.hidden = false;
    document.body.style.overflow = "hidden";
    var focusable = focusableIn(overlay);
    if (focusable.length > 0) {
      focusable[0].focus();
    }
  }

  function closeModal(overlay: HTMLElement | null): void {
    if (!overlay) {
      return;
    }
    overlay.hidden = true;
    document.body.style.overflow = "";
    // lastModalOpener is typed as Element; narrow to HTMLElement for focus().
    var opener = lastModalOpener as HTMLElement | null;
    if (opener && typeof opener.focus === "function") {
      opener.focus();
      lastModalOpener = null;
    }
  }

  document.querySelectorAll("[data-modal-open]").forEach(function (button) {
    button.addEventListener("click", function () {
      openModal(document.getElementById(button.getAttribute("data-modal-open") ?? ""));
    });
  });

  document.querySelectorAll<HTMLElement>("[data-modal]").forEach(function (overlay) {
    overlay.addEventListener("click", function (event) {
      if (event.target === overlay) {
        closeModal(overlay);
      }
    });
    overlay.querySelectorAll("[data-modal-close]").forEach(function (button) {
      button.addEventListener("click", function () {
        closeModal(overlay);
      });
    });
    overlay.addEventListener("keydown", function (event) {
      if (event.key === "Escape") {
        closeModal(overlay);
        return;
      }
      // Minimal focus trap: keep Tab inside the modal.
      if (event.key !== "Tab") {
        return;
      }
      var focusable = focusableIn(overlay);
      if (focusable.length === 0) {
        return;
      }
      var first = focusable[0];
      var last = focusable[focusable.length - 1];
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    });
  });

  /* Module list from /api/v1/extensions */

  function modulesFromEnvelope(data: unknown): ModuleItem[] {
    return itemsFromEnvelope<ModuleItem>(data);
  }

  function renderModuleNav(modules: ModuleItem[]): void {
    var nav = document.getElementById("module-nav-list");
    if (!nav) {
      return;
    }
    if (modules.length === 0) {
      nav.innerHTML =
        '<span class="sidebar-empty">No modules registered yet.</span>';
      return;
    }
    nav.innerHTML = modules
      .map(function (module) {
        return (
          '<a class="sidebar-link" href="/dashboard/' +
          encodeURIComponent(module.id) +
          '">' +
          moduleIcon(module.id) +
          '<span class="sidebar-label">' +
          escapeHtml(module.name || module.id) +
          "</span></a>"
        );
      })
      .join("");
  }

  function renderModuleGrid(modules: ModuleItem[]): void {
    var grid = document.getElementById("module-grid");
    if (!grid) {
      return;
    }
    var count = document.querySelector("[data-module-count]");
    if (count) {
      count.textContent =
        modules.length === 0
          ? "None registered"
          : modules.length + " registered";
    }
    if (modules.length === 0) {
      grid.innerHTML =
        '<p class="empty-state">No modules registered yet. Modules appear here once they are registered with the application.</p>';
      return;
    }
    grid.innerHTML = modules
      .map(function (module) {
        var enabled = module.enabled !== false && module.is_enabled !== false;
        var badge = enabled
          ? '<span class="badge badge-enabled">' + icon("check", "icon-sm") + 'Enabled</span>'
          : '<span class="badge badge-disabled">Disabled</span>';
        var version = module.version
          ? "<span>v" + escapeHtml(module.version) + "</span>"
          : "";
        return (
          '<article class="module-card">' +
          '<span class="module-card-icon" aria-hidden="true">' +
          moduleIcon(module.id) +
          "</span>" +
          "<h3>" +
          escapeHtml(module.name || module.id) +
          "</h3>" +
          '<p class="module-description">' +
          escapeHtml(module.description || "") +
          "</p>" +
          '<p class="module-meta">' +
          badge +
          version +
          "</p>" +
          '<a class="button button-small" href="/dashboard/' +
          encodeURIComponent(module.id) +
          '">Open</a>' +
          "</article>"
        );
      })
      .join("");
  }

  function setModuleStatus(message: string): void {
    var status = document.querySelector("[data-module-status]");
    if (status) {
      status.textContent = message;
    }
  }

  function loadModules(): void {
    var grid = document.getElementById("module-grid");
    if (!grid) {
      return;
    }
    // Only replace the server-rendered list when the API answers;
    // a failed fetch leaves the server-rendered content in place.
    fetchJson("/api/v1/extensions")
      .then(function (envelope: unknown) {
        var modules = modulesFromEnvelope((envelope as ApiEnvelope | null)?.data);
        renderModuleNav(modules);
        renderModuleGrid(modules);
        setModuleStatus("");
      })
      .catch(function (error) {
        setModuleStatus(
          "Could not refresh the module list from the server; showing the list rendered with the page."
        );
        if (window.console && window.console.warn) {
          window.console.warn("Failed to load modules:", error);
        }
      });
  }

  /* Dashboard search (filters the module grid) */

  const searchInput = document.querySelector("[data-dashboard-search]") as HTMLInputElement | null;
  if (searchInput) {
    searchInput.addEventListener("input", function () {
      var query = searchInput.value.trim().toLowerCase();
      document.querySelectorAll<HTMLElement>("#module-grid .module-card").forEach(function (card) {
        var text = (card.textContent || "").toLowerCase();
        card.style.display =
          query === "" || text.indexOf(query) !== -1 ? "" : "none";
      });
    });
  }

  /* System status widget: GET /api/v1/service-monitor/status */

  function statusBadge(status?: string): string {
    if (status === "ok") {
      return '<span class="badge badge-success badge-has-dot">Operational</span>';
    }
    if (status === "maintenance") {
      return '<span class="badge badge-warning badge-has-dot">Maintenance</span>';
    }
    if (status === "paused") {
      return '<span class="badge badge-warning badge-has-dot">Paused</span>';
    }
    return '<span class="badge badge-neutral badge-has-dot">' + escapeHtml(status || "Unknown") + "</span>";
  }

  function loadSystemStatus(): void {
    const widget = document.querySelector('[data-widget="system-status"]');
    if (!widget) {
      return;
    }
    fetchJson("/api/v1/service-monitor/status")
      .then(function (envelope: unknown) {
        var data = ((envelope as ApiEnvelope<SystemStatusData> | null)?.data || {}) as SystemStatusData;
        var total = Number(data.total_memory_bytes) || 0;
        var used = Number(data.used_memory_bytes) || 0;
        var memoryPercent = total > 0 ? Math.round((used / total) * 100) : 0;
        var cpu = Number(data.cpu_usage_percent) || 0;
        var meterClass =
          memoryPercent >= 90 ? " is-danger" : memoryPercent >= 75 ? " is-warning" : "";
        widget.innerHTML =
          '<div class="status-summary">' +
          '<div><p class="status-metric-label">Status</p><p class="status-metric-value">' +
          '<span class="status-dot' +
          (data.status === "ok" ? "" : " is-warning") +
          '" aria-hidden="true"></span> ' +
          statusBadge(data.status) +
          "</p></div>" +
          '<div><p class="status-metric-label">Uptime</p><p class="status-metric-value">' +
          escapeHtml(formatUptime(data.uptime_seconds)) +
          '</p><p class="status-metric-sub">Version ' +
          escapeHtml(data.version || "unknown") +
          "</p></div>" +
          '<div><p class="status-metric-label">CPU</p><p class="status-metric-value">' +
          cpu.toFixed(1) +
          '%</p><p class="status-metric-sub">' +
          escapeHtml(String(data.cpu_count || 0)) +
          " cores &middot; " +
          escapeHtml(String(data.workers || 0)) +
          " workers</p></div>" +
          '<div><p class="status-metric-label">Memory</p><p class="status-metric-value">' +
          escapeHtml(formatBytes(used)) +
          ' / ' +
          escapeHtml(formatBytes(total)) +
          '</p><div class="meter" role="img" aria-label="Memory usage ' +
          memoryPercent +
          ' percent"><div class="meter-fill' +
          meterClass +
          '" style="width: ' +
          memoryPercent +
          '%"></div></div></div>' +
          "</div>";
      })
      .catch(function (error) {
        widget.innerHTML =
          '<p class="empty-state">System status is unavailable right now. The service monitor did not answer.</p>';
        if (window.console && window.console.warn) {
          window.console.warn("Failed to load system status:", error);
        }
      });
  }

  /* Tasks widget: GET /api/v1/tasks */

  var TASK_STATUS_LABELS: Record<string, string> = {
    todo: "To do",
    in_progress: "In progress",
    review: "In review",
    done: "Done",
    blocked: "Blocked",
    cancelled: "Cancelled",
  };

  function taskStatusBadge(status?: string): string {
    var label = TASK_STATUS_LABELS[status ?? ""] || status || "Unknown";
    var kind =
      status === "done"
        ? "badge-success"
        : status === "blocked"
          ? "badge-danger"
          : status === "in_progress" || status === "review"
            ? "badge-info"
            : "badge-neutral";
    return '<span class="badge ' + kind + '">' + escapeHtml(label) + "</span>";
  }

  function taskPriorityBadge(priority?: string): string {
    var kind =
      priority === "urgent"
        ? "badge-danger"
        : priority === "high"
          ? "badge-warning"
          : priority === "medium"
            ? "badge-info"
            : "badge-neutral";
    var label = priority
      ? priority.charAt(0).toUpperCase() + priority.slice(1)
      : "Medium";
    return '<span class="badge ' + kind + '">' + escapeHtml(label) + "</span>";
  }

  function loadTasks(): void {
    const widget = document.querySelector('[data-widget="tasks"]');
    if (!widget) {
      return;
    }
    fetchJson("/api/v1/tasks?per_page=8&sort=due_date&order=asc")
      .then(function (envelope: unknown) {
        var tasks = itemsFromEnvelope<TaskItem>((envelope as ApiEnvelope | null)?.data)
          .filter(function (task) {
            return task.status !== "done" && task.status !== "cancelled";
          })
          .slice(0, 5);
        if (tasks.length === 0) {
          widget.innerHTML =
            '<p class="empty-state">No open tasks. New tasks appear here once they are created.</p>';
          return;
        }
        widget.innerHTML =
          '<ul class="widget-list">' +
          tasks
            .map(function (task) {
              var due = task.due_date
                ? '<span>' + icon("clock", "icon-sm") + " " + escapeHtml(formatRelativeTime(task.due_date)) + "</span>"
                : "";
              var overdue = task.overdue
                ? '<span class="badge badge-danger">Overdue</span>'
                : "";
              var project = task.project_name
                ? '<span>' + escapeHtml(task.project_name) + "</span>"
                : "";
              return (
                '<li class="widget-list-item">' +
                '<div class="widget-list-item-body">' +
                '<p class="widget-list-item-title">' +
                escapeHtml(task.title || "Untitled task") +
                "</p>" +
                '<p class="widget-list-item-meta">' +
                taskPriorityBadge(task.priority) +
                taskStatusBadge(task.status) +
                overdue +
                project +
                due +
                "</p></div>" +
                '<span class="widget-list-item-action"><a class="button button-small button-secondary" href="/dashboard/tasks">Open</a></span>' +
                "</li>"
              );
            })
            .join("") +
          "</ul>";
      })
      .catch(function (error) {
        widget.innerHTML =
          '<p class="empty-state">Tasks are unavailable right now. The tasks service did not answer.</p>';
        if (window.console && window.console.warn) {
          window.console.warn("Failed to load tasks:", error);
        }
      });
  }

  /* Notifications widget: GET /api/v1/notifications */

  function notificationKindBadge(kind?: string): string {
    var map: Record<string, string> = {
      info: "badge-info",
      warning: "badge-warning",
      error: "badge-danger",
      success: "badge-success",
    };
    var kindClass = map[kind ?? ""] || "badge-neutral";
    return '<span class="badge ' + kindClass + '">' + escapeHtml(kind || "info") + "</span>";
  }

  function loadNotifications(): void {
    const widget = document.querySelector('[data-widget="notifications"]');
    if (!widget) {
      return;
    }
    fetchJson("/api/v1/notifications?per_page=8")
      .then(function (envelope: unknown) {
        var allNotifications = itemsFromEnvelope<NotificationItem>((envelope as ApiEnvelope | null)?.data);
        var unread = allNotifications.filter(function (item) {
          return !item.read;
        }).length;
        var notifications = allNotifications.slice(0, 5);
        var bellBadge = document.querySelector("[data-notification-count]") as HTMLElement | null;
        if (bellBadge) {
          bellBadge.textContent = String(unread);
          bellBadge.hidden = unread === 0;
        }
        if (notifications.length === 0) {
          widget.innerHTML =
            '<p class="empty-state">No notifications yet. New notifications appear here as they arrive.</p>';
          return;
        }
        widget.innerHTML =
          '<ul class="widget-list">' +
          notifications
            .map(function (item) {
              return (
                '<li class="widget-list-item' +
                (item.read ? "" : " is-unread") +
                '">' +
                '<div class="widget-list-item-body">' +
                '<p class="widget-list-item-title">' +
                escapeHtml(item.title || "Notification") +
                "</p>" +
                (item.body
                  ? '<p class="widget-list-item-text">' + escapeHtml(item.body) + "</p>"
                  : "") +
                '<p class="widget-list-item-meta">' +
                notificationKindBadge(item.kind) +
                "<span>" +
                escapeHtml(formatRelativeTime(item.created_at)) +
                "</span></p></div>" +
                (item.read
                  ? ""
                  : '<span class="widget-list-item-action"><button type="button" class="button button-small button-ghost" data-mark-read="' +
                    escapeHtml(item.id) +
                    '">Mark read</button></span>') +
                "</li>"
              );
            })
            .join("") +
          "</ul>";
        widget.querySelectorAll<HTMLButtonElement>("[data-mark-read]").forEach(function (button) {
          button.addEventListener("click", function () {
            // data-mark-read is always set by the render above, so this is never null.
            var id = button.getAttribute("data-mark-read") as string;
            button.disabled = true;
            button.textContent = "Saving...";
            fetchWithCsrf(
              "/api/v1/notifications/" + encodeURIComponent(id) + "/read",
              "POST"
            )
              .then(function (response) {
                if (!response.ok) {
                  throw new Error("Mark read returned HTTP " + response.status);
                }
                loadNotifications();
              })
              .catch(function (error) {
                button.disabled = false;
                button.textContent = "Mark read";
                showToast("Could not mark the notification as read.", "error");
                if (window.console && window.console.warn) {
                  window.console.warn("Failed to mark notification read:", error);
                }
              });
          });
        });
      })
      .catch(function (error) {
        widget.innerHTML =
          '<p class="empty-state">Notifications are unavailable right now. The notifications service did not answer.</p>';
        if (window.console && window.console.warn) {
          window.console.warn("Failed to load notifications:", error);
        }
      });
  }

  /* Command palette */

  var paletteOverlay = document.getElementById("command-palette");
  const paletteInput = document.querySelector("[data-palette-input]") as HTMLInputElement | null;
  var paletteList = document.querySelector("[data-palette-list]");

  function paletteItems(): PaletteItem[] {
    var items = [];
    document.querySelectorAll(".sidebar-link").forEach(function (link) {
      var label = (link.textContent || "").trim();
      if (label) {
        items.push({
          group: "Navigate",
          label: label,
          href: link.getAttribute("href"),
        });
      }
    });
    document.querySelectorAll(".site-nav .nav-link").forEach(function (link) {
      var label = (link.textContent || "").trim();
      if (label) {
        items.push({
          group: "Navigate",
          label: label,
          href: link.getAttribute("href"),
        });
      }
    });
    items.push(
      { group: "Actions", label: "Toggle dark / light theme", action: "theme" },
      { group: "Actions", label: "Go to dashboard", href: "/dashboard" },
      { group: "Actions", label: "Reload the module list", action: "reload-modules" }
    );
    // De-duplicate by label + href.
    var seen: Record<string, boolean> = {};
    return items.filter(function (item) {
      var key = item.label + "|" + (item.href || item.action || "");
      if (seen[key]) {
        return false;
      }
      seen[key] = true;
      return true;
    });
  }

  var paletteSelected = -1;

  function renderPalette(query?: string): void {
    if (!paletteList) {
      return;
    }
    var needle = (query || "").trim().toLowerCase();
    var matches = paletteItems().filter(function (item) {
      return needle === "" || item.label.toLowerCase().indexOf(needle) !== -1;
    });
    paletteSelected = matches.length > 0 ? 0 : -1;
    if (matches.length === 0) {
      paletteList.innerHTML =
        '<p class="palette-empty">No matching navigation item. The palette searches this page\u2019s navigation only.</p>';
      return;
    }
    var lastGroup: string | null = null;
    paletteList.innerHTML = matches
      .map(function (item, index) {
        var groupHtml =
          item.group !== lastGroup
            ? '<p class="palette-group">' + escapeHtml(item.group) + "</p>"
            : "";
        lastGroup = item.group;
        return (
          groupHtml +
          '<button type="button" class="palette-item' +
          (index === paletteSelected ? " is-selected" : "") +
          '" data-palette-index="' +
          index +
          '" data-palette-href="' +
          escapeHtml(item.href || "") +
          '" data-palette-action="' +
          escapeHtml(item.action || "") +
          '">' +
          icon(item.action ? "settings" : "grid") +
          "<span>" +
          escapeHtml(item.label) +
          "</span>" +
          '<span class="palette-item-hint">' +
          (item.href ? escapeHtml(item.href) : "Action") +
          "</span></button>"
        );
      })
      .join("");
    paletteList.querySelectorAll(".palette-item").forEach(function (button) {
      button.addEventListener("click", function () {
        runPaletteItem(button);
      });
    });
  }

  function runPaletteItem(button: Element): void {
    var action = button.getAttribute("data-palette-action");
    var href = button.getAttribute("data-palette-href");
    closePalette();
    if (action === "theme") {
      applyTheme(currentTheme() === "dark" ? "light" : "dark");
      return;
    }
    if (action === "reload-modules") {
      loadModules();
      showToast("Module list refreshed.", "success");
      return;
    }
    if (href) {
      window.location.href = href;
    }
  }

  function openPalette(): void {
    if (!paletteOverlay) {
      return;
    }
    paletteOverlay.hidden = false;
    document.body.style.overflow = "hidden";
    renderPalette("");
    if (paletteInput) {
      paletteInput.value = "";
      paletteInput.focus();
    }
  }

  function closePalette(): void {
    if (!paletteOverlay) {
      return;
    }
    paletteOverlay.hidden = true;
    document.body.style.overflow = "";
  }

  document.querySelectorAll("[data-palette-open]").forEach(function (button) {
    button.addEventListener("click", openPalette);
  });

  if (paletteOverlay) {
    paletteOverlay.addEventListener("click", function (event) {
      if (event.target === paletteOverlay) {
        closePalette();
      }
    });
    paletteOverlay.addEventListener("keydown", function (event) {
      if (event.key === "Escape") {
        closePalette();
      }
    });
  }

  if (paletteInput) {
    paletteInput.addEventListener("input", function () {
      renderPalette(paletteInput.value);
    });
    paletteInput.addEventListener("keydown", function (event) {
      var items = (paletteList ? paletteList.querySelectorAll(".palette-item") : []) as NodeListOf<Element>;
      if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        event.preventDefault();
        if (items.length === 0) {
          return;
        }
        paletteSelected =
          event.key === "ArrowDown"
            ? Math.min(paletteSelected + 1, items.length - 1)
            : Math.max(paletteSelected - 1, 0);
        items.forEach(function (item, index) {
          item.classList.toggle("is-selected", index === paletteSelected);
        });
        if (items[paletteSelected]) {
          items[paletteSelected].scrollIntoView({ block: "nearest" });
        }
      } else if (event.key === "Enter") {
        event.preventDefault();
        if (items[paletteSelected]) {
          runPaletteItem(items[paletteSelected]);
        }
      }
    });
  }

  document.addEventListener("keydown", function (event) {
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "k") {
      event.preventDefault();
      if (paletteOverlay && !paletteOverlay.hidden) {
        closePalette();
      } else {
        openPalette();
      }
    }
  });

  /* Dashboard section tables */
  // Section pages render their panel server-side (see the
  // `section_*` flags) and fill their table from the matching
  // dashboards API endpoint. The endpoints are the same ones the
  // API documents; a failed fetch leaves the honest loading note in
  // place instead of inventing rows.
  function renderTable(container: HTMLElement, headers: string[], rows: unknown[][]): void {
    if (!rows.length) {
      container.innerHTML = "<p class=\"empty-note\">Nothing here yet.</p>";
      return;
    }
    var html = "<div class=\"table-wrap\"><table class=\"data-table\"><thead><tr>";
    headers.forEach(function (header) {
      html += "<th scope=\"col\">" + escapeHtml(header) + "</th>";
    });
    html += "</tr></thead><tbody>";
    rows.forEach(function (row) {
      html += "<tr>";
      row.forEach(function (cell) {
        html += "<td>" + escapeHtml(cell == null ? "" : cell) + "</td>";
      });
      html += "</tr>";
    });
    html += "</tbody></table></div>";
    container.innerHTML = html;
  }

  function loadSectionTable<T>(containerId: string, url: string, headers: string[], mapRow: (item: T) => unknown[]): void {
    const container = document.getElementById(containerId);
    if (!container) {
      return;
    }
    fetchJson(url)
      .then(function (body: unknown) {
        var rows = itemsFromEnvelope<T>((body as ApiEnvelope | null)?.data);
        renderTable(container, headers, rows.map(mapRow));
      })
      .catch(function () {
        container.innerHTML =
          "<p class=\"empty-note\">Could not load this section right now.</p>";
      });
  }

  function loadDashboardSections(): void {
    loadSectionTable(
      "user-licenses-list",
      "/api/v1/dashboards/user/licenses?per_page=20",
      ["Product", "Status", "Expires", "Created"],
      function (item: LicenseItem) {
        return [item.product, item.status, item.expires_at || "-", item.created_at];
      }
    );
    loadSectionTable(
      "user-downloads-list",
      "/api/v1/dashboards/user/downloads?per_page=20",
      ["Software", "Downloaded"],
      function (item: DownloadItem) {
        return [item.software_slug || "-", item.downloaded_at];
      }
    );
    loadSectionTable(
      "user-orders-list",
      "/api/v1/dashboards/user/orders?per_page=20",
      ["Status", "Total", "Created"],
      function (item: OrderItem) {
        return [
          item.status,
          item.total + " " + (item.currency || ""),
          item.created_at,
        ];
      }
    );
    const modulesBox = document.getElementById("admin-modules-list");
    if (modulesBox) {
      fetchJson("/api/v1/dashboards/admin/modules")
        .then(function (body: unknown) {
          var data = ((body as ApiEnvelope<AdminModulesData> | null)?.data || {}) as AdminModulesData;
          var modules = Array.isArray(data.modules) ? data.modules : [];
          if (!modules.length) {
            modulesBox.innerHTML = "<p class=\"empty-note\">No modules registered.</p>";
            return;
          }
          var html =
            "<div class=\"table-wrap\"><table class=\"data-table\"><thead><tr>" +
            "<th scope=\"col\">Module</th><th scope=\"col\">Version</th>" +
            "<th scope=\"col\">Description</th></tr></thead><tbody>";
          modules.forEach(function (module: ModuleItem) {
            html +=
              "<tr><td>" +
              escapeHtml(module.name || module.id) +
              " (" +
              escapeHtml(module.id) +
              ")</td><td>" +
              escapeHtml(module.version || "") +
              "</td><td>" +
              escapeHtml(module.description || "") +
              "</td></tr>";
          });
          html += "</tbody></table></div>";
          if (data.dependencies_valid === false) {
            html +=
              "<p class=\"empty-note\">Dependency issues found; see the admin overview for details.</p>";
          }
          modulesBox.innerHTML = html;
        })
        .catch(function () {
          modulesBox.innerHTML =
            "<p class=\"empty-note\">Could not load modules right now.</p>";
        });
    }
    loadSectionTable(
      "admin-users-list",
      "/api/v1/dashboards/admin/users?per_page=20",
      ["Name", "Email", "Active", "Created"],
      function (item: UserItem) {
        return [
          item.name,
          item.email,
          item.is_active ? "Yes" : "No",
          item.created_at,
        ];
      }
    );
  }

  /* Boot */

  initTheme();
  loadModules();
  loadSystemStatus();
  loadTasks();
  loadNotifications();
  loadDashboardSections();

  // Small public surface for other dashboard scripts.
  (window as unknown as { UnifiedDashboard: UnifiedDashboardApi }).UnifiedDashboard = {
    applyTheme: applyTheme,
    showFlash: showFlash,
    showToast: showToast,
    openModal: openModal,
    closeModal: closeModal,
    openPalette: openPalette,
    closePalette: closePalette,
    loadModules: loadModules,
    loadSystemStatus: loadSystemStatus,
    loadTasks: loadTasks,
    loadNotifications: loadNotifications,
  };
})();
