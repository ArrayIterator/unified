import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import path from "node:path";

// React track build. The app is served by the Rust server under the /react/
// mount, so `base` must stay "/react/" — the server worker relies on it.
// Build output goes to assets/react/dist/<theme>/ and IS committed
// (releases ship it), unlike the Handlebars track which has no build
// step at all.
// `vite build` does not typecheck (esbuild strips types); the typecheck
// runs first as part of the `build:react` script, see package.json.
//
// Per-theme builds: the active theme's React components live in
// themes/<theme>/react/ and are imported through the `@theme` alias,
// so the app code never hard-codes a theme path. `UNIFIED_THEME`
// selects the theme (default "untitled"); each theme builds to its
// own dist/<theme>/ bundle, and the Rust server serves the bundle of
// the currently active theme (falling back to the untitled bundle).
// The theme's own package.json (themes/<theme>/package.json) declares
// its framework dependencies; the root package.json keeps only the
// shared toolchain and shared deps.
const theme = process.env.UNIFIED_THEME || "untitled";
const themeDir = path.resolve(__dirname, "themes", theme, "react");

export default defineConfig({
  root: "assets/react",
  base: "/react/",
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@theme": themeDir,
    },
  },
  server: {
    fs: {
      // The theme lives outside the Vite root; allow serving it.
      allow: [path.resolve(__dirname, "assets"), path.resolve(__dirname, "themes")],
    },
  },
  build: {
    outDir: `dist/${theme}`,
    emptyOutDir: true,
    sourcemap: false,
    minify: true,
  },
});
