import { defineConfig } from "vite";

// Site-builder editor build (GrapeJS).
//
// The admin page `admin-site-builder` loads this bundle from
// `/assets/site-builder/dist/` - the Rust server's static asset
// route serves it from disk, exactly like the React track's
// `assets/react/dist/`. GrapeJS is bundled locally (no runtime CDN:
// a self-hosted ERP must work without one); the email builder's
// CDN-loaded GrapeJS is a separate, older page and is not reused
// here. Filenames are FIXED (no content hash): the Handlebars
// template references them directly.
//
// Build with `npm run build:site-builder`. The output IS committed
// (releases ship it), like the React track.
export default defineConfig({
  root: "assets/site-builder",
  base: "/assets/site-builder/dist/",
  build: {
    outDir: "dist",
    emptyOutDir: true,
    sourcemap: false,
    minify: true,
    rollupOptions: {
      input: "src/editor.ts",
      output: {
        entryFileNames: "editor.js",
        chunkFileNames: "editor-[name].js",
        assetFileNames: "editor.css",
      },
    },
  },
});
