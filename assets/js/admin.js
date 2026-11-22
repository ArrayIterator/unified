/* Admin dashboard script.
 *
 * Small enhancements only: the theme toggle, sidebar drawer, and
 * collapse behaviour live in `dashboard.js` and are reused as-is.
 * The only fetch here is the view-as stop call, which maps to the
 * route this module registers
 * (`DELETE /api/v1/dashboards/admin/view-as`); authentication rides
 * on the session cookie set by the page response, so the call uses
 * `credentials: "same-origin"` and no token handling.
 */
(function () {
  "use strict";

  /* ---------- Navigation active state ---------- */
  // Server-rendered links already carry `is-active` / `aria-current`;
  // this only keeps the state correct when the browser moves between
  // admin sections without a full reload (same-document navigation).
  function markActiveLink() {
    var current = window.location.pathname.replace(/\/$/, "") || "/";
    document.querySelectorAll(".admin-nav .sidebar-link").forEach(function (link) {
      var href = link.getAttribute("href") || "";
      var normalized = href.replace(/\/$/, "") || "/";
      var active = normalized === current;
      link.classList.toggle("is-active", active);
      if (active) {
        link.setAttribute("aria-current", "page");
      } else {
        link.removeAttribute("aria-current");
      }
    });
  }

  /* ---------- View-as: stop the active session ---------- */
  function stopViewAs(button) {
    button.disabled = true;
    fetch("/api/v1/dashboards/admin/view-as", {
      method: "DELETE",
      credentials: "same-origin",
      headers: { Accept: "application/json" },
    })
      .then(function (response) {
        return response.json().then(function (body) {
          return { status: response.status, body: body };
        });
      })
      .then(function (result) {
        if (result.status >= 200 && result.status < 300) {
          window.location.reload();
          return;
        }
        var message =
          (result.body &&
            result.body.error &&
            result.body.error.message) ||
          "Could not stop the view-as session.";
        button.disabled = false;
        window.alert(message);
      })
      .catch(function () {
        button.disabled = false;
        window.alert("Could not stop the view-as session.");
      });
  }

  document.addEventListener("DOMContentLoaded", function () {
    markActiveLink();
    document.querySelectorAll("[data-view-as-stop]").forEach(function (button) {
      button.addEventListener("click", function () {
        stopViewAs(button);
      });
    });
  });
})();
