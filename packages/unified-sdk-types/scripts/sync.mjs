// Sync the published SDK types from the single source of truth:
//   crates/unified-scripting/unified.d.ts
//
// Usage:
//   node scripts/sync.mjs          regenerate packages/unified-sdk-types/index.d.ts
//   node scripts/sync.mjs --check  exit 1 when the generated file is out of sync (CI gate)
//
// The generated file is NEVER edited by hand: any change to the
// `Unified` API is made in unified.d.ts (the runtime contract),
// then this script copies it here. The script also checks that
// the package version tracks the scripting crate's version.
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const source = join(root, "crates", "unified-scripting", "unified.d.ts");
const target = join(root, "packages", "unified-sdk-types", "index.d.ts");
const pkgPath = join(root, "packages", "unified-sdk-types", "package.json");
const crateManifest = join(root, "crates", "unified-scripting", "Cargo.toml");

const header = `/**
 * GENERATED — do not edit by hand.
 * Source of truth: crates/unified-scripting/unified.d.ts
 * Regenerate with: npm run sync   (in packages/unified-sdk-types)
 * Verify with:     npm run check
 */
`;

const dts = readFileSync(source, "utf8");
const pkg = JSON.parse(readFileSync(pkgPath, "utf8"));
const crate = readFileSync(crateManifest, "utf8");
const crateVersion = crate.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
if (crateVersion && crateVersion !== pkg.version) {
  console.error(
    `version mismatch: package.json is ${pkg.version} but crates/unified-scripting is ${crateVersion} — bump the package version first.`
  );
  process.exit(1);
}

const generated = header + "\n" + dts;

if (process.argv.includes("--check")) {
  const current = readFileSync(target, "utf8");
  if (current !== generated) {
    console.error(
      "index.d.ts is out of sync with crates/unified-scripting/unified.d.ts — run `npm run sync`."
    );
    process.exit(1);
  }
  console.log("index.d.ts is in sync with unified.d.ts.");
} else {
  writeFileSync(target, generated);
  console.log("index.d.ts regenerated from crates/unified-scripting/unified.d.ts.");
}
