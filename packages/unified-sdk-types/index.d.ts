/**
 * GENERATED — do not edit by hand.
 * Source of truth: crates/unified-scripting/unified.d.ts
 * Regenerate with: npm run sync   (in packages/unified-sdk-types)
 * Verify with:     npm run check
 */

/**
 * Type declarations for the `Unified` global object injected into
 * every TypeScript external module.
 *
 * This file is the canonical contract: the runtime
 * (`crates/unified-scripting`) injects exactly the object declared
 * here, backed by the real Rust SDK. Type-check a module with:
 *
 *   tsc --noEmit
 *
 * with a `tsconfig.json` that includes this file (copy it next to
 * your module, or reference it with a `/// <reference path="..." />`
 * comment, as `modules/example-ts/module.ts` does).
 *
 * The runtime transpiles TypeScript to JavaScript with OXC and
 * evaluates the result as a plain script in QuickJS. Do not use
 * `import`/`export`, `async`/`await` at the top level, or any
 * Node/Deno/Bun API: only the `Unified` global exists.
 */

/** The `Unified` global: the whole SDK, one object per module. */
declare namespace Unified {
  /** SDK version (`"0.1.0"`). */
  const version: string;

  /** This module's own identity, from its `module.json`. */
  const module: {
    readonly id: string;
    readonly name: string;
    readonly version: string;
  };

  /** Structured logging (goes to the server log as `unified::scripting::<module-id>`). */
  interface Log {
    debug(...args: unknown[]): void;
    info(...args: unknown[]): void;
    warn(...args: unknown[]): void;
    error(...args: unknown[]): void;
  }
  const log: Log;

  /**
   * Read-only settings. Only the curated keys the server exposes
   * are visible (`app.mode`, `app.workers`, `scripting.*`); never
   * secrets. `get` returns `undefined` for an unknown key.
   */
  interface Config {
    get(key: string): unknown;
    keys(): string[];
  }
  const config: Config;

  /** One hook emission as a handler receives it. */
  interface HookEvent {
    /** Event name, e.g. `"user.created"`. */
    name: string;
    /** The payload the emitter passed (JSON value). */
    data: unknown;
  }

  /** A hook handler. Must be synchronous; a returned Promise is ignored. */
  type HookHandler = (event: HookEvent) => unknown;

  /**
   * The server hook bus. `on` registers a handler for an event
   * (the module's `register_hooks` equivalent); `emit` emits
   * synchronously on the real bus and returns the handlers'
   * results. Names use `[a-z0-9._-]` only, at most 128 characters.
   */
  interface Hooks {
    on(event: string, handler: HookHandler): void;
    emit(event: string, data?: unknown): unknown[];
  }
  const hooks: Hooks;
  /** Same bus as `hooks`, under its second name. */
  const events: Hooks;

  /** One HTTP request as a route handler receives it. */
  interface UnifiedRequest {
    /** Upper-case method, e.g. `"GET"`. */
    method: string;
    /** Request path, e.g. `"/hello"`. */
    path: string;
    /** `:name` segments from the route pattern. */
    params: Record<string, string>;
    /** Decoded query string. */
    query: Record<string, string>;
    /** Request headers (lower-cased names). */
    headers: Record<string, string>;
    /** Parsed JSON body, a raw string when not JSON, or `null`. */
    body: unknown;
  }

  /**
   * What a route handler may return. Return `{ status, body }`
   * to choose the HTTP status (100-599; anything else becomes
   * 200); return any other JSON value and it becomes the body
   * with status 200. The dispatcher wraps the body in the
   * standard `{"data": ...}` envelope.
   */
  interface UnifiedResponse {
    status?: number;
    body?: unknown;
  }

  /** A route handler. Must be synchronous; an `async` handler is refused at dispatch time. */
  type RouteHandler = (req: UnifiedRequest) => UnifiedResponse | unknown;

  /**
   * HTTP routes. Patterns start with `/`, have 1-10 segments,
   * and support `:name` segments (`"/users/:id"`). The module is
   * served under `/api/v1/ts/<module-id>` + the pattern.
   */
  interface Routes {
    get(pattern: string, handler: RouteHandler): void;
    post(pattern: string, handler: RouteHandler): void;
    put(pattern: string, handler: RouteHandler): void;
    patch(pattern: string, handler: RouteHandler): void;
    delete(pattern: string, handler: RouteHandler): void;
  }
  const routes: Routes;

  /** Options for `http.fetch`. Unknown keys are ignored. */
  interface FetchOptions {
    /** Default `"GET"`. One of GET/POST/PUT/PATCH/DELETE/HEAD/OPTIONS. */
    method?: string;
    headers?: Record<string, string>;
    /** Request body (sent as-is). */
    body?: string;
    /** Timeout in milliseconds (default 10000, max 30000). */
    timeout_ms?: number;
  }

  /** What `http.fetch` returns. */
  interface FetchResponse {
    /** `true` for 2xx statuses. */
    ok: boolean;
    status: number;
    headers: Record<string, string>;
    /** Response body as text (5 MiB cap). */
    body: string;
  }

  /**
   * Outbound HTTP. SSRF-safe: http(s) only, no credentials in
   * the URL, every resolved address must be public (loopback
   * only with the dev exception, allowlisted hosts skip the
   * check), redirects never followed, 2048-character URL cap.
   */
  interface Http {
    fetch(url: string, options?: FetchOptions): FetchResponse;
  }
  const http: Http;

  /**
   * File access, scoped to this module's own directory
   * (`<storage>/scripting-storage/<module-id>/`). Absolute
   * paths, `..` escapes, and symlink escapes are rejected;
   * writes are atomic (temp file + rename); 10 MiB file cap.
   */
  interface Storage {
    read(path: string): string;
    write(path: string, data: string): void;
    list(dir?: string): string[];
    /** Returns `true` when a file was actually removed. */
    delete(path: string): boolean;
  }
  const storage: Storage;

  /**
   * Per-module key/value store, namespaced by tenant and module
   * id. Values are JSON. `get` returns `undefined` for a missing
   * key.
   */
  interface Kv {
    get(key: string): unknown;
    set(key: string, value: unknown): void;
    delete(key: string): boolean;
    list(prefix?: string): string[];
  }

  interface Db {
    kv: Kv;
    /**
     * Tenant-scoped read-only query. SELECT only — WITH is refused
     * because a CTE can carry a write — `$1`/`$2`/...
     * parameters substituted as escaped text literals (see
     * `substitute_params` in the scripting crate), 1000-row and
     * 64-parameter caps. Fails without a tenant context or a
     * configured database. Never touches the control-plane database.
     */
    query(sql: string, params?: unknown[]): Record<string, unknown>[];
  }
  const db: Db;
}
