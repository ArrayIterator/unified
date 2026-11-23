// Build the React bundle for every theme that ships one.
//
// Each theme declares its framework and libraries in its own
// themes/<theme>/package.json and its manifest (themes/<theme>/
// theme.yaml, `require` / `framework` / `assets`). The root
// package.json keeps only the shared toolchain and shared deps.
// This script typechecks once, then runs `vite build` per theme
// with UNIFIED_THEME set, producing assets/react/dist/<theme>/ per
// theme. The Rust server serves the active theme's bundle
// (falling back to the untitled bundle).
import { execSync } from "node:child_process";
import { existsSync, readdirSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

function themesWithReact() {
  const themesDir = path.join(root, "themes");
  if (!existsSync(themesDir)) return [];
  return readdirSync(themesDir, { withFileTypes: true })
    .filter((entry) => entry.isDirectory())
    .map((entry) => entry.name)
    .filter((name) => existsSync(path.join(themesDir, name, "react")))
    .sort();
}

function run(command, env) {
  execSync(command, { cwd: root, stdio: "inherit", env: { ...process.env, ...env } });
}

const themes = themesWithReact();
if (themes.length === 0) {
  console.log("No themes with a React app found under themes/.");
  process.exit(0);
}
run("npx tsc --noEmit -p tsconfig.react.json", {});
for (const theme of themes) {
  console.log(`\nBuilding React bundle for theme "${theme}" ...`);
  run("npx vite build", { UNIFIED_THEME: theme });
}
console.log(`\nBuilt ${themes.length} theme bundle(s): ${themes.join(", ")}`);
