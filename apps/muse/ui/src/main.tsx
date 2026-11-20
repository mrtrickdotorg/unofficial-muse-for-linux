import { createRoot } from "react-dom/client";

// The native side reads this to tell a script error from an empty page.
declare global {
  interface Window {
    __MUSE_ERR__?: string;
  }
}
window.__MUSE_ERR__ = "";
window.addEventListener("error", (event) => {
  window.__MUSE_ERR__ = String(event.message || "");
});
import { Library } from "./Library";
import "./hub.css";

const mount = document.getElementById("root");
if (mount) createRoot(mount).render(<Library />);
