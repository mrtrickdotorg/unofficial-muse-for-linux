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
import { Goals } from "./Goals";
import { Ideas } from "./Ideas";
import { Library } from "./Library";
import { Tasks } from "./Tasks";
import { Threads } from "./Threads";
import "./hub.css";

// One bundle, one window per page: the boot payload names which page this
// document is, so a page never has to be told twice.
const boot = window.__MUSE_BOOT__;
const page =
  boot?.page ??
  (boot && "goals" in boot
    ? "goals"
    : boot && "schedules" in boot
      ? "tasks"
      : boot && "ideas" in boot
        ? "ideas"
        : boot && "threads" in boot
          ? "threads"
          : "library");

const mount = document.getElementById("root");
if (mount) {
  createRoot(mount).render(
    page === "goals" ? (
      <Goals />
    ) : page === "tasks" ? (
      <Tasks />
    ) : page === "ideas" ? (
      <Ideas />
    ) : page === "threads" ? (
      <Threads />
    ) : (
      <Library />
    ),
  );
}
