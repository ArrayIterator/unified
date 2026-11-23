/* Client (user / reseller) dashboard script.
 *
 * TypeScript source; transpiled in-memory by the Rust server at
 * startup (no build step). Small enhancements only: the theme toggle,
 * sidebar drawer, and collapse behaviour live in `dashboard.js` and
 * are reused as-is. The only fetch here is the view-as stop call,
 * which maps to the route this module registers
 * (`DELETE /api/v1/dashboards/admin/view-as`); authentication rides
 * on the session cookie set by the page response, so the call uses
 * `credentials: "same-origin"` and sends the `X-CSRF-Token` header
 * the server requires for cookie-authenticated mutations.
 */
(function (): void {
  "use strict";

  interface ApiErrorBody {
    error?: { message?: string };
  }

  /* ---------- CSRF protection ---------- */
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
        var envelope = body as ApiErrorBody | null;
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

  interface ViewAsResult {
    status: number;
    body: unknown;
  }

  /* ---------- Navigation active state ---------- */
  // Server-rendered links already carry `is-active` / `aria-current`;
  // this only keeps the state correct when the browser moves between
  // dashboard sections without a full reload (same-document navigation).
  function markActiveLink(): void {
    var current: string = window.location.pathname.replace(/\/$/, "") || "/";
    document.querySelectorAll(".client-nav .sidebar-link").forEach(function (link: Element): void {
      var href: string = link.getAttribute("href") || "";
      var normalized: string = href.replace(/\/$/, "") || "/";
      var active: boolean = normalized === current;
      link.classList.toggle("is-active", active);
      if (active) {
        link.setAttribute("aria-current", "page");
      } else {
        link.removeAttribute("aria-current");
      }
    });
  }

  /* ---------- View-as: stop the session an admin started ---------- */
  // The banner is shown to the impersonated user. The request carries
  // the admin's session cookie (set by the page response), and the
  // server checks the REAL admin identity, so this button only works
  // while a session exists for that admin.
  function stopViewAs(button: HTMLButtonElement): void {
    button.disabled = true;
    fetchWithCsrf("/api/v1/dashboards/admin/view-as", "DELETE")
      .then(function (response: Response): Promise<ViewAsResult> {
        return response.json().then(function (body: unknown): ViewAsResult {
          return { status: response.status, body: body };
        });
      })
      .then(function (result: ViewAsResult): void {
        if (result.status >= 200 && result.status < 300) {
          window.location.reload();
          return;
        }
        var errorBody = result.body as ApiErrorBody | null;
        var message: string =
          (errorBody && errorBody.error && errorBody.error.message) ||
          "Could not stop the view-as session.";
        button.disabled = false;
        window.alert(message);
      })
      .catch(function (): void {
        button.disabled = false;
        window.alert("Could not stop the view-as session.");
      });
  }

  document.addEventListener("DOMContentLoaded", function (): void {
    markActiveLink();
    // [data-view-as-stop] is rendered on <button> elements.
    document.querySelectorAll<HTMLButtonElement>("[data-view-as-stop]").forEach(function (button: HTMLButtonElement): void {
      button.addEventListener("click", function (): void {
        stopViewAs(button);
      });
    });
  });
})();
