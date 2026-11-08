import { defineConfig } from "vite";

// The service worker, on its own (HS-067, ADR-0072 D4).
//
// A second build because a worker needs a **fixed** URL — `/sw.js`, at scope `/` — and the app's build hashes
// every entry it emits. Library mode is Vite's supported way to say "one file, this name": `fileName` is the
// name, `formats: ["es"]` (Rollup keeps no imports because the worker's only import is bundled in), and
// `emptyOutDir: false` so this build does not delete the app the first one just wrote. `publicDir` is off for
// the same reason it is not the worker's job to copy the icons twice.
export default defineConfig({
  publicDir: false,
  build: {
    outDir: "dist",
    emptyOutDir: false,
    // Readable on purpose: the served bytes are what `case_pwa` compares with this tree's `web/dist/sw.js`, and
    // the four sentences and the id rule are what a reader of the artifact can see.
    minify: false,
    lib: {
      entry: "src/sw.ts",
      formats: ["es"],
      fileName: () => "sw.js",
    },
  },
});
