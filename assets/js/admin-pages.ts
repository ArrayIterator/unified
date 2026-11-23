/* Admin pages script: email templates, settings, and themes.
 *
 * TypeScript source, transpiled in-memory by the Rust server at
 * startup (no build step, no checked-in .js).
 *
 * One script serves the three admin content templates that load it
 * (`admin-email-templates`, `admin-settings`, `admin-themes`). Each
 * section runs only when its container is on the page, and every call
 * goes to the API that module registers:
 *
 *   Email templates:
 *     GET  /api/v1/email-templates
 *     DELETE /api/v1/email-templates/{id}
 *     GET  /api/v1/email-templates/{id}/versions
 *     POST /api/v1/email-templates/{id}/restore/{version}
 *     POST /api/v1/email-templates/{id}/preview {variables}
 *     POST /api/v1/email-templates/{id}/test-send {to}
 *   Settings:
 *     GET /api/v1/settings?per_page=100
 *     PUT /api/v1/settings/{namespace}/{key} {value}
 *   Themes:
 *     GET  /api/v1/theme/themes
 *     GET  /api/v1/theme/active
 *     POST /api/v1/theme/themes/{id}/activate
 *
 * List responses arrive in the standard envelope; paginated lists
 * carry `items`, so both shapes are accepted.
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

  interface EmailTemplate {
    id: string;
    name: string;
    subject: string;
    category?: string | null;
    version: string;
  }

  interface TemplateVersion {
    version: string;
    subject: string;
    created_at: string;
  }

  interface TemplatePreview {
    rendered_html?: string;
  }

  interface TemplateVersionsResponse {
    versions?: TemplateVersion[];
  }

  interface SettingRecord {
    namespace: string;
    key: string;
    type: string;
    value: unknown;
  }

  interface Theme {
    id: string;
    name: string;
    version: string;
    description?: string | null;
    screenshot?: string | null;
  }

  interface ThemeListResponse {
    themes?: Theme[];
  }

  interface ActiveTheme {
    id?: string | null;
    missing?: boolean;
  }

  /** A list payload: either a bare array or a paginated `{items}` page. */
  type ListPayload<T> = T[] | { items: T[] };

  function escapeHtml(value: unknown): string {
    return String(value == null ? "" : value)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;")
      .replace(/'/g, "&#39;");
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

  function isCsrfError(body: unknown): boolean {
    var envelope = body as ApiErrorBody | null;
    var message = envelope && envelope.error ? envelope.error.message : null;
    return typeof message === "string" && message.indexOf("CSRF") !== -1;
  }

  function itemsFromEnvelope<T>(data: unknown): T[] {
    if (Array.isArray(data)) {
      return data as T[];
    }
    if (data && typeof data === "object" && Array.isArray((data as { items?: unknown }).items)) {
      return (data as { items: T[] }).items;
    }
    return [];
  }

  function request<T = unknown>(url: string, method?: string, payload?: unknown): Promise<T> {
    var verb = method || "GET";
    if (verb === "GET") {
      return requestOnce<T>(url, verb, payload, null, false);
    }
    return fetchCsrfToken().then(function (token: string | null): Promise<T> {
      return requestOnce<T>(url, verb, payload, token, false);
    });
  }

  function requestOnce<T>(
    url: string,
    method: string,
    payload: unknown,
    token: string | null,
    retried: boolean
  ): Promise<T> {
    var headers: Record<string, string> = {
      "Content-Type": "application/json",
      Accept: "application/json",
    };
    if (token) {
      headers["X-CSRF-Token"] = token;
    }
    return window
      .fetch(url, {
        method: method,
        headers: headers,
        credentials: "same-origin",
        body: payload === undefined ? undefined : JSON.stringify(payload),
      })
      .then(function (response: Response) {
        return response.json().then(function (body: unknown) {
          if (!response.ok) {
            if (response.status === 403 && token && !retried && isCsrfError(body)) {
              // Token may have expired: drop it and retry once with a fresh token.
              csrfToken = null;
              csrfTokenRequest = null;
              return fetchCsrfToken().then(function (fresh: string | null): Promise<T> {
                if (!fresh) {
                  throw new Error(
                    errorMessage(body, "Request failed with HTTP " + response.status)
                  );
                }
                return requestOnce<T>(url, method, payload, fresh, true);
              });
            }
            throw new Error(errorMessage(body, "Request failed with HTTP " + response.status));
          }
          // The standard envelope unwraps `data`; a bare body is used as-is.
          return (body && typeof body === "object" && "data" in body
            ? (body as { data: unknown }).data
            : body) as T;
        });
      });
  }

  function showStatus(box: HTMLElement | null, message: string, kind?: StatusKind): void {
    if (!box) {
      return;
    }
    box.textContent = message;
    box.className = "flash flash-" + (kind || "info");
    box.hidden = false;
  }

  /* ---------- Email templates ---------- */

  function setupEmailTemplates(): void {
    const list = document.getElementById("et-list");
    if (!list) {
      return;
    }
    var statusBox = document.getElementById("et-status");
    var versionsBox = document.getElementById("et-versions");
    var versionList = document.getElementById("et-version-list");
    var currentTemplate: string | null = null;

    const loadTemplates = function (): void {
      request<ListPayload<EmailTemplate>>("/api/v1/email-templates?per_page=50")
        .then(function (data: ListPayload<EmailTemplate>) {
          var templates = itemsFromEnvelope<EmailTemplate>(data);
          if (!templates.length) {
            list.innerHTML =
              '<tr><td colspan="5">No templates yet. Create the first one in the email builder.</td></tr>';
            return;
          }
          list.innerHTML = templates
            .map(function (template: EmailTemplate) {
              return (
                "<tr><td>" +
                escapeHtml(template.name) +
                "</td><td>" +
                escapeHtml(template.subject) +
                "</td><td>" +
                escapeHtml(template.category || "") +
                "</td><td>" +
                escapeHtml(template.version) +
                "</td><td>" +
                '<a class="button button-small" href="/assets/email-builder.html?id=' +
                encodeURIComponent(template.id) +
                '">Edit</a> ' +
                '<button class="button button-small button-secondary" type="button" data-et-preview="' +
                escapeHtml(template.id) +
                '">Preview</button> ' +
                '<button class="button button-small button-secondary" type="button" data-et-test="' +
                escapeHtml(template.id) +
                '">Test send</button> ' +
                '<button class="button button-small button-secondary" type="button" data-et-versions="' +
                escapeHtml(template.id) +
                '">Versions</button> ' +
                '<button class="button button-small button-danger" type="button" data-et-delete="' +
                escapeHtml(template.id) +
                '">Delete</button>' +
                "</td></tr>"
              );
            })
            .join("");
        })
        .catch(function (error: Error) {
          list.innerHTML =
            '<tr><td colspan="5">Could not load templates: ' +
            escapeHtml(error.message) +
            "</td></tr>";
        });
    };

    function openPreview(html: string | undefined): void {
      var win = window.open("", "_blank");
      if (!win) {
        showStatus(statusBox, "The preview was blocked by a pop-up blocker.", "warning");
        return;
      }
      win.document.open();
      win.document.write(html || "");
      win.document.close();
    }

    document.addEventListener("click", function (event: MouseEvent) {
      var target = (event.target as Element | null)?.closest<HTMLElement>(
        "[data-et-preview],[data-et-test],[data-et-versions],[data-et-delete],[data-et-restore]"
      );
      if (!target) {
        return;
      }
      if (target.hasAttribute("data-et-preview")) {
        // hasAttribute just confirmed the attribute is present.
        var previewId = target.getAttribute("data-et-preview")!;
        request<TemplatePreview>(
          "/api/v1/email-templates/" + encodeURIComponent(previewId) + "/preview",
          "POST",
          {
            variables: {},
          }
        )
          .then(function (data: TemplatePreview) {
            openPreview(data.rendered_html);
          })
          .catch(function (error: Error) {
            showStatus(statusBox, "Could not preview the template: " + error.message, "error");
          });
        return;
      }
      if (target.hasAttribute("data-et-test")) {
        var testId = target.getAttribute("data-et-test")!;
        const to = window.prompt("Send a test email to which address?");
        if (!to) {
          return;
        }
        request("/api/v1/email-templates/" + encodeURIComponent(testId) + "/test-send", "POST", {
          to: to.trim(),
        })
          .then(function () {
            showStatus(statusBox, "Test email sent to " + to.trim() + ".", "success");
          })
          .catch(function (error: Error) {
            showStatus(statusBox, "Could not send the test email: " + error.message, "error");
          });
        return;
      }
      if (target.hasAttribute("data-et-versions")) {
        var versionsId = target.getAttribute("data-et-versions")!;
        currentTemplate = versionsId;
        request<TemplateVersionsResponse>(
          "/api/v1/email-templates/" + encodeURIComponent(versionsId) + "/versions"
        )
          .then(function (data: TemplateVersionsResponse) {
            var versions = Array.isArray(data.versions) ? data.versions : [];
            if (versionsBox) {
              versionsBox.hidden = false;
            }
            if (!versions.length) {
              // The template always renders et-version-list with et-versions.
              versionList!.innerHTML =
                '<tr><td colspan="4">No earlier versions for this template.</td></tr>';
              return;
            }
            versionList!.innerHTML = versions
              .map(function (version: TemplateVersion) {
                return (
                  "<tr><td>" +
                  escapeHtml(version.version) +
                  "</td><td>" +
                  escapeHtml(version.subject) +
                  "</td><td>" +
                  escapeHtml(version.created_at) +
                  "</td><td>" +
                  '<button class="button button-small" type="button" data-et-restore="' +
                  escapeHtml(version.version) +
                  '">Restore</button>' +
                  "</td></tr>"
                );
              })
              .join("");
          })
          .catch(function (error: Error) {
            showStatus(statusBox, "Could not load versions: " + error.message, "error");
          });
        return;
      }
      if (target.hasAttribute("data-et-restore")) {
        if (!currentTemplate) {
          return;
        }
        var version = target.getAttribute("data-et-restore")!;
        request(
          "/api/v1/email-templates/" +
            encodeURIComponent(currentTemplate) +
            "/restore/" +
            encodeURIComponent(version),
          "POST"
        )
          .then(function () {
            showStatus(statusBox, "Version " + version + " restored.", "success");
            loadTemplates();
          })
          .catch(function (error: Error) {
            showStatus(statusBox, "Could not restore the version: " + error.message, "error");
          });
        return;
      }
      if (target.hasAttribute("data-et-delete")) {
        var deleteId = target.getAttribute("data-et-delete")!;
        if (!window.confirm("Delete this email template?")) {
          return;
        }
        request("/api/v1/email-templates/" + encodeURIComponent(deleteId), "DELETE")
          .then(function () {
            showStatus(statusBox, "Template deleted.", "success");
            loadTemplates();
          })
          .catch(function (error: Error) {
            showStatus(statusBox, "Could not delete the template: " + error.message, "error");
          });
      }
    });

    loadTemplates();
  }

  /* ---------- Settings ---------- */

  function setupSettings(): void {
    const container = document.getElementById("st-list");
    if (!container) {
      return;
    }
    var statusBox = document.getElementById("st-status");
    const namespaceSelect = document.getElementById("st-namespace") as HTMLSelectElement | null;
    var searchInput = document.getElementById("st-search") as HTMLInputElement | null;
    var filterForm = document.getElementById("st-filter") as HTMLFormElement | null;
    var records: SettingRecord[] = [];

    function valueInput(record: SettingRecord, index: number): string {
      var id = "st-value-" + index;
      if (record.type === "bool") {
        return (
          '<input id="' +
          id +
          '" type="checkbox" data-st-value ' +
          (record.value === true ? "checked" : "") +
          ">"
        );
      }
      if (record.type === "number") {
        return (
          '<input id="' +
          id +
          '" type="number" step="any" value="' +
          escapeHtml(record.value) +
          '" data-st-value>'
        );
      }
      if (record.type === "json") {
        return (
          '<textarea id="' +
          id +
          '" rows="2" data-st-value>' +
          escapeHtml(JSON.stringify(record.value)) +
          "</textarea>"
        );
      }
      return (
        '<input id="' +
        id +
        '" type="text" value="' +
        escapeHtml(record.value) +
        '" data-st-value>'
      );
    }

    const render = function (): void {
      var namespace = namespaceSelect ? namespaceSelect.value : "";
      var search = searchInput ? searchInput.value.trim().toLowerCase() : "";
      var filtered = records.filter(function (record: SettingRecord) {
        if (namespace && record.namespace !== namespace) {
          return false;
        }
        if (search) {
          var haystack = (record.namespace + " " + record.key + " " + JSON.stringify(record.value)).toLowerCase();
          if (haystack.indexOf(search) === -1) {
            return false;
          }
        }
        return true;
      });
      if (!filtered.length) {
        container.innerHTML = "<p class=\"empty-note\">No settings match.</p>";
        return;
      }
      var byNamespace: Record<string, SettingRecord[]> = {};
      filtered.forEach(function (record: SettingRecord) {
        (byNamespace[record.namespace] = byNamespace[record.namespace] || []).push(record);
      });
      var html = "";
      Object.keys(byNamespace)
        .sort()
        .forEach(function (name: string) {
          html += '<section class="panel section-panel" aria-label="Namespace ' + escapeHtml(name) + '">';
          html += "<h2>" + escapeHtml(name) + "</h2>";
          html += '<div class="table-wrap"><table class="data-table"><thead><tr><th scope="col">Key</th><th scope="col">Type</th><th scope="col">Value</th><th scope="col">Actions</th></tr></thead><tbody>';
          byNamespace[name].forEach(function (record: SettingRecord) {
            var index = records.indexOf(record);
            html +=
              "<tr><td>" +
              escapeHtml(record.key) +
              "</td><td>" +
              escapeHtml(record.type) +
              "</td><td>" +
              valueInput(record, index) +
              "</td><td>" +
              '<button class="button button-small" type="button" data-st-save="' +
              index +
              '">Save</button> ' +
              '<button class="button button-small button-danger" type="button" data-st-delete="' +
              index +
              '">Delete</button>' +
              "</td></tr>";
          });
          html += "</tbody></table></div></section>";
        });
      container.innerHTML = html;
    };

    function readValue(
      record: SettingRecord,
      input: HTMLInputElement | HTMLTextAreaElement
    ): unknown {
      if (record.type === "bool") {
        // Bool settings always render a checkbox input, never a textarea.
        return (input as HTMLInputElement).checked;
      }
      if (record.type === "number") {
        var number = Number(input.value);
        if (isNaN(number)) {
          throw new Error("Value must be a number.");
        }
        return number;
      }
      if (record.type === "json") {
        return JSON.parse(input.value);
      }
      return input.value;
    }

    document.addEventListener("click", function (event: MouseEvent) {
      var target = (event.target as Element | null)?.closest<HTMLElement>(
        "[data-st-save],[data-st-delete]"
      );
      if (!target) {
        return;
      }
      var index = Number(target.getAttribute("data-st-save") || target.getAttribute("data-st-delete"));
      var record = records[index];
      if (!record) {
        return;
      }
      var url =
        "/api/v1/settings/" +
        encodeURIComponent(record.namespace) +
        "/" +
        encodeURIComponent(record.key);
      if (target.hasAttribute("data-st-delete")) {
        if (!window.confirm("Delete the setting " + record.namespace + "." + record.key + "?")) {
          return;
        }
        request(url, "DELETE")
          .then(function () {
            showStatus(statusBox, "Setting deleted.", "success");
            records.splice(index, 1);
            render();
          })
          .catch(function (error: Error) {
            showStatus(statusBox, "Could not delete the setting: " + error.message, "error");
          });
        return;
      }
      var input = document.getElementById("st-value-" + index) as
        | HTMLInputElement
        | HTMLTextAreaElement
        | null;
      if (!input) {
        return;
      }
      var value: unknown = null;
      try {
        value = readValue(record, input);
      } catch (error) {
        // readValue only throws Error instances (validation / JSON.parse).
        showStatus(statusBox, error instanceof Error ? error.message : String(error), "error");
        return;
      }
      request(url, "PUT", { value: value, type: record.type })
        .then(function () {
          showStatus(
            statusBox,
            "Setting " + record.namespace + "." + record.key + " saved.",
            "success"
          );
          record.value = value;
        })
        .catch(function (error: Error) {
          showStatus(statusBox, "Could not save the setting: " + error.message, "error");
        });
    });

    if (filterForm) {
      filterForm.addEventListener("submit", function (event: SubmitEvent) {
        event.preventDefault();
        render();
      });
    }
    if (searchInput) {
      searchInput.addEventListener("input", render);
    }

    request<ListPayload<SettingRecord>>("/api/v1/settings?per_page=100")
      .then(function (data: ListPayload<SettingRecord>) {
        records = itemsFromEnvelope<SettingRecord>(data);
        var namespaces: string[] = [];
        records.forEach(function (record: SettingRecord) {
          if (namespaces.indexOf(record.namespace) === -1) {
            namespaces.push(record.namespace);
          }
        });
        namespaces.sort();
        if (namespaceSelect) {
          namespaces.forEach(function (name: string) {
            var option = document.createElement("option");
            option.value = name;
            option.textContent = name;
            namespaceSelect.appendChild(option);
          });
          namespaceSelect.addEventListener("change", render);
        }
        render();
      })
      .catch(function (error: Error) {
        container.innerHTML =
          "<p class=\"empty-note\">Could not load settings: " + escapeHtml(error.message) + "</p>";
      });
  }

  /* ---------- Themes ---------- */

  function setupThemes(): void {
    const list = document.getElementById("th-list");
    if (!list) {
      return;
    }
    var statusBox = document.getElementById("th-status");
    var activeBox = document.getElementById("th-active");
    var activeId: string | null = null;

    const loadThemes = function (): void {
      request<ThemeListResponse>("/api/v1/theme/themes")
        .then(function (data: ThemeListResponse) {
          var themes = Array.isArray(data.themes) ? data.themes : [];
          if (!themes.length) {
            list.innerHTML = '<tr><td colspan="5">No themes found.</td></tr>';
            return;
          }
          list.innerHTML = themes
            .map(function (theme: Theme) {
              var isActive = theme.id === activeId;
              return (
                "<tr><td>" +
                escapeHtml(theme.name) +
                " (" +
                escapeHtml(theme.id) +
                ")</td><td>" +
                escapeHtml(theme.version) +
                "</td><td>" +
                escapeHtml(theme.description || "") +
                "</td><td>" +
                (theme.screenshot ? "Yes" : "No") +
                "</td><td>" +
                (isActive
                  ? '<span class="section-status section-status-ok">Active</span>'
                  : '<button class="button button-small" type="button" data-th-activate="' +
                    escapeHtml(theme.id) +
                    '">Activate</button>') +
                "</td></tr>"
              );
            })
            .join("");
        })
        .catch(function (error: Error) {
          list.innerHTML =
            '<tr><td colspan="5">Could not load themes: ' + escapeHtml(error.message) + "</td></tr>";
        });
    };

    document.addEventListener("click", function (event: MouseEvent) {
      var target = (event.target as Element | null)?.closest<HTMLElement>("[data-th-activate]");
      if (!target) {
        return;
      }
      // hasAttribute is implied by the closest() selector match.
      var id = target.getAttribute("data-th-activate")!;
      request("/api/v1/theme/themes/" + encodeURIComponent(id) + "/activate", "POST")
        .then(function () {
          activeId = id;
          showStatus(statusBox, 'Theme "' + id + '" activated.', "success");
          if (activeBox) {
            activeBox.textContent = "Active theme: " + id;
          }
          loadThemes();
        })
        .catch(function (error: Error) {
          showStatus(statusBox, "Could not activate the theme: " + error.message, "error");
        });
    });

    request<ActiveTheme>("/api/v1/theme/active")
      .then(function (data: ActiveTheme) {
        activeId = data.id || null;
        if (activeBox) {
          activeBox.textContent = data.missing
            ? "Active theme: " + activeId + " (missing; the embedded default theme is rendering instead)"
            : "Active theme: " + activeId;
        }
        loadThemes();
      })
      .catch(function () {
        if (activeBox) {
          activeBox.textContent = "Could not load the active theme.";
        }
        loadThemes();
      });
  }

  document.addEventListener("DOMContentLoaded", function () {
    setupEmailTemplates();
    setupSettings();
    setupThemes();
  });
})();
