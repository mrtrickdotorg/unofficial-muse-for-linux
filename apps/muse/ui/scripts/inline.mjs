// Fold the Vite build into one HTML file: the app is handed to WebKit with
// load_html(), so a <script src> or <link href> would resolve against
// about:blank and fail. Output is apps/muse/ui/built/library.html, which the
// Rust side includes verbatim.
import { mkdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const dist = join(root, "dist");
let html = readFileSync(join(dist, "index.html"), "utf8");

html = html.replace(
  /<script type="module"[^>]*src="([^"]+)"[^>]*><\/script>/g,
  (_, src) => {
    const code = readFileSync(join(dist, src.replace(/^\//, "")), "utf8");
    return '<script type="module">' + code.replace(/<\/script>/g, "<\\/script>") + "</script>";
  },
);

html = html.replace(
  /<link rel="stylesheet"[^>]*href="([^"]+)"[^>]*>/g,
  (_, href) => {
    const css = readFileSync(join(dist, href.replace(/^\//, "")), "utf8");
    return "<style>" + css + "</style>";
  },
);

if (/<script[^>]+src=|<link[^>]+href=/.test(html)) {
  console.error("inline: a fetched asset survived inlining");
  process.exit(1);
}

const out = join(root, "built");
mkdirSync(out, { recursive: true });
writeFileSync(join(out, "library.html"), html);
console.log("inline: built/library.html " + statSync(join(out, "library.html")).size + " bytes");
