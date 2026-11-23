/// <reference path="../../crates/unified-scripting/unified.d.ts" />

/**
 * Example TypeScript external module for Unified.
 *
 * Mirrors `crates/example-hello-module` (the smallest complete
 * native module): one HTTP route and one hook listener, no
 * database, no configuration section.
 *
 * Served at `GET /api/v1/ts/example-ts/hello?name=world`.
 * The hook fires whenever a user is created.
 */

Unified.routes.get("/hello", (req) => {
  const name = req.query.name ?? "world";
  Unified.log.info("hello route called for", name);
  return {
    status: 200,
    body: {
      message: `Hello, ${name}!`,
      module: Unified.module.id,
      version: Unified.module.version,
    },
  };
});

Unified.hooks.on("user.created", (event) => {
  Unified.log.info("a user was created", event.data);
  Unified.db.kv.set("last-user-created", event.data);
});
