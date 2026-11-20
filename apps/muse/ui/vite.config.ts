/// <reference types="vitest" />
import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

// One HTML file out: the app is loaded with load_html(), so nothing can be
// fetched from disk at runtime. The inline step below folds the emitted CSS
// and module into the document; there is no dev server in the path.
export default defineConfig({
  plugins: [react()],
  build: {
    outDir: "dist",
    target: "es2020",
    cssCodeSplit: false,
    assetsDir: "assets",
  },
  // The page's behaviour is checked in jsdom against the same channel the
  // app speaks, so a page can be verified without a running window.
  test: {
    environment: "jsdom",
    setupFiles: ["./src/setupTests.ts"],
    include: ["src/**/*.test.{ts,tsx}"],
  },
});
