/* Shared media picker.
 *
 * TypeScript source, transpiled in-memory by the Rust server at startup.
 * One picker over the shared documents store - never a per-module
 * upload silo. The list tab shows PUBLIC files only
 * (`GET /api/v1/documents/picker` filters `visibility = 'public'`
 * server-side); a private file is invisible here until a manager or
 * admin flips it public. The upload tab stores into the same shared
 * store (`POST /api/v1/documents/upload`, multipart); an upload
 * starts private unless the caller asks for public, and the server
 * only honours a public request when the virus scan is clean.
 *
 * Usage from any edit form (posts, pages, digital products,
 * e-learning courses, ...):
 *
 *   window.UnifiedMediaPicker.open({
 *     moduleId: "content",
 *     onAttach: function (item) {
 *       // item.url is the /files/public/{key} URL to store on the
 *       // record's attachment reference; item.document_id is the
 *       // documents-store id; item.name is the original filename
 *       // from the database. The description lives with the
 *       // attachment reference on the record, never on the document.
 *     },
 *   });
 *
 * Authentication rides on the session cookie set by the page
 * response, so calls use `credentials: "same-origin"` and send the
 * `X-CSRF-Token` header the server requires for cookie-authenticated
 * mutations (same helper shape as the other dashboard scripts).
 */
(function () {
  "use strict";

  /* Types (compile-time only; erased by the transpiler) */

  interface ApiErrorBody {
    error?: {
      message?: string;
    };
  }

  /** One row of `GET /api/v1/documents/picker`. */
  interface PickerItem {
    document_id: string;
    name: string;
    mime: string;
    size: number;
    storage_key: string;
    url: string;
    visibility: string;
    created_at: string;
  }

  interface PickerPage {
    items: PickerItem[];
    page: number;
    per_page: number;
    total: number;
    total_pages: number;
  }

  interface MediaPickerOptions {
    /** Called once per attached file with the picked document. */
    onAttach: (item: PickerItem) => void;
    /** Module the upload tab files new uploads under. Defaults to "content". */
    moduleId?: string;
    /** Exact MIME filter for the list tab (for example "image/png"). */
    mime?: string;
    /** Allow attaching several files without closing the picker. Defaults to false. */
    multiple?: boolean;
  }

  interface UnifiedMediaPickerApi {
    open: (options: MediaPickerOptions) => void;
    close: () => void;
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
    body: BodyInit | null,
    token: string | null,
    retried: boolean
  ): Promise<Response> {
    var headers: Record<string, string> = { Accept: "application/json" };
    if (token) {
      headers["X-CSRF-Token"] = token;
    }
    return window
      .fetch(url, {
        method: method,
        headers: headers,
        body: body,
        credentials: "same-origin",
      })
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
            return sendWithCsrf(url, method, body, fresh, true);
          });
        });
      });
  }

  // One mutating request with the CSRF header attached; retries
  // once with a fresh token when the server says the token expired.
  function fetchWithCsrf(url: string, method: string, body: BodyInit | null): Promise<Response> {
    return fetchCsrfToken().then(function (token: string | null): Promise<Response> {
      return sendWithCsrf(url, method, body, token, false);
    });
  }

  /* Helpers */

  function apiErrorMessage(body: unknown, fallback: string): string {
    var envelope = body as ApiErrorBody | null;
    var message = envelope && envelope.error ? envelope.error.message : null;
    return typeof message === "string" && message ? message : fallback;
  }

  function formatSize(bytes: number): string {
    if (bytes < 1024) {
      return bytes + " B";
    }
    if (bytes < 1024 * 1024) {
      return (bytes / 1024).toFixed(1) + " KB";
    }
    return (bytes / (1024 * 1024)).toFixed(1) + " MB";
  }

  function escapeHtml(value: string): string {
    return value
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;");
  }

  /* Picker dialog */

  var overlay: HTMLDivElement | null = null;
  var currentOptions: MediaPickerOptions | null = null;
  var currentPage = 1;
  var currentSearch = "";

  function closePicker(): void {
    if (overlay && overlay.parentNode) {
      overlay.parentNode.removeChild(overlay);
    }
    overlay = null;
    currentOptions = null;
    document.removeEventListener("keydown", onKeyDown, true);
  }

  function onKeyDown(event: KeyboardEvent): void {
    if (event.key === "Escape") {
      closePicker();
    }
  }

  function setStatus(message: string): void {
    var status = overlay ? overlay.querySelector<HTMLElement>("[data-picker-status]") : null;
    if (status) {
      status.textContent = message;
    }
  }

  function pickerQuery(page: number): string {
    var params = new URLSearchParams();
    params.set("page", String(page));
    params.set("per_page", "20");
    if (currentSearch) {
      params.set("search", currentSearch);
    }
    if (currentOptions && currentOptions.mime) {
      params.set("mime", currentOptions.mime);
    }
    return "/api/v1/documents/picker?" + params.toString();
  }

  function loadPage(page: number): void {
    var list = overlay ? overlay.querySelector<HTMLElement>("[data-picker-list]") : null;
    if (!overlay || !list) {
      return;
    }
    // Narrowed once here so the fetch callbacks below (which TS
    // cannot narrow across) keep a non-null element.
    var listEl: HTMLElement = list;
    currentPage = page;
    listEl.innerHTML = "<p>Loading...</p>";
    setStatus("");
    window
      .fetch(pickerQuery(page), {
        headers: { Accept: "application/json" },
        credentials: "same-origin",
      })
      .then(function (response: Response): Promise<{ status: number; body: unknown }> {
        return response.json().then(function (body: unknown) {
          return { status: response.status, body: body };
        });
      })
      .then(function (result: { status: number; body: unknown }) {
        if (result.status < 200 || result.status >= 300) {
          setStatus(apiErrorMessage(result.body, "Could not load the media library."));
          listEl.innerHTML = "";
          return;
        }
        var envelope = result.body as { data?: PickerPage } | null;
        var data = envelope && envelope.data ? envelope.data : null;
        if (!data || !data.items) {
          setStatus("Could not load the media library.");
          listEl.innerHTML = "";
          return;
        }
        renderItems(listEl, data);
      })
      .catch(function () {
        setStatus("Could not load the media library.");
        listEl.innerHTML = "";
      });
  }

  function renderItems(list: HTMLElement, data: PickerPage): void {
    if (data.items.length === 0) {
      list.innerHTML =
        "<p class=\"empty-state\">No public files yet. Upload one below - " +
        "it starts private and only appears here once it is public.</p>";
      renderPager(data);
      return;
    }
    var rows = data.items
      .map(function (item) {
        var preview =
          item.mime.indexOf("image/") === 0
            ? "<img src=\"" +
              escapeHtml(item.url) +
              "\" alt=\"\" loading=\"lazy\" style=\"max-width: 64px; max-height: 64px; object-fit: cover;\">"
            : "<span>" + escapeHtml(item.mime) + "</span>";
        return (
          "<tr>" +
          "<td>" + preview + "</td>" +
          "<td>" + escapeHtml(item.name) + "</td>" +
          "<td>" + formatSize(item.size) + "</td>" +
          "<td><button type=\"button\" class=\"button\" data-attach=\"" +
          escapeHtml(item.document_id) +
          "\">Attach</button></td>" +
          "</tr>"
        );
      })
      .join("");
    list.innerHTML =
      "<table class=\"table\"><thead><tr>" +
      "<th scope=\"col\">Preview</th><th scope=\"col\">Name</th>" +
      "<th scope=\"col\">Size</th><th scope=\"col\"></th>" +
      "</tr></thead><tbody>" + rows + "</tbody></table>";
    list.querySelectorAll<HTMLButtonElement>("[data-attach]").forEach(function (button) {
      button.addEventListener("click", function () {
        var id = button.getAttribute("data-attach") || "";
        var item: PickerItem | null = null;
        for (var i = 0; i < data.items.length; i++) {
          if (data.items[i].document_id === id) {
            item = data.items[i];
            break;
          }
        }
        if (!item || !currentOptions) {
          return;
        }
        currentOptions.onAttach(item);
        if (!currentOptions.multiple) {
          closePicker();
        }
      });
    });
    renderPager(data);
  }

  function renderPager(data: PickerPage): void {
    var pager = overlay ? overlay.querySelector<HTMLElement>("[data-picker-pager]") : null;
    if (!pager) {
      return;
    }
    pager.innerHTML = "";
    if (data.total_pages <= 1) {
      return;
    }
    var info = document.createElement("span");
    info.textContent = "Page " + data.page + " of " + data.total_pages + " (" + data.total + " total)";
    pager.appendChild(info);
    if (data.page > 1) {
      var prev = document.createElement("button");
      prev.type = "button";
      prev.className = "button";
      prev.textContent = "Previous";
      prev.addEventListener("click", function () {
        loadPage(data.page - 1);
      });
      pager.appendChild(prev);
    }
    if (data.page < data.total_pages) {
      var next = document.createElement("button");
      next.type = "button";
      next.className = "button";
      next.textContent = "Next";
      next.addEventListener("click", function () {
        loadPage(data.page + 1);
      });
      pager.appendChild(next);
    }
  }

  function uploadFile(file: File, visibility: string): void {
    if (!currentOptions) {
      return;
    }
    setStatus("Uploading " + file.name + "...");
    var form = new FormData();
    form.append("module_id", currentOptions.moduleId || "content");
    form.append("scope", "owner");
    form.append("visibility", visibility);
    form.append("file", file, file.name);
    fetchWithCsrf("/api/v1/documents/upload", "POST", form)
      .then(function (response: Response): Promise<{ status: number; body: unknown }> {
        return response.json().then(function (body: unknown) {
          return { status: response.status, body: body };
        });
      })
      .then(function (result: { status: number; body: unknown }) {
        if (result.status < 200 || result.status >= 300) {
          setStatus(apiErrorMessage(result.body, "Upload failed."));
          return;
        }
        var envelope = result.body as {
          data?: {
            uploaded?: Array<{
              id: string;
              name: string;
              mime: string;
              size: number;
              storage_key: string;
              visibility: string;
              created_at: string;
            }>;
            failed?: Array<{ error?: string | null; message?: string | null }>;
          };
        } | null;
        var data = envelope && envelope.data ? envelope.data : null;
        var uploaded = data && data.uploaded ? data.uploaded : [];
        if (uploaded.length === 0) {
          var failed = data && data.failed ? data.failed : [];
          var firstFailed = failed.length > 0 ? failed[0] : null;
          var reason =
            firstFailed && (firstFailed.error || firstFailed.message)
              ? String(firstFailed.error || firstFailed.message)
              : "Upload failed.";
          setStatus(reason);
          return;
        }
        var document = uploaded[0];
        if (document.visibility === "public") {
          setStatus("Uploaded. The file is public and listed above.");
          loadPage(1);
          return;
        }
        setStatus(
          "Uploaded as private. It stays out of the picker until a manager " +
            "or admin flips it public."
        );
      })
      .catch(function () {
        setStatus("Upload failed.");
      });
  }

  function openPicker(options: MediaPickerOptions): void {
    closePicker();
    currentOptions = options;
    currentPage = 1;
    currentSearch = "";

    var root = document.createElement("div");
    root.className = "media-picker-overlay";
    root.setAttribute("role", "dialog");
    root.setAttribute("aria-modal", "true");
    root.setAttribute("aria-label", "Media picker");
    root.innerHTML =
      "<div class=\"media-picker-dialog admin-section section-panel\">" +
      "<h2>Media picker</h2>" +
      "<p class=\"page-intro\">Public files from the shared media library. " +
      "Private files stay hidden here until a manager or admin makes them public.</p>" +
      "<p><label>Search <input type=\"search\" data-picker-search placeholder=\"Search by name\"></label></p>" +
      "<div data-picker-list></div>" +
      "<nav data-picker-pager aria-label=\"Pagination\"></nav>" +
      "<hr>" +
      "<h3>Upload new file</h3>" +
      "<p><label>File <input type=\"file\" data-picker-file></label></p>" +
      "<p><label>Visibility " +
      "<select data-picker-visibility><option value=\"private\">Private</option>" +
      "<option value=\"public\">Public</option></select></label> " +
      "<span>Public only takes effect when the virus scan is clean.</span></p>" +
      "<p><button type=\"button\" class=\"button\" data-picker-upload>Upload</button> " +
      "<button type=\"button\" class=\"button\" data-picker-close>Close</button></p>" +
      "<p data-picker-status role=\"status\"></p>" +
      "</div>";
    root.addEventListener("click", function (event) {
      if (event.target === root) {
        closePicker();
      }
    });
    document.body.appendChild(root);
    overlay = root;
    document.addEventListener("keydown", onKeyDown, true);

    var search = root.querySelector<HTMLInputElement>("[data-picker-search]");
    if (search) {
      // Narrowed once here so the debounced callback below (which
      // TS cannot narrow across) keeps a non-null element.
      var searchEl: HTMLInputElement = search;
      var timer: number | null = null;
      searchEl.addEventListener("input", function () {
        if (timer !== null) {
          window.clearTimeout(timer);
        }
        timer = window.setTimeout(function () {
          currentSearch = searchEl.value.trim();
          loadPage(1);
        }, 300);
      });
    }
    var uploadButton = root.querySelector<HTMLButtonElement>("[data-picker-upload]");
    var fileInput = root.querySelector<HTMLInputElement>("[data-picker-file]");
    var visibilitySelect = root.querySelector<HTMLSelectElement>("[data-picker-visibility]");
    if (uploadButton && fileInput && visibilitySelect) {
      // Narrowed once here so the click callback below keeps
      // non-null elements.
      var fileEl: HTMLInputElement = fileInput;
      var visibilityEl: HTMLSelectElement = visibilitySelect;
      uploadButton.addEventListener("click", function () {
        var file = fileEl.files && fileEl.files.length > 0 ? fileEl.files[0] : null;
        if (!file) {
          setStatus("Choose a file to upload first.");
          return;
        }
        uploadFile(file, visibilityEl.value);
      });
    }
    var closeButton = root.querySelector<HTMLButtonElement>("[data-picker-close]");
    if (closeButton) {
      closeButton.addEventListener("click", function () {
        closePicker();
      });
    }
    loadPage(1);
  }

  // Small public surface for edit forms.
  (window as unknown as { UnifiedMediaPicker: UnifiedMediaPickerApi }).UnifiedMediaPicker = {
    open: openPicker,
    close: closePicker,
  };

  // Any element with `data-media-picker-open` opens the picker; the
  // owning form wires `onAttach` through `data-media-picker-target`
  // (the id of a hidden input that receives the picked document id)
  // and `data-media-picker-url-target` (the id of the input that
  // receives the picked /files/ URL). Pages without those targets
  // still get the dialog; the attach callback is a no-op there.
  document.addEventListener("DOMContentLoaded", function () {
    document.querySelectorAll<HTMLElement>("[data-media-picker-open]").forEach(function (button) {
      button.addEventListener("click", function () {
        var idTarget = button.getAttribute("data-media-picker-target");
        var urlTarget = button.getAttribute("data-media-picker-url-target");
        var moduleId = button.getAttribute("data-media-picker-module") || "content";
        openPicker({
          moduleId: moduleId,
          onAttach: function (item) {
            if (idTarget) {
              var idInput = document.getElementById(idTarget) as HTMLInputElement | null;
              if (idInput) {
                idInput.value = item.document_id;
              }
            }
            if (urlTarget) {
              var urlInput = document.getElementById(urlTarget) as HTMLInputElement | null;
              if (urlInput) {
                urlInput.value = item.url;
              }
            }
          },
        });
      });
    });
  });
})();
