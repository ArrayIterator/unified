/* Unified dashboard behaviour.
 *
 * Vanilla JavaScript, no build step and no framework:
 *   - dark / light theme toggle, persisted in localStorage;
 *   - sidebar open / close on small screens;
 *   - module list rendered from the Extensions API
 *     (`GET /api/v1/extensions`), which returns the standard envelope
 *     `{"data": ...}` (a plain array, or a paginated `{"items": [...]}`);
 *   - flash messages shown for a few seconds, then dismissed.
 */

(function () {
  "use strict";

  var THEME_KEY = "unified-theme";
  var root = document.documentElement;

  /* ---------- Theme: dark / light ---------- */

  function currentTheme() {
    return root.getAttribute("data-theme") === "dark" ? "dark" : "light";
  }

  function applyTheme(theme) {
    var value = theme === "dark" ? "dark" : "light";
    root.setAttribute("data-theme", value);
    root.classList.remove("theme-light", "theme-dark");
    root.classList.add("theme-" + value);
    try {
      window.localStorage.setItem(THEME_KEY, value);
    } catch (error) {
      // Private browsing or blocked storage: the theme still applies
      // for this page load.
    }
    updateToggleLabels(value);
  }

  function updateToggleLabels(theme) {
    var toggles = document.querySelectorAll("[data-theme-toggle]");
    toggles.forEach(function (button) {
      button.setAttribute(
        "aria-label",
        theme === "dark" ? "Switch to light theme" : "Switch to dark theme"
      );
    });
  }

  function initTheme() {
    var stored = null;
    try {
      stored = window.localStorage.getItem(THEME_KEY);
    } catch (error) {
      stored = null;
    }
    if (stored === "dark" || stored === "light") {
      applyTheme(stored);
      return;
    }
    if (
      window.matchMedia &&
      window.matchMedia("(prefers-color-scheme: dark)").matches
    ) {
      applyTheme("dark");
    } else {
      applyTheme(currentTheme());
    }
  }

  document.querySelectorAll("[data-theme-toggle]").forEach(function (button) {
    button.addEventListener("click", function () {
      applyTheme(currentTheme() === "dark" ? "light" : "dark");
    });
  });

  /* ---------- Sidebar toggle (small screens) ---------- */

  var shell = document.querySelector(".dashboard-shell");
  document.querySelectorAll("[data-sidebar-toggle]").forEach(function (button) {
    button.addEventListener("click", function () {
      if (shell) {
        shell.classList.toggle("sidebar-open");
      }
    });
  });

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

  /* ---------- Module list from /api/v1/extensions ---------- */

  function escapeHtml(value) {
    return String(value)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;")
      .replace(/'/g, "&#39;");
  }

  function modulesFromEnvelope(data) {
    if (Array.isArray(data)) {
      return data;
    }
    if (data && Array.isArray(data.items)) {
      return data.items;
    }
    return [];
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
          escapeHtml(module.name || module.id) +
          "</a>"
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
          ? '<span class="badge badge-enabled">Enabled</span>'
          : '<span class="badge badge-disabled">Disabled</span>';
        var version = module.version
          ? "<span>v" + escapeHtml(module.version) + "</span>"
          : "";
        return (
          '<article class="module-card">' +
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
    window
      .fetch("/api/v1/extensions", {
        headers: { Accept: "application/json" },
        credentials: "same-origin",
      })
      .then(function (response) {
        if (!response.ok) {
          throw new Error("Extensions API returned HTTP " + response.status);
        }
        return response.json();
      })
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

  /* ---------- Dashboard search ---------- */

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

  initTheme();
  loadModules();

  // Small public surface for other dashboard scripts.
  window.UnifiedDashboard = {
    applyTheme: applyTheme,
    showFlash: showFlash,
    loadModules: loadModules,
  };
})();
