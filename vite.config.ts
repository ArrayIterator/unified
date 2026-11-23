import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

// React track build. The app is served by the Rust server under the /react/
// mount, so `base` must stay "/react/" — the server worker relies on it.
// Build output goes to assets/react/dist/ and IS committed (releases ship
// it), unlike the Handlebars track which has no build step at all.
// `vite build` does not typecheck (esbuild strips types); the typecheck
// runs first as part of the `build:react` script, see package.json.
export default defineConfig({
  root: "assets/react",
  base: "/react/",
  plugins: [react(), tailwindcss()],
  build: {
    outDir: "dist",
    emptyOutDir: true,
    sourcemap: false,
    minify: true,
  },
});
