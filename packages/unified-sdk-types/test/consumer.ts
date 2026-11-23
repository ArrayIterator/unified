/// <reference path="../index.d.ts" />

// Consumer test: a module authoring against the published
// @unified/sdk-types package. Must compile with `tsc --noEmit`
// (`npm test` in this package). A real consumer installs the
// package and references it by name instead of by path.

const moduleId: string = Unified.module.id;
const sdkVersion: string = Unified.version;

Unified.log.info("module", moduleId, "sdk", sdkVersion);
Unified.config.get("app.mode");
Unified.config.keys();

Unified.hooks.on("user.created", (event) => {
  const name: string = event.name;
  const data: unknown = event.data;
  Unified.db.kv.set("last-user-created", data);
  return name;
});
Unified.events.emit("user.created", { id: 1 });

Unified.routes.get("/hello", (req) => {
  const name: string = req.query.name ?? "world";
  const id: string = req.params.id;
  const ua: string = req.headers["user-agent"] ?? "";
  return {
    status: 200,
    body: { message: `Hello, ${name}!`, id, ua, method: req.method, path: req.path },
  };
});
Unified.routes.post("/users", (_req) => ({ status: 201, body: { ok: true } }));

const res: Unified.FetchResponse = Unified.http.fetch("https://example.com/api", {
  method: "GET",
  timeout_ms: 5000,
});
const ok: boolean = res.ok;

Unified.storage.write("notes.txt", "hello");
const text: string = Unified.storage.read("notes.txt");
const names: string[] = Unified.storage.list();
const deleted: boolean = Unified.storage.delete("notes.txt");

Unified.db.kv.set("key", { a: 1 });
const value: unknown = Unified.db.kv.get("key");
const keys: string[] = Unified.db.kv.list("prefix");
const rows: Record<string, unknown>[] = Unified.db.query(
  "SELECT id FROM users WHERE id = $1",
  [1]
);

void ok;
void text;
void names;
void deleted;
void value;
void keys;
void rows;
