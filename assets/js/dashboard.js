/* Unified dashboard behaviour.
 *
 * Vanilla JavaScript, no build step and no framework:
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

  var THEME_KEY = "unified-theme";
  var SIDEBAR_KEY = "unified-sidebar-collapsed";
  var root = document.documentElement;

  /* ---------- Small helpers ---------- */

  function escapeHtml(value) {
    return String(value)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;")
      .replace(/'/g, "&#39;");
  }

  function itemsFromEnvelope(data) {
    if (Array.isArray(data)) {
      return data;
    }
    if (data && Array.isArray(data.items)) {
      return data.items;
    }
    return [];
  }

  function fetchJson(url) {
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

  function formatBytes(bytes) {
    var value = Number(bytes) || 0;
    var units = ["B", "KB", "MB", "GB", "TB"];
    var index = 0;
    while (value >= 1024 && index < units.length - 1) {
      value /= 1024;
      index += 1;
    }
    return (value >= 100 || index === 0 ? Math.round(value) : value.toFixed(1)) + " " + units[index];
  }

  function formatUptime(totalSeconds) {
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

  function formatRelativeTime(isoValue) {
    var date = new Date(isoValue);
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

  function icon(name, extraClass) {
    return (
      '<svg class="icon' +
      (extraClass ? " " + extraClass : "") +
      '" aria-hidden="true"><use href="#icon-' +
      name +
      '"></use></svg>'
    );
  }

  /* ---------- Theme: dark / light ---------- */

  function storedTheme() {
    try {
      var value = window.localStorage.getItem(THEME_KEY);
      return value === "dark" || value === "light" ? value : null;
    } catch (error) {
      return null;
    }
  }

  function systemTheme() {
    return window.matchMedia &&
      window.matchMedia("(prefers-color-scheme: dark)").matches
      ? "dark"
      : "light";
  }

  function currentTheme() {
    return root.getAttribute("data-theme") === "dark" ? "dark" : "light";
  }

  function applyTheme(theme, persist) {
    var value = theme === "dark" ? "dark" : "light";
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

  function updateToggleLabels(theme) {
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

  function initTheme() {
    var stored = storedTheme();
    if (stored) {
      applyTheme(stored, false);
      return;
    }
    applyTheme(systemTheme(), false);
    // Follow the operating system while the user has not chosen.
    if (window.matchMedia) {
      var query = window.matchMedia("(prefers-color-scheme: dark)");
      var onSystemChange = function (event) {
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

  /* ---------- Sidebar: drawer (small screens) + collapse (desktop) ---------- */

  var shell = document.querySelector(".dashboard-shell");
  var backdrop = document.querySelector("[data-sidebar-backdrop]");

  function setSidebarOpen(open) {
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

  function setSidebarCollapsed(collapsed) {
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

  /* ---------- Flash messages ---------- */

  document.querySelectorAll("[data-flash]").forEach(function (flash) {
    window.setTimeout(function () {
      flash.remove();
    }, 6000);
  });

  function showFlash(message, type) {
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

  /* ---------- Toasts ---------- */

  function toastRegion() {
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

  function showToast(message, type) {
    var toast = document.createElement("div");
    toast.className = "toast toast-" + (type || "info");
    toast.setAttribute("role", "status");
    var iconName =
      type === "success" ? "check-circle" : type === "error" ? "alert" : "info";
    toast.innerHTML =
      icon(iconName) +
      '<span class="toast-message"></span>' +
      '<button type="button" class="toast-close" aria-label="Dismiss notification">&times;</button>';
    toast.querySelector(".toast-message").textContent = message;
    toast.querySelector(".toast-close").addEventListener("click", function () {
      toast.remove();
    });
    toastRegion().appendChild(toast);
    window.setTimeout(function () {
      toast.remove();
    }, 6000);
    return toast;
  }

  /* ---------- Dropdown menus ---------- */

  document.querySelectorAll("[data-menu-toggle]").forEach(function (toggle) {
    var menu = document.getElementById(toggle.getAttribute("aria-controls"));
    if (!menu) {
      return;
    }
    toggle.addEventListener("click", function (event) {
      event.stopPropagation();
      var open = menu.hidden;
      document.querySelectorAll(".menu").forEach(function (other) {
        other.hidden = true;
      });
      document.querySelectorAll("[data-menu-toggle]").forEach(function (other) {
        other.setAttribute("aria-expanded", "false");
      });
      menu.hidden = !open;
      toggle.setAttribute("aria-expanded", open ? "true" : "false");
      if (open) {
        var first = menu.querySelector(".menu-item");
        if (first) {
          first.focus();
        }
      }
    });
  });

  document.addEventListener("click", function (event) {
    if (!event.target.closest(".menu-wrap")) {
      document.querySelectorAll(".menu").forEach(function (menu) {
        menu.hidden = true;
      });
      document.querySelectorAll("[data-menu-toggle]").forEach(function (toggle) {
        toggle.setAttribute("aria-expanded", "false");
      });
    }
  });

  document.addEventListener("keydown", function (event) {
    if (event.key === "Escape") {
      document.querySelectorAll(".menu").forEach(function (menu) {
        menu.hidden = true;
      });
      document.querySelectorAll("[data-menu-toggle]").forEach(function (toggle) {
        toggle.setAttribute("aria-expanded", "false");
      });
    }
  });

  /* ---------- Tabs ---------- */

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
        document.querySelectorAll("[data-tab-panel]").forEach(function (panel) {
          panel.hidden = panel.id !== targetId;
        });
      });
    });
  });

  /* ---------- Modals (Esc, overlay click, focus return) ---------- */

  var lastModalOpener = null;

  function focusableIn(element) {
    return Array.prototype.slice
      .call(
        element.querySelectorAll(
          'a[href], button:not([disabled]), input, select, textarea, [tabindex]:not([tabindex="-1"])'
        )
      )
      .filter(function (node) {
        return node.offsetParent !== null;
      });
  }

  function openModal(overlay) {
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

  function closeModal(overlay) {
    if (!overlay) {
      return;
    }
    overlay.hidden = true;
    document.body.style.overflow = "";
    if (lastModalOpener && typeof lastModalOpener.focus === "function") {
      lastModalOpener.focus();
      lastModalOpener = null;
    }
  }

  document.querySelectorAll("[data-modal-open]").forEach(function (button) {
    button.addEventListener("click", function () {
      openModal(document.getElementById(button.getAttribute("data-modal-open")));
    });
  });

  document.querySelectorAll("[data-modal]").forEach(function (overlay) {
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

  /* ---------- Module list from /api/v1/extensions ---------- */

  function modulesFromEnvelope(data) {
    return itemsFromEnvelope(data);
  }

  function renderModuleNav(modules) {
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
          icon("layers") +
          '<span class="sidebar-label">' +
          escapeHtml(module.name || module.id) +
          "</span></a>"
        );
      })
      .join("");
  }

  function renderModuleGrid(modules) {
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
          icon("layers", "icon-lg") +
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

  function setModuleStatus(message) {
    var status = document.querySelector("[data-module-status]");
    if (status) {
      status.textContent = message;
    }
  }

  function loadModules() {
    var grid = document.getElementById("module-grid");
    if (!grid) {
      return;
    }
    // Only replace the server-rendered list when the API answers;
    // a failed fetch leaves the server-rendered content in place.
    fetchJson("/api/v1/extensions")
      .then(function (envelope) {
        var modules = modulesFromEnvelope(envelope && envelope.data);
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

  /* ---------- Dashboard search (filters the module grid) ---------- */

  var searchInput = document.querySelector("[data-dashboard-search]");
  if (searchInput) {
    searchInput.addEventListener("input", function () {
      var query = searchInput.value.trim().toLowerCase();
      document.querySelectorAll("#module-grid .module-card").forEach(function (card) {
        var text = card.textContent.toLowerCase();
        card.style.display =
          query === "" || text.indexOf(query) !== -1 ? "" : "none";
      });
    });
  }

  /* ---------- System status widget: GET /api/v1/service-monitor/status ---------- */

  function statusBadge(status) {
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

  function loadSystemStatus() {
    var widget = document.querySelector('[data-widget="system-status"]');
    if (!widget) {
      return;
    }
    fetchJson("/api/v1/service-monitor/status")
      .then(function (envelope) {
        var data = (envelope && envelope.data) || {};
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

  /* ---------- Tasks widget: GET /api/v1/tasks ---------- */

  var TASK_STATUS_LABELS = {
    todo: "To do",
    in_progress: "In progress",
    review: "In review",
    done: "Done",
    blocked: "Blocked",
    cancelled: "Cancelled",
  };

  function taskStatusBadge(status) {
    var label = TASK_STATUS_LABELS[status] || status || "Unknown";
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

  function taskPriorityBadge(priority) {
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

  function loadTasks() {
    var widget = document.querySelector('[data-widget="tasks"]');
    if (!widget) {
      return;
    }
    fetchJson("/api/v1/tasks?per_page=8&sort=due_date&order=asc")
      .then(function (envelope) {
        var tasks = itemsFromEnvelope(envelope && envelope.data)
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

  /* ---------- Notifications widget: GET /api/v1/notifications ---------- */

  function notificationKindBadge(kind) {
    var map = {
      info: "badge-info",
      warning: "badge-warning",
      error: "badge-danger",
      success: "badge-success",
    };
    var kindClass = map[kind] || "badge-neutral";
    return '<span class="badge ' + kindClass + '">' + escapeHtml(kind || "info") + "</span>";
  }

  function loadNotifications() {
    var widget = document.querySelector('[data-widget="notifications"]');
    if (!widget) {
      return;
    }
    fetchJson("/api/v1/notifications?per_page=8")
      .then(function (envelope) {
        var allNotifications = itemsFromEnvelope(envelope && envelope.data);
        var unread = allNotifications.filter(function (item) {
          return !item.read;
        }).length;
        var notifications = allNotifications.slice(0, 5);
        var bellBadge = document.querySelector("[data-notification-count]");
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
        widget.querySelectorAll("[data-mark-read]").forEach(function (button) {
          button.addEventListener("click", function () {
            var id = button.getAttribute("data-mark-read");
            button.disabled = true;
            button.textContent = "Saving…";
            window
              .fetch("/api/v1/notifications/" + encodeURIComponent(id) + "/read", {
                method: "POST",
                headers: { Accept: "application/json" },
                credentials: "same-origin",
              })
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

  /* ---------- Command palette ---------- */

  var paletteOverlay = document.getElementById("command-palette");
  var paletteInput = document.querySelector("[data-palette-input]");
  var paletteList = document.querySelector("[data-palette-list]");

  function paletteItems() {
    var items = [];
    document.querySelectorAll(".sidebar-link").forEach(function (link) {
      var label = link.textContent.trim();
      if (label) {
        items.push({
          group: "Navigate",
          label: label,
          href: link.getAttribute("href"),
        });
      }
    });
    document.querySelectorAll(".site-nav .nav-link").forEach(function (link) {
      var label = link.textContent.trim();
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
    var seen = {};
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

  function renderPalette(query) {
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
    var lastGroup = null;
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

  function runPaletteItem(button) {
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

  function openPalette() {
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

  function closePalette() {
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
      var items = paletteList ? paletteList.querySelectorAll(".palette-item") : [];
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

  /* ---------- Boot ---------- */

  initTheme();
  loadModules();
  loadSystemStatus();
  loadTasks();
  loadNotifications();

  // Small public surface for other dashboard scripts.
  window.UnifiedDashboard = {
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
