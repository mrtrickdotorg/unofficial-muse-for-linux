import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";

// jsdom does not paint: every test starts from a clean document and a window
// with no channel, so a page that forgets to ask for one is visible.
afterEach(() => {
  cleanup();
  delete (window as { __MUSE_BOOT__?: unknown }).__MUSE_BOOT__;
  delete (window as { webkit?: unknown }).webkit;
  delete (window as { __MUSE_ROWS__?: unknown }).__MUSE_ROWS__;
});
