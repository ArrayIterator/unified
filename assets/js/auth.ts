/* Auth pages script.
 *
 * TypeScript source, transpiled in-memory by the Rust server at
 * startup (no build step, no checked-in .js).
 *
 * The sign-in and registration pages are plain server-rendered forms;
 * this script posts them to the existing JSON APIs with fetch:
 *
 *   POST /api/v1/auth-manager/jwt/issue  (sign-in; returns a Bearer token)
 *   POST /api/v1/users                   (registration; needs users:manage)
 *
 * On a successful sign-in the issued token is stored in the session
 * cookie (name rendered into the form as `data-session-name`, the
 * `app.session_name` config value, default `vault`), because the
 * dashboard middleware authenticates page requests from that cookie.
 * The cookie is set without `HttpOnly` here because JavaScript is the
 * only writer available on this page; the dashboard page responses
 * set the same cookie with `HttpOnly` whenever a Bearer token is
 * presented, so a token obtained here is upgraded on the first
 * dashboard visit. Registration without the permission is reported
 * honestly instead of pretending the account was created.
 */
(function () {
  "use strict";

  type StatusKind = "info" | "success" | "warning" | "error";

  /** Error shape inside the standard `{"data": ...}` envelope. */
  interface ApiErrorBody {
    error?: {
      message?: string;
    };
  }

  /** Envelope returned by the JSON APIs: `{"data": ...}` on success. */
  interface ApiEnvelope<T> {
    data?: T;
    error?: {
      message?: string;
    };
  }

  interface PostResult<T> {
    status: number;
    ok: boolean;
    body: ApiEnvelope<T>;
  }

  interface LoginPayload {
    email: string;
    password: string;
    totp_code?: string;
    recovery_code?: string;
  }

  interface LoginResponse {
    token?: string;
  }

  interface RegisterPayload {
    name: string;
    email: string;
    password: string;
  }

  function errorMessage(body: unknown, fallback: string): string {
    if (body && typeof body === "object") {
      const message = (body as ApiErrorBody).error?.message;
      if (message) {
        return message;
      }
    }
    return fallback;
  }

  /* CSRF protection */
  // Cookie-authenticated mutations require a valid `X-CSRF-Token`
  // header: the server answers 403 without one. The token is bound
  // to the session, fetched from `GET /api/v1/security/csrf-token`,
  // and cached in memory; a failed fetch resolves to `null` and the
  // request goes out without the header. Sign-in and registration
  // are pre-auth calls, so they need no token - but when a session
  // already exists the token is attached anyway, covering the
  // cookie-authenticated auth writes.
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

  function showStatus(box: HTMLElement, message: string, kind?: StatusKind): void {
    box.textContent = message;
    box.className = "flash flash-" + (kind || "error");
    box.hidden = false;
  }

  function hideStatus(box: HTMLElement): void {
    box.hidden = true;
    box.textContent = "";
  }

  function postJson<T>(url: string, payload: unknown): Promise<PostResult<T>> {
    return fetchCsrfToken().then(function (token: string | null): Promise<PostResult<T>> {
      var headers: Record<string, string> = {
        "Content-Type": "application/json",
        Accept: "application/json",
      };
      if (token) {
        headers["X-CSRF-Token"] = token;
      }
      return window
        .fetch(url, {
          method: "POST",
          headers: headers,
          credentials: "same-origin",
          body: JSON.stringify(payload),
        })
        .then(function (response: Response) {
          return response.json().then(function (body: unknown) {
            // response.json() yields untyped JSON; the envelope shape is
            // asserted here and narrowed by callers before use.
            return { status: response.status, ok: response.ok, body: body as ApiEnvelope<T> };
          });
        });
    });
  }

  function setupLogin(form: HTMLFormElement): void {
    // The login template always renders every element looked up here.
    var statusBox = document.getElementById("login-status") as HTMLElement;
    var submit = document.getElementById("login-submit") as HTMLButtonElement;
    var totpField = document.getElementById("login-totp-field") as HTMLElement;
    var recoveryField = document.getElementById("login-recovery-field") as HTMLElement;
    var toggle = document.getElementById("login-show-totp") as HTMLElement;
    var totpInput = document.getElementById("login-totp") as HTMLInputElement;
    var recoveryInput = document.getElementById("login-recovery") as HTMLInputElement;
    var sessionName = form.getAttribute("data-session-name") || "vault";
    var dashboardUrl = form.getAttribute("data-dashboard-url") || "/dashboard";

    toggle.addEventListener("click", function () {
      var showTotp = totpField.hidden;
      totpField.hidden = !showTotp;
      recoveryField.hidden = showTotp;
      if (showTotp) {
        totpInput.focus();
        toggle.textContent = "Use a recovery code instead";
      } else {
        recoveryInput.focus();
        toggle.textContent = "Use an authenticator code";
      }
    });

    form.addEventListener("submit", function (event: SubmitEvent) {
      event.preventDefault();
      hideStatus(statusBox);
      var email = (document.getElementById("login-email") as HTMLInputElement).value.trim();
      var password = (document.getElementById("login-password") as HTMLInputElement).value;
      if (!email || !password) {
        showStatus(statusBox, "Email and password are both required.");
        return;
      }
      var payload: LoginPayload = { email: email, password: password };
      if (!totpField.hidden && totpInput.value.trim()) {
        payload.totp_code = totpInput.value.trim();
      }
      if (!recoveryField.hidden && recoveryInput.value.trim()) {
        payload.recovery_code = recoveryInput.value.trim();
      }
      submit.disabled = true;
      submit.textContent = "Signing in...";
      postJson<LoginResponse>("/api/v1/auth-manager/jwt/issue", payload)
        .then(function (result: PostResult<LoginResponse>) {
          if (!result.ok) {
            showStatus(
              statusBox,
              errorMessage(result.body, "Sign-in failed with HTTP " + result.status)
            );
            return;
          }
          var data = result.body.data || {};
          var token = data.token;
          if (!token) {
            showStatus(statusBox, "Sign-in succeeded but no token was returned.");
            return;
          }
          var secure = window.location.protocol === "https:" ? "; Secure" : "";
          document.cookie =
            sessionName + "=" + token + "; Path=/; SameSite=Lax" + secure;
          window.location.href = dashboardUrl;
        })
        .catch(function () {
          showStatus(statusBox, "Could not reach the server. Try again.");
        })
        .finally(function () {
          submit.disabled = false;
          submit.textContent = "Sign in";
        });
    });
  }

  function setupRegister(form: HTMLFormElement): void {
    // The register template always renders every element looked up here.
    var statusBox = document.getElementById("register-status") as HTMLElement;
    var submit = document.getElementById("register-submit") as HTMLButtonElement;
    var loginUrl = form.getAttribute("data-login-url") || "/login";

    form.addEventListener("submit", function (event: SubmitEvent) {
      event.preventDefault();
      hideStatus(statusBox);
      var name = (document.getElementById("register-name") as HTMLInputElement).value.trim();
      var email = (document.getElementById("register-email") as HTMLInputElement).value.trim();
      var password = (document.getElementById("register-password") as HTMLInputElement).value;
      if (!name || !email || !password) {
        showStatus(statusBox, "Name, email, and password are all required.");
        return;
      }
      if (password.length < 8) {
        showStatus(statusBox, "Password must be at least 8 characters.");
        return;
      }
      submit.disabled = true;
      submit.textContent = "Creating account...";
      var payload: RegisterPayload = { name: name, email: email, password: password };
      postJson<unknown>("/api/v1/users", payload)
        .then(function (result: PostResult<unknown>) {
          if (!result.ok) {
            if (result.status === 401 || result.status === 403) {
              showStatus(
                statusBox,
                "Registration is disabled for visitors: ask an administrator to create your account.",
                "warning"
              );
            } else {
              showStatus(
                statusBox,
                errorMessage(result.body, "Registration failed with HTTP " + result.status)
              );
            }
            return;
          }
          showStatus(statusBox, "Account created. You can now sign in.", "success");
          window.setTimeout(function () {
            window.location.href = loginUrl;
          }, 1200);
        })
        .catch(function () {
          showStatus(statusBox, "Could not reach the server. Try again.");
        })
        .finally(function () {
          submit.disabled = false;
          submit.textContent = "Create account";
        });
    });
  }

  document.addEventListener("DOMContentLoaded", function () {
    var loginForm = document.getElementById("login-form") as HTMLFormElement | null;
    if (loginForm) {
      setupLogin(loginForm);
    }
    var registerForm = document.getElementById("register-form") as HTMLFormElement | null;
    if (registerForm) {
      setupRegister(registerForm);
    }
  });
})();
