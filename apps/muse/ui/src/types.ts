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
  /// The service's own emoji for the row (Ideas); the page falls back to
  /// the native default when it is absent.
  icon?: string | null;
};

export type GoalsSnapshot = {
  goals: ListingItem[];
  goals_total: number;
  tracking: ListingItem[];
  tracking_total: number;
  tracking_error?: string | null;
};

// The Ideas roster, with `loaded` separating an account with no ideas from a
// page that has not read them yet.
export type IdeasSnapshot = {
  ideas: ListingItem[];
  total: number;
  loaded: boolean;
  error?: string | null;
};

// One Search result: the conversation it belongs to, and the native label's
// parts (icon, title, and the kicker/kind/stamp line).
export type SearchRow = {
  id: string;
  icon: string;
  title: string;
  meta: string;
  image_url: string;
};

// The search as the web view opens it: the query this page ran, and the rows
// the native search painted for it.
export type SearchSnapshot = {
  query: string;
  rows: SearchRow[];
  total: number;
  loaded: boolean;
  error?: string | null;
};

// One Feed card: the native page's own meta line, relative time, action
// label and graphic rule. `widget_html` is service markup for the card's
// frame; `widget_text` is the extracted text the native page falls back to.
export type FeedCard = {
  id: string;
  icon: string;
  meta: string;
  title: string;
  when: string;
  body: string;
  action: string;
  why: string;
  image_url: string;
  widget_html: string;
  widget_text: string;
  live: boolean;
};

// The feed as the web view opens it: the generating prompt, the service's
// generation line, and the cards.
export type FeedSnapshot = {
  cards: FeedCard[];
  prompt: string;
  status: string;
  total: number;
  loaded: boolean;
  error?: string | null;
};

// One Threads row: the group header, the snippet-and-clock line and the
// side/pin flags are computed natively, so the page cannot word a row
// differently from the list beside it.
export type ThreadRow = {
  id: string;
  title: string;
  meta: string;
  group: string;
  unread: number;
  pinned: boolean;
  side: boolean;
};

// The conversation listing, with `loaded` separating an account with no
// conversations from a page that has not read them yet.
export type ThreadsSnapshot = {
  threads: ThreadRow[];
  total: number;
  loaded: boolean;
  error?: string | null;
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

// One transcript row, in the three shapes the native list builds: a day
// divider, a message, or a service notice. `id` is the service's own message
// id, which is what lets a reply that grows keep its element.
export type ChatRow = {
  kind: "day" | "message" | "notice";
  id: string;
  role: "user" | "assistant" | "";
  text: string;
  day: string;
  sent_at?: string | null;
  attachments: string[];
  options: string[];
  selected?: string | null;
};

// The Chat surface as this window opens it: the open thread, the rows the
// native transcript last built, and the composer's status line. `loaded`
// separates a thread nobody has read yet from an empty one.
export type ChatSnapshot = {
  thread?: string | null;
  title: string;
  account: string;
  status?: string | null;
  // Whether the app has a send in flight: the flag the native card swaps
  // Send for Stop on, so the page swaps the same circle.
  sending?: boolean;
  rows: ChatRow[];
  loaded: boolean;
  error?: string | null;
};

// One bundle serves every window: the boot payload names the page it opens.
export type Boot =
  | (LibrarySnapshot & { page?: "library" })
  | (GoalsSnapshot & { page?: "goals" })
  | (TasksSnapshot & { page?: "tasks" })
  | (IdeasSnapshot & { page?: "ideas" })
  | (ThreadsSnapshot & { page?: "threads" })
  | (FeedSnapshot & { page?: "feed" })
  | (SearchSnapshot & { page?: "search" })
  | (ChatSnapshot & { page?: "chat" });
