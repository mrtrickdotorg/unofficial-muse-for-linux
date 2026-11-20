# Web UI slice (React)

The first page of the Tauri/React migration, rendered by the app's own
WebKitGTK view. Sources live here; `built/library.html` is the artifact the
Rust side includes with `include_str!`.

```sh
cd apps/muse/ui
npm install          # once
npm run build        # tsc --noEmit && vite build && inline into built/library.html
```

`scripts/inline.mjs` folds the Vite output into one HTML file because the page
is handed to WebKit with `load_html()` — a `<script src>` or `<link href>`
would resolve against `about:blank` and fail. There is no dev server and no
second browser engine in the bundle.

`built/library.html` is committed so a plain `cargo build` (and the AppImage
container) needs no Node toolchain. Rebuild it whenever `src/` changes.

The page talks to Rust through `window.webkit.messageHandlers.muse`: each call
posts `{"id","cmd"}` and the native side answers with
`window.museReply(id, reply)`. Commands read native state only; a mutating
command arrives with the first page that mutates.
