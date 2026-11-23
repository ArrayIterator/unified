/* Site builder editor script.
 *
 * Admin page script for `/admin/site-builder` (content template
 * `admin-site-builder`). TypeScript source; transpiled in-memory by
 * the Rust server at startup (no build step). It talks only to the
 * site builder API the module registers:
 *
 *   GET    /api/v1/site-builder/pages
 *   POST   /api/v1/site-builder/pages                 {title, slug?, theme_id}
 *   GET    /api/v1/site-builder/pages/{id}
 *   DELETE /api/v1/site-builder/pages/{id}
 *   POST   /api/v1/site-builder/pages/{id}/publish
 *   POST   /api/v1/site-builder/pages/{id}/unpublish
 *   GET    /api/v1/site-builder/pages/{id}/render     (returns {html})
 *   GET/POST  /api/v1/site-builder/pages/{id}/blocks
 *   PUT/DELETE /api/v1/site-builder/pages/{id}/blocks/{block_id}
 *   PUT   /api/v1/site-builder/pages/{id}/blocks/order {ids}
 *   GET/POST  /api/v1/site-builder/pages/{id}/sections
 *   PUT/DELETE /api/v1/site-builder/pages/{id}/sections/{section_id}
 *   PUT   /api/v1/site-builder/pages/{id}/sections/order {ids}
 *
 * Block content is edited as JSON: the content shape is free-form
 * per block type, so a typed form would silently drop fields the
 * editor does not know about. The hint under the content box lists
 * the fields each block type reads when rendered.
 */
(function (): void {
  "use strict";

  var API = "/api/v1/site-builder/pages";

  var BLOCK_HINTS: Record<string, string> = {
    hero: "hero reads heading, subheading, image_url, cta_text, cta_url.",
    text: "text reads html (admin-authored markup) or text (escaped).",
    image: "image reads image_url and alt.",
    gallery: "gallery reads images (array of image_url values).",
    form: "form reads fields (array of {name, label, type, options}) and submit_text.",
    custom: "custom reads html.",
    pricing_table: "pricing_table reads plans (array of {name, price, features}).",
    marketplace_storefront: "marketplace_storefront reads storefront settings.",
  };

  type StatusKind = "info" | "success" | "error" | "warning";

  // Free-form JSON: block content shapes vary per block type, so the
  // editor keeps them as JSON values instead of a fixed interface.
  type JsonValue =
    | string
    | number
    | boolean
    | null
    | JsonValue[]
    | { [key: string]: JsonValue };

  interface ApiErrorBody {
    error?: { message?: string };
  }

  interface SiteBuilderBlock {
    id: string;
    type: string;
    order: number;
    content?: JsonValue;
  }

  interface SiteBuilderSection {
    id: string;
    name: string;
    layout?: string;
    order: number;
  }

  interface SiteBuilderPage {
    id: string;
    title: string;
    slug: string;
    status: string;
    theme_id: string;
    blocks?: SiteBuilderBlock[];
    sections?: SiteBuilderSection[];
  }

  interface RenderResult {
    html?: string;
  }

  interface CreatePagePayload {
    title: string;
    slug?: string;
    theme_id: string;
  }

  interface BlockPayload {
    type: string;
    content: JsonValue;
  }

  interface SectionPayload {
    name: string;
    layout?: string;
  }

  type PageAction = "publish" | "unpublish";

  // Elements that expose a string `value` (input, select, textarea).
  type ValueField = HTMLInputElement | HTMLSelectElement | HTMLTextAreaElement;

  function byId(id: string): HTMLElement | null {
    return document.getElementById(id);
  }

  function fieldById(id: string): ValueField | null {
    // The template renders these ids on input/select/textarea elements.
    return document.getElementById(id) as ValueField | null;
  }

  function formById(id: string): HTMLFormElement | null {
    return document.getElementById(id) as HTMLFormElement | null;
  }

  var statusBox: HTMLElement | null = null;
  var pageList: HTMLElement | null = null;
  var editor: HTMLElement | null = null;
  var editorMeta: HTMLElement | null = null;
  var blockList: HTMLElement | null = null;
  var sectionList: HTMLElement | null = null;
  var currentPage: SiteBuilderPage | null = null;
  var editingBlockId: string | null = null;
  var editingSectionId: string | null = null;

  function escapeHtml(value: unknown): string {
    return String(value == null ? "" : value)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;")
      .replace(/'/g, "&#39;");
  }

  function showStatus(message: string, kind?: StatusKind): void {
    if (!statusBox) {
      return;
    }
    statusBox.textContent = message;
    statusBox.className = "flash flash-" + (kind || "info");
    statusBox.hidden = false;
  }

  function errorMessage(body: unknown, fallback: string): string {
    var envelope = body as ApiErrorBody | null;
    if (envelope && envelope.error && envelope.error.message) {
      return envelope.error.message;
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
    if (
      data !== null &&
      typeof data === "object" &&
      Array.isArray((data as { items?: unknown }).items)
    ) {
      return (data as { items?: unknown }).items as T[];
    }
    return [];
  }

  function request<T>(url: string, method?: string, payload?: unknown): Promise<T> {
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
      .then(function (response: Response): Promise<T> {
        return response.json().then(function (body: unknown): T | Promise<T> {
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
          var envelope = body as { data?: unknown } | null;
          return (envelope && envelope.data !== undefined ? envelope.data : body) as T;
        });
      });
  }

  function contentSummary(content: unknown): string {
    if (content == null) {
      return "—";
    }
    var text: string = typeof content === "string" ? content : JSON.stringify(content);
    return text.length > 80 ? text.slice(0, 80) + "…" : text;
  }

  function loadPages(): void {
    // Capture once: the list is only used inside the fetch callbacks,
    // and a const keeps the null check narrowed there.
    const list = pageList;
    if (!list) {
      return;
    }
    request<unknown>(API + "?per_page=50")
      .then(function (data: unknown): void {
        var pages = itemsFromEnvelope<SiteBuilderPage>(data);
        if (!pages.length) {
          list.innerHTML =
            '<tr><td colspan="4">No pages yet. Create the first one above.</td></tr>';
          return;
        }
        list.innerHTML = pages
          .map(function (page: SiteBuilderPage): string {
            return (
              "<tr><td>" +
              escapeHtml(page.title) +
              "</td><td>" +
              escapeHtml(page.slug) +
              "</td><td>" +
              escapeHtml(page.status) +
              "</td><td>" +
              '<button class="button button-small" type="button" data-sb-edit-page="' +
              escapeHtml(page.id) +
              '">Edit</button> ' +
              '<button class="button button-small button-danger" type="button" data-sb-delete-page="' +
              escapeHtml(page.id) +
              '">Delete</button>' +
              "</td></tr>"
            );
          })
          .join("");
      })
      .catch(function (error: Error): void {
        list.innerHTML =
          '<tr><td colspan="4">Could not load pages: ' + escapeHtml(error.message) + "</td></tr>";
      });
  }

  function renderBlocks(page: SiteBuilderPage): void {
    if (!blockList) {
      return;
    }
    var blocks = (page.blocks || []).slice().sort(function (a: SiteBuilderBlock, b: SiteBuilderBlock): number {
      return a.order - b.order;
    });
    if (!blocks.length) {
      blockList.innerHTML = '<tr><td colspan="4">No blocks yet. Add the first one below.</td></tr>';
      return;
    }
    blockList.innerHTML = blocks
      .map(function (block: SiteBuilderBlock, index: number): string {
        return (
          "<tr><td>" +
          block.order +
          "</td><td>" +
          escapeHtml(block.type) +
          "</td><td>" +
          escapeHtml(contentSummary(block.content)) +
          "</td><td>" +
          '<button class="button button-small" type="button" data-sb-edit-block="' +
          escapeHtml(block.id) +
          '">Edit</button> ' +
          '<button class="button button-small" type="button" data-sb-move-block="' +
          escapeHtml(block.id) +
          '" data-sb-direction="-1" ' +
          (index === 0 ? "disabled" : "") +
          ">Up</button> " +
          '<button class="button button-small" type="button" data-sb-move-block="' +
          escapeHtml(block.id) +
          '" data-sb-direction="1" ' +
          (index === blocks.length - 1 ? "disabled" : "") +
          ">Down</button> " +
          '<button class="button button-small button-danger" type="button" data-sb-delete-block="' +
          escapeHtml(block.id) +
          '">Delete</button>' +
          "</td></tr>"
        );
      })
      .join("");
  }

  function renderSections(page: SiteBuilderPage): void {
    if (!sectionList) {
      return;
    }
    var sections = (page.sections || []).slice().sort(function (a: SiteBuilderSection, b: SiteBuilderSection): number {
      return a.order - b.order;
    });
    if (!sections.length) {
      sectionList.innerHTML =
        '<tr><td colspan="4">No sections yet. Add the first one below.</td></tr>';
      return;
    }
    sectionList.innerHTML = sections
      .map(function (section: SiteBuilderSection, index: number): string {
        return (
          "<tr><td>" +
          section.order +
          "</td><td>" +
          escapeHtml(section.name) +
          "</td><td>" +
          escapeHtml(section.layout || "") +
          "</td><td>" +
          '<button class="button button-small" type="button" data-sb-edit-section="' +
          escapeHtml(section.id) +
          '">Edit</button> ' +
          '<button class="button button-small" type="button" data-sb-move-section="' +
          escapeHtml(section.id) +
          '" data-sb-direction="-1" ' +
          (index === 0 ? "disabled" : "") +
          ">Up</button> " +
          '<button class="button button-small" type="button" data-sb-move-section="' +
          escapeHtml(section.id) +
          '" data-sb-direction="1" ' +
          (index === sections.length - 1 ? "disabled" : "") +
          ">Down</button> " +
          '<button class="button button-small button-danger" type="button" data-sb-delete-section="' +
          escapeHtml(section.id) +
          '">Delete</button>' +
          "</td></tr>"
        );
      })
      .join("");
  }

  function loadPage(id: string): void {
    request<SiteBuilderPage>(API + "/" + encodeURIComponent(id))
      .then(function (page: SiteBuilderPage): void {
        currentPage = page;
        editingBlockId = null;
        editingSectionId = null;
        if (editor) {
          editor.hidden = false;
        }
        if (editorMeta) {
          editorMeta.textContent =
            page.title +
            " — slug: " +
            page.slug +
            " — status: " +
            page.status +
            " — theme: " +
            page.theme_id;
        }
        renderBlocks(page);
        renderSections(page);
      })
      .catch(function (error: Error): void {
        showStatus("Could not load the page: " + error.message, "error");
      });
  }

  function refreshCurrent(): void {
    if (currentPage) {
      loadPage(currentPage.id);
    }
  }

  function parseContent(text: string): JsonValue {
    var trimmed = (text || "").trim();
    if (!trimmed) {
      return {};
    }
    return JSON.parse(trimmed) as JsonValue;
  }

  document.addEventListener("DOMContentLoaded", function (): void {
    statusBox = document.getElementById("sb-status");
    pageList = document.getElementById("sb-page-list");
    editor = document.getElementById("sb-editor");
    editorMeta = document.getElementById("sb-editor-meta");
    blockList = document.getElementById("sb-block-list");
    sectionList = document.getElementById("sb-section-list");
    if (!pageList) {
      return;
    }
    loadPages();

    const typeSelect: ValueField | null = fieldById("sb-block-type");
    const hint: HTMLElement | null = byId("sb-block-hint");
    if (typeSelect && hint) {
      typeSelect.addEventListener("change", function (): void {
        hint.textContent =
          BLOCK_HINTS[typeSelect.value] || "Content is JSON for this block type.";
      });
    }

    const newPageForm: HTMLFormElement | null = formById("sb-new-page");
    if (newPageForm) {
      newPageForm.addEventListener("submit", function (event: SubmitEvent): void {
        event.preventDefault();
        const titleInput: ValueField | null = fieldById("sb-page-title");
        const slugInput: ValueField | null = fieldById("sb-page-slug");
        const themeInput: ValueField | null = fieldById("sb-page-theme");
        if (!titleInput || !themeInput) {
          showStatus("Title and theme are both required.", "error");
          return;
        }
        var title: string = titleInput.value.trim();
        var slug: string = slugInput ? slugInput.value.trim() : "";
        var themeId: string = themeInput.value.trim();
        if (!title || !themeId) {
          showStatus("Title and theme are both required.", "error");
          return;
        }
        var payload: CreatePagePayload = { title: title, theme_id: themeId };
        if (slug) {
          payload.slug = slug;
        }
        request<SiteBuilderPage>(API, "POST", payload)
          .then(function (page: SiteBuilderPage): void {
            showStatus('Page "' + page.title + '" created.', "success");
            newPageForm.reset();
            themeInput.value = "default";
            loadPages();
            loadPage(page.id);
          })
          .catch(function (error: Error): void {
            showStatus("Could not create the page: " + error.message, "error");
          });
      });
    }

    // Delegated actions for the page, block, and section tables.
    document.addEventListener("click", function (event: MouseEvent): void {
      // event.target is EventTarget | null; only Element targets carry
      // closest(), and click targets on this page always are elements.
      var target: Element | null =
        event.target instanceof Element
          ? event.target.closest("[data-sb-edit-page],[data-sb-delete-page],[data-sb-edit-block],[data-sb-delete-block],[data-sb-move-block],[data-sb-edit-section],[data-sb-delete-section],[data-sb-move-section]")
          : null;
      if (!target) {
        return;
      }
      // getAttribute() returns null exactly when hasAttribute() is
      // false, so the null checks below match the original attribute
      // checks while also narrowing the id to string.
      var editPageId: string | null = target.getAttribute("data-sb-edit-page");
      if (editPageId !== null) {
        loadPage(editPageId);
        return;
      }
      var deletePageId: string | null = target.getAttribute("data-sb-delete-page");
      if (deletePageId !== null) {
        var pageId: string = deletePageId;
        if (!window.confirm("Delete this page and all its blocks and sections?")) {
          return;
        }
        request<unknown>(API + "/" + encodeURIComponent(pageId), "DELETE")
          .then(function (): void {
            showStatus("Page deleted.", "success");
            if (currentPage && currentPage.id === pageId) {
              currentPage = null;
              if (editor) {
                editor.hidden = true;
              }
            }
            loadPages();
          })
          .catch(function (error: Error): void {
            showStatus("Could not delete the page: " + error.message, "error");
          });
        return;
      }
      if (!currentPage) {
        return;
      }
      var pageUrl: string = API + "/" + encodeURIComponent(currentPage.id);
      var editBlockId: string | null = target.getAttribute("data-sb-edit-block");
      if (editBlockId !== null) {
        var block: SiteBuilderBlock | undefined = (currentPage.blocks || []).find(function (item: SiteBuilderBlock): boolean {
          return item.id === editBlockId;
        });
        if (!block) {
          return;
        }
        editingBlockId = block.id;
        var typeField: ValueField | null = fieldById("sb-block-type");
        var contentField: ValueField | null = fieldById("sb-block-content");
        if (typeField) {
          typeField.value = block.type;
        }
        if (contentField) {
          contentField.value = JSON.stringify(
            block.content || {},
            null,
            2
          );
          contentField.focus();
        }
        if (hint) {
          hint.textContent = BLOCK_HINTS[block.type] || "Content is JSON for this block type.";
        }
        showStatus('Editing block "' + block.type + '". Save it with Add block.', "info");
        return;
      }
      var deleteBlockId: string | null = target.getAttribute("data-sb-delete-block");
      if (deleteBlockId !== null) {
        request<unknown>(
          pageUrl + "/blocks/" + encodeURIComponent(deleteBlockId),
          "DELETE"
        )
          .then(function (): void {
            showStatus("Block deleted.", "success");
            refreshCurrent();
          })
          .catch(function (error: Error): void {
            showStatus("Could not delete the block: " + error.message, "error");
          });
        return;
      }
      var moveBlockId: string | null = target.getAttribute("data-sb-move-block");
      if (moveBlockId !== null) {
        var blocks: SiteBuilderBlock[] = (currentPage.blocks || []).slice().sort(function (a: SiteBuilderBlock, b: SiteBuilderBlock): number {
          return a.order - b.order;
        });
        var index: number = blocks.findIndex(function (item: SiteBuilderBlock): boolean {
          return item.id === moveBlockId;
        });
        var next: number = index + Number(target.getAttribute("data-sb-direction"));
        if (index < 0 || next < 0 || next >= blocks.length) {
          return;
        }
        var moved: SiteBuilderBlock = blocks.splice(index, 1)[0];
        blocks.splice(next, 0, moved);
        request<unknown>(
          pageUrl + "/blocks/order",
          "PUT",
          { ids: blocks.map(function (item: SiteBuilderBlock): string { return item.id; }) }
        )
          .then(refreshCurrent)
          .catch(function (error: Error): void {
            showStatus("Could not reorder blocks: " + error.message, "error");
          });
        return;
      }
      var editSectionId: string | null = target.getAttribute("data-sb-edit-section");
      if (editSectionId !== null) {
        var section: SiteBuilderSection | undefined = (currentPage.sections || []).find(function (item: SiteBuilderSection): boolean {
          return item.id === editSectionId;
        });
        if (!section) {
          return;
        }
        editingSectionId = section.id;
        var nameField: ValueField | null = fieldById("sb-section-name");
        var layoutField: ValueField | null = fieldById("sb-section-layout");
        if (nameField) {
          nameField.value = section.name;
          nameField.focus();
        }
        if (layoutField) {
          layoutField.value = section.layout || "";
        }
        showStatus('Editing section "' + section.name + '". Save it with Add section.', "info");
        return;
      }
      var deleteSectionId: string | null = target.getAttribute("data-sb-delete-section");
      if (deleteSectionId !== null) {
        request<unknown>(
          pageUrl + "/sections/" + encodeURIComponent(deleteSectionId),
          "DELETE"
        )
          .then(function (): void {
            showStatus("Section deleted.", "success");
            refreshCurrent();
          })
          .catch(function (error: Error): void {
            showStatus("Could not delete the section: " + error.message, "error");
          });
        return;
      }
      var moveSectionId: string | null = target.getAttribute("data-sb-move-section");
      if (moveSectionId !== null) {
        var sections: SiteBuilderSection[] = (currentPage.sections || []).slice().sort(function (a: SiteBuilderSection, b: SiteBuilderSection): number {
          return a.order - b.order;
        });
        var sectionIndex: number = sections.findIndex(function (item: SiteBuilderSection): boolean {
          return item.id === moveSectionId;
        });
        var sectionNext: number = sectionIndex + Number(target.getAttribute("data-sb-direction"));
        if (sectionIndex < 0 || sectionNext < 0 || sectionNext >= sections.length) {
          return;
        }
        var movedSection: SiteBuilderSection = sections.splice(sectionIndex, 1)[0];
        sections.splice(sectionNext, 0, movedSection);
        request<unknown>(
          pageUrl + "/sections/order",
          "PUT",
          { ids: sections.map(function (item: SiteBuilderSection): string { return item.id; }) }
        )
          .then(refreshCurrent)
          .catch(function (error: Error): void {
            showStatus("Could not reorder sections: " + error.message, "error");
          });
      }
    });

    const newBlockForm: HTMLFormElement | null = formById("sb-new-block");
    if (newBlockForm) {
      newBlockForm.addEventListener("submit", function (event: SubmitEvent): void {
        event.preventDefault();
        if (!currentPage) {
          showStatus("Select a page first.", "error");
          return;
        }
        var page: SiteBuilderPage = currentPage;
        var typeField: ValueField | null = fieldById("sb-block-type");
        var contentField: ValueField | null = fieldById("sb-block-content");
        if (!typeField || !contentField) {
          return;
        }
        var content: JsonValue = null;
        try {
          content = parseContent(contentField.value);
        } catch (error) {
          showStatus("Block content is not valid JSON.", "error");
          return;
        }
        var payload: BlockPayload = {
          type: typeField.value,
          content: content,
        };
        var done = function (): void {
          showStatus(
            editingBlockId ? "Block updated." : "Block added.",
            "success"
          );
          editingBlockId = null;
          newBlockForm.reset();
          refreshCurrent();
        };
        var failed = function (error: Error): void {
          showStatus("Could not save the block: " + error.message, "error");
        };
        if (editingBlockId) {
          request<unknown>(
            API + "/" + encodeURIComponent(page.id) + "/blocks/" + encodeURIComponent(editingBlockId),
            "PUT",
            payload
          ).then(done, failed);
        } else {
          request<unknown>(API + "/" + encodeURIComponent(page.id) + "/blocks", "POST", payload).then(
            done,
            failed
          );
        }
      });
    }

    const newSectionForm: HTMLFormElement | null = formById("sb-new-section");
    if (newSectionForm) {
      newSectionForm.addEventListener("submit", function (event: SubmitEvent): void {
        event.preventDefault();
        if (!currentPage) {
          showStatus("Select a page first.", "error");
          return;
        }
        var page: SiteBuilderPage = currentPage;
        var nameField: ValueField | null = fieldById("sb-section-name");
        var layoutField: ValueField | null = fieldById("sb-section-layout");
        if (!nameField || !layoutField) {
          return;
        }
        var name: string = nameField.value.trim();
        var layout: string = layoutField.value.trim();
        if (!name) {
          showStatus("Section name is required.", "error");
          return;
        }
        var payload: SectionPayload = { name: name };
        if (layout) {
          payload.layout = layout;
        }
        var done = function (): void {
          showStatus(
            editingSectionId ? "Section updated." : "Section added.",
            "success"
          );
          editingSectionId = null;
          newSectionForm.reset();
          refreshCurrent();
        };
        var failed = function (error: Error): void {
          showStatus("Could not save the section: " + error.message, "error");
        };
        if (editingSectionId) {
          request<unknown>(
            API + "/" + encodeURIComponent(page.id) + "/sections/" + encodeURIComponent(editingSectionId),
            "PUT",
            payload
          ).then(done, failed);
        } else {
          request<unknown>(
            API + "/" + encodeURIComponent(page.id) + "/sections",
            "POST",
            payload
          ).then(done, failed);
        }
      });
    }

    function pageAction(action: PageAction, verb: string): void {
      const button: HTMLElement | null = byId("sb-" + action);
      if (!button) {
        return;
      }
      button.addEventListener("click", function (): void {
        if (!currentPage) {
          return;
        }
        request<SiteBuilderPage>(
          API + "/" + encodeURIComponent(currentPage.id) + "/" + action,
          "POST"
        )
          .then(function (page: SiteBuilderPage): void {
            currentPage = page;
            showStatus("Page " + verb + ".", "success");
            loadPages();
            renderBlocks(page);
            renderSections(page);
            if (editorMeta) {
              editorMeta.textContent =
                page.title +
                " — slug: " +
                page.slug +
                " — status: " +
                page.status +
                " — theme: " +
                page.theme_id;
            }
          })
          .catch(function (error: Error): void {
            showStatus("Could not " + verb + " the page: " + error.message, "error");
          });
      });
    }
    pageAction("publish", "published");
    pageAction("unpublish", "unpublished");

    const previewButton: HTMLElement | null = byId("sb-preview");
    if (previewButton) {
      previewButton.addEventListener("click", function (): void {
        if (!currentPage) {
          return;
        }
        request<RenderResult>(API + "/" + encodeURIComponent(currentPage.id) + "/render")
          .then(function (data: RenderResult): void {
            var win = window.open("", "_blank");
            if (!win) {
              showStatus("The preview was blocked by a pop-up blocker.", "warning");
              return;
            }
            win.document.open();
            win.document.write(data.html || "");
            win.document.close();
          })
          .catch(function (error: Error): void {
            showStatus("Could not preview the page: " + error.message, "error");
          });
      });
    }

    const deletePageButton: HTMLElement | null = byId("sb-delete-page");
    if (deletePageButton) {
      deletePageButton.addEventListener("click", function (): void {
        if (!currentPage) {
          return;
        }
        if (!window.confirm("Delete this page and all its blocks and sections?")) {
          return;
        }
        var id: string = currentPage.id;
        request<unknown>(API + "/" + encodeURIComponent(id), "DELETE")
          .then(function (): void {
            showStatus("Page deleted.", "success");
            currentPage = null;
            if (editor) {
              editor.hidden = true;
            }
            loadPages();
          })
          .catch(function (error: Error): void {
            showStatus("Could not delete the page: " + error.message, "error");
          });
      });
    }

    // Deep link: /admin/site-builder/<page-id> opens that page's editor.
    var parts: string[] = window.location.pathname.split("/").filter(Boolean);
    if (parts.length >= 3 && parts[0] === "admin" && parts[1] === "site-builder") {
      loadPage(decodeURIComponent(parts.slice(2).join("/")));
    }
  });
})();
