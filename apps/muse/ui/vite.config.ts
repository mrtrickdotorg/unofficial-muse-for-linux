import { defineConfig } from "vite";
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
});
