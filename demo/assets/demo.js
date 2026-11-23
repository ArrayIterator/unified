/* ==========================================================================
   Unified demo — shared script (phase 1)
   --------------------------------------------------------------------------
   One source of truth for the shared shell: every page ships only its
   <main> content plus <body data-page="..." data-layout="app|auth"> and
   this script renders the topbar, sidebar, footer and toast around it.
   Layouts: "app" (topbar + sidebar + footer, signed-in shell),
   "public" (topbar + footer, no sidebar — public pages such as the home
   and tenant front pages), "auth" (centred, no sidebar).

   HOW TO ADD A PAGE (phase 2):
   1. Copy any existing page. Keep the <head> block verbatim (stylesheet,
      theme bootstrap snippet, sprite.js + this script at the end of body).
   2. Set <body data-page="your-page" data-layout="app"> (or "public" for a
      public page with no sidebar, "auth" for centred auth-style pages)
   3. If the page needs top-level navigation, add one entry to NAV below
      and, when it is a section of its own, to SIDEBAR_NAV. Do not copy
      the header/sidebar markup into the page.
   4. Use relative links only, kebab-case module ids for icons
      (<svg class="module-icon"><use href="#module-icon-billing"/></svg>),
      and English demo data. Forms get data-demo-form (no real submit).

   Conventions: theme choice persists in localStorage key THEME_KEY;
   icons come from assets/sprite.js (injected below, works from file://);
   every interactive control is demo state only and shows a toast.
   ========================================================================== */

(function () {
  "use strict";

  var THEME_KEY = "unified-demo-theme";

  /* ---------- Theme switcher (demo only) ---------- */

  function applyTheme(name) {
    var dark = name === "editorial";
    document.documentElement.classList.toggle("theme-dark", dark);
    try {
      localStorage.setItem(THEME_KEY, dark ? "editorial" : "default");
    } catch (e) {
      /* Storage unavailable (private mode) — theme still applies. */
    }
    syncThemeControls(dark ? "editorial" : "default");
  }

  function syncThemeControls(name) {
    var buttons = document.querySelectorAll("[data-theme-switch]");
    for (var i = 0; i < buttons.length; i++) {
      buttons[i].setAttribute(
        "aria-pressed",
        buttons[i].getAttribute("data-theme-switch") === name ? "true" : "false"
      );
    }
    var picks = document.querySelectorAll("[data-theme-pick]");
    for (var j = 0; j < picks.length; j++) {
      picks[j].setAttribute(
        "aria-pressed",
        picks[j].getAttribute("data-theme-pick") === name ? "true" : "false"
      );
    }
  }

  function currentTheme() {
    try {
      return localStorage.getItem(THEME_KEY) === "editorial" ? "editorial" : "default";
    } catch (e) {
      return "default";
    }
  }

  /* ---------- Shared navigation ---------- */

  /* Topbar entries. `page` matches <body data-page="..."> for the active state. */
  var NAV = [
    { page: "home", label: "Home", href: "index.html" },
    { page: "dashboard", label: "Dashboard", href: "dashboard.html" },
    { page: "tenant", label: "Tenant demo", href: "tenant.html" },
    { page: "modules", label: "Modules", href: "modules.html" },
    { page: "store", label: "Store", href: "store.html" },
    { page: "admin", label: "Admin", href: "admin.html" },
    { page: "settings", label: "Settings", href: "settings.html" }
  ];

  /* Sidebar module shortcuts (real module ids, kebab-case, matching the sprite). */
  var SIDEBAR_MODULES = [
    { id: "billing", label: "Billing", href: "module.html" },
    { id: "crm", label: "CRM", href: "modules.html" },
    { id: "content", label: "Content", href: "modules.html" },
    { id: "documents", label: "Documents", href: "modules.html" },
    { id: "licensing", label: "Licensing", href: "modules.html" },
    { id: "users", label: "Users", href: "modules.html" },
    { id: "security", label: "Security", href: "modules.html" },
    { id: "store", label: "Store", href: "store.html" }
  ];

  function icon(id, large) {
    return (
      '<svg class="module-icon' + (large ? " module-icon-lg" : "") +
      '" aria-hidden="true"><use href="#module-icon-' + id + '"></use></svg>'
    );
  }

  function renderTopbar(page, layout) {
    var nav = "";
    if (layout === "app" || layout === "public") {
      nav = '<nav class="topbar-nav" aria-label="Primary">' +
        NAV.map(function (item) {
          var active = page === item.page || (page === "module" && item.page === "modules");
          return '<a href="' + item.href + '"' +
            (active ? ' class="is-active" aria-current="page"' : "") +
            ">" + item.label + "</a>";
        }).join("") +
        "</nav>";
    }
    var right =
      '<div class="topbar-right">' +
      '<div class="theme-switcher" role="group" aria-label="Demo theme">' +
      '<button type="button" data-theme-switch="default" aria-pressed="true">Default</button>' +
      '<button type="button" data-theme-switch="editorial" aria-pressed="false">Editorial Dark</button>' +
      "</div>";
    if (layout === "app") {
      right +=
        '<button type="button" class="cart-button" data-cart-button aria-label="Demo cart">' +
        'Cart <span class="cart-count" id="cart-count">0</span></button>' +
        '<a class="user-chip" href="admin.html"><span class="avatar">DA</span><span>Demo Admin</span></a>';
    } else {
      right += '<a class="button button-small" href="login.html">Sign in</a>';
    }
    right += "</div>";
    return (
      '<header class="topbar">' +
      '<a class="brand" href="index.html"><span class="brand-mark" aria-hidden="true">U</span>' +
      '<span class="brand-name">Unified</span> <span class="demo-badge">Demo</span></a>' +
      nav + right +
      "</header>"
    );
  }

  function renderSidebar(page) {
    var links = NAV.map(function (item) {
      var active = page === item.page || (page === "module" && item.page === "modules");
      return '<a class="sidebar-link' + (active ? " is-active" : "") + '" href="' + item.href + '"' +
        (active ? ' aria-current="page"' : "") + ">" + item.label + "</a>";
    }).join("");
    var modules = SIDEBAR_MODULES.map(function (mod) {
      return '<a class="sidebar-link" href="' + mod.href + '">' + icon(mod.id) +
        "<span>" + mod.label + "</span></a>";
    }).join("");
    return (
      '<aside class="sidebar" aria-label="Demo navigation">' +
      '<p class="sidebar-section">Navigate</p>' + links +
      '<p class="sidebar-section">Modules</p>' + modules +
      "</aside>"
    );
  }

  function renderFooter() {
    return (
      '<footer class="footer">' +
      "<p><strong>Unified demo</strong> — static preview, no backend. " +
      "All data on this page is demo data; nothing is saved or sent anywhere.</p>" +
      '<p><a href="index.html">Home</a> &middot; <a href="dashboard.html">Dashboard</a> &middot; <a href="tenant.html">Tenant demo</a> &middot; <a href="modules.html">Modules</a> &middot; ' +
      '<a href="store.html">Store</a> &middot; <a href="settings.html">Settings</a></p>' +
      "</footer>"
    );
  }

  function renderShell() {
    /* Icon sprite (real sprite, injected so <use> works from file:// too). */
    if (window.UNIFIED_DEMO_SPRITE) {
      var sprite = document.createElement("div");
      sprite.setAttribute("aria-hidden", "true");
      sprite.style.display = "none";
      sprite.innerHTML = window.UNIFIED_DEMO_SPRITE;
      document.body.insertBefore(sprite, document.body.firstChild);
    }

    var main = document.querySelector("main");
    if (!main) return;
    var page = document.body.getAttribute("data-page") || "dashboard";
    var layout = document.body.getAttribute("data-layout") || "app";

    var header = document.createElement("div");
    header.innerHTML = renderTopbar(page, layout);
    document.body.insertBefore(header.firstChild, main);

    if (layout === "app") {
      var shell = document.createElement("div");
      shell.className = "shell";
      main.parentNode.insertBefore(shell, main);
      var aside = document.createElement("div");
      aside.innerHTML = renderSidebar(page);
      shell.appendChild(aside.firstChild);
      shell.appendChild(main);
    }

    var footer = document.createElement("div");
    footer.innerHTML = renderFooter();
    document.body.appendChild(footer.firstChild);

    var toast = document.createElement("div");
    toast.className = "toast";
    toast.id = "demo-toast";
    toast.setAttribute("role", "status");
    document.body.appendChild(toast);
  }

  /* ---------- Toast ---------- */

  var toastTimer = null;

  function showToast(message) {
    var toast = document.getElementById("demo-toast");
    if (!toast) return;
    toast.textContent = message;
    toast.classList.add("show");
    if (toastTimer) clearTimeout(toastTimer);
    toastTimer = setTimeout(function () {
      toast.classList.remove("show");
    }, 2600);
  }

  /* ---------- Demo interactions ---------- */

  function initTheme() {
    applyTheme(currentTheme());
    document.addEventListener("click", function (event) {
      var button = event.target.closest("[data-theme-switch], [data-theme-pick]");
      if (!button) return;
      applyTheme(button.getAttribute("data-theme-switch") || button.getAttribute("data-theme-pick"));
      showToast("Theme switched (demo only).");
    });
  }

  function initTicker() {
    var item = document.querySelector("[data-ticker-item]");
    var items = document.querySelectorAll("[data-ticker-source] li");
    if (!item || items.length === 0) return;
    var index = 0;
    setInterval(function () {
      index = (index + 1) % items.length;
      item.style.opacity = "0";
      setTimeout(function () {
        item.textContent = items[index].textContent;
        item.style.opacity = "1";
      }, 300);
    }, 5000);
  }

  function initModuleFilter() {
    var input = document.getElementById("module-search");
    if (!input) return;
    var cards = document.querySelectorAll("[data-module-name]");
    var empty = document.getElementById("module-empty");
    input.addEventListener("input", function () {
      var query = input.value.trim().toLowerCase();
      var visible = 0;
      cards.forEach(function (card) {
        var match = card.getAttribute("data-module-name").toLowerCase().indexOf(query) !== -1;
        card.style.display = match ? "" : "none";
        if (match) visible += 1;
      });
      if (empty) empty.hidden = visible !== 0;
    });
  }

  function initToggles() {
    document.addEventListener("click", function (event) {
      var toggle = event.target.closest(".switch");
      if (!toggle) return;
      var on = toggle.getAttribute("aria-pressed") !== "true";
      toggle.setAttribute("aria-pressed", on ? "true" : "false");
      showToast(toggle.getAttribute("data-label") + (on ? " enabled (demo)." : " disabled (demo)."));
    });
  }

  var cartCount = 0;

  function initCart() {
    document.addEventListener("click", function (event) {
      var button = event.target.closest("[data-add-cart]");
      if (!button) return;
      cartCount += 1;
      var badge = document.getElementById("cart-count");
      if (badge) badge.textContent = String(cartCount);
      showToast(button.getAttribute("data-add-cart") + " added to cart (demo).");
    });
    var cartButton = document.querySelector("[data-cart-button]");
    if (cartButton) {
      cartButton.addEventListener("click", function () {
        showToast(
          cartCount === 0
            ? "Your demo cart is empty."
            : "Demo cart: " + cartCount + " item" + (cartCount === 1 ? "" : "s") + " (no checkout in this demo)."
        );
      });
    }
  }

  function initTabs() {
    var buttons = document.querySelectorAll("[data-tab]");
    if (buttons.length === 0) return;
    buttons.forEach(function (button) {
      button.addEventListener("click", function () {
        var target = button.getAttribute("data-tab");
        buttons.forEach(function (other) {
          other.setAttribute("aria-selected", other === button ? "true" : "false");
        });
        document.querySelectorAll("[data-tab-panel]").forEach(function (panel) {
          panel.hidden = panel.getAttribute("data-tab-panel") !== target;
        });
      });
    });
  }

  function initForms() {
    document.querySelectorAll("form[data-demo-form]").forEach(function (form) {
      form.addEventListener("submit", function (event) {
        event.preventDefault();
        showToast("Demo only — nothing was saved.");
        var redirect = form.getAttribute("data-demo-redirect");
        if (redirect) {
          setTimeout(function () {
            window.location.href = redirect;
          }, 700);
        }
      });
    });
    var totpToggle = document.getElementById("login-show-totp");
    var totpField = document.getElementById("login-totp-field");
    if (totpToggle && totpField) {
      totpToggle.addEventListener("click", function () {
        var showing = totpField.hidden;
        totpField.hidden = !showing;
        totpToggle.textContent = showing ? "Hide authenticator code" : "Use an authenticator code";
        if (showing) document.getElementById("login-totp").focus();
      });
    }
  }

  /* Generic demo buttons: <button data-toast="message"> shows a toast. */
  function initToastButtons() {
    document.addEventListener("click", function (event) {
      var button = event.target.closest("[data-toast]");
      if (button) showToast(button.getAttribute("data-toast"));
    });
  }

  /* ---------- Boot ---------- */

  document.addEventListener("DOMContentLoaded", function () {
    renderShell();
    initTheme();
    initTicker();
    initModuleFilter();
    initToggles();
    initCart();
    initTabs();
    initToastButtons();
    initForms();
  });
})();
