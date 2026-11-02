import { defineConfig } from "vite";

// The app is embedded in the binary and served from `/`, so every asset URL is absolute
// and nothing is fingerprinted into a nested directory.
export default defineConfig({
  base: "/",
  build: {
    outDir: "dist",
    emptyOutDir: true,
    assetsDir: "assets",
  },
});
