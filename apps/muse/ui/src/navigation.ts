import { createContext, useContext } from "react";

/// The shell's page switch, for a page whose row opens another surface
/// (Upcoming's waiting approvals open Chat). A page mounted outside the shell
/// -- a preview, a test -- gets a no-op, so its rows still render and nothing
/// pretends to navigate.
export const PageNav = createContext<(page: string) => void>(() => {});

export function usePageNav() {
  return useContext(PageNav);
}
