// The shapes the native side hands this page: the boot payload written into
// the document, and the reply of every command.

export type Artifact = {
  artifact_ref: string;
  title: string;
  kind?: string | null;
  format?: string | null;
  target_kind?: string | null;
  slug?: string | null;
  updated_at_ms?: number | null;
  pinned: boolean;
  can_open: boolean;
  can_share: boolean;
};

export type LibrarySnapshot = { artifacts: Artifact[]; total: number };

// One Goals roster or Tracking row, projected by the bridge. Only these
// fields reach the page; a row always has a title.
export type ListingItem = {
  id?: string | null;
  title: string;
  body?: string | null;
  kind?: string | null;
  category?: string | null;
  kicker?: string | null;
  stamp?: string | null;
};

export type GoalsSnapshot = {
  goals: ListingItem[];
  goals_total: number;
  tracking: ListingItem[];
  tracking_total: number;
  tracking_error?: string | null;
};

// One Tasks row: the native page's own words, so the two renderers agree.
export type TaskRow = { id: string; label: string };

// `loaded` is the difference between an empty account and a page that has
// not read it yet, which the row lists alone cannot make.
export type TasksSnapshot = {
  schedules: TaskRow[];
  runs: TaskRow[];
  local: TaskRow[];
  loaded: boolean;
  error?: string | null;
};

// One bundle serves every window: the boot payload names the page it opens.
export type Boot =
  | (LibrarySnapshot & { page?: "library" })
  | (GoalsSnapshot & { page?: "goals" })
  | (TasksSnapshot & { page?: "tasks" });
