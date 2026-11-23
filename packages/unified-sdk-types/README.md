# @unified/sdk-types

Type declarations for the `Unified` global SDK object injected
into TypeScript external modules (`crates/unified-scripting`).
This is the `@types`-style package module authors depend on
instead of copying `crates/unified-scripting/unified.d.ts`.

## Single source of truth

`crates/unified-scripting/unified.d.ts` is the canonical
contract (it is what the runtime injects). `index.d.ts` in this
package is **generated** from it — never edit it by hand:

```sh
npm run sync    # regenerate index.d.ts from unified.d.ts
npm run check   # fail when index.d.ts is out of sync (CI gate)
npm test        # tsc --noEmit on the consumer test
```

The sync script also checks that this package's version tracks
the scripting crate's version (`crates/unified-scripting/Cargo.toml`).

## Consuming it

Install as a dev dependency (private package — `file:` path
until it is published to a registry):

```json
{
  "devDependencies": {
    "@unified/sdk-types": "file:../packages/unified-sdk-types"
  }
}
```

Then reference it from your module (or list it in your
`tsconfig.json` under `compilerOptions.types`):

```ts
/// <reference types="@unified/sdk-types" />

Unified.routes.get("/hello", (req) => {
  return { status: 200, body: { message: "Hello!" } };
});
```

## What the types cover

`Unified.version`, `Unified.module`, `Unified.log`,
`Unified.config`, `Unified.hooks` / `Unified.events`,
`Unified.routes`, `Unified.http`, `Unified.storage`,
`Unified.db.kv`, `Unified.db.query` — the full runtime API.
See `crates/unified-scripting/README.md` for the runtime
reference (limits, sandbox rules, reload).
