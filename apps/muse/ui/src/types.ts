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

// `armed` is the space whose unshare press the app has armed: the page paints
// the confirm from the app's own arm, never from a local flag that could drift.
export type LibrarySnapshot = {
  artifacts: Artifact[];
  total: number;
  armed?: string | null;
  delete_armed?: string | null;
  /// `loaded` separates an account with no artifacts from a listing nothing
  /// has read yet: the shell can be the only surface, and there no native
  /// paint fills the held rows.
  loaded?: boolean;
};

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
  /// `loaded` separates an account with no goals from a roster nothing has
  /// read yet: the shell can be the only surface, where no native paint fills
  /// the held rows.
  loaded?: boolean;
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

// One picture staged in the app's composer, as its chip reads it: the name,
// the pixel size and the byte count. The bytes never cross the channel -- the
// page asks the app to paste, and the app's own send carries the picture.
export type StagedChip = {
  name: string;
  width: number;
  height: number;
  bytes: number;
};

// One outcome word a pending approval offers: the service's own word and the
// native label, so the page cannot invent a sixth or reword one of the five.
export type ApprovalChoice = { word: string; label: string };

// One pending approval, projected from the same list the native panel paints:
// the native headline, the service's summary and detail rows verbatim.
export type ApprovalCard = {
  id: string;
  headline: string;
  summary: string;
  details: string[];
  choices: ApprovalChoice[];
};

// The approvals the panel last painted. `loaded` separates "the service
// reports none waiting" from "nothing has been read yet", and `note` carries
// the panel's own link-first sentence.
export type ApprovalsView = {
  cards: ApprovalCard[];
  loaded: boolean;
  note?: string | null;
};

// The run the account has next, as Upcoming names it: the schedule's own title
// and the app's own word for the distance, never a time this page computed.
export type NextRun = { title: string; when: string };

// Upcoming's own payload: the next run the last Tasks read named and the
// approvals the panel last painted. `schedules_loaded` separates "nothing is
// scheduled" from "no listing has been read yet".
export type UpcomingSnapshot = {
  next_run: NextRun | null;
  schedules_loaded: boolean;
  approvals: ApprovalsView;
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
  // The turn this message answers, named the way the transcript shows it:
  // the quoted text capped to a line, never a bare id.
  reply_to?: string | null;
};

// The Chat surface as this window opens it: the open thread, the rows the
// native transcript last built, and the composer's status line. `loaded`
// separates a thread nobody has read yet from an empty one.
// The chat's turn menu as the app offers it: the press's own label (the
// native button's words) and whether the latest turn is armed for the
// confirming press. The arming lives in the app, never here.
export type TurnMenu = {
  delete_label: string;
  armed: boolean;
};

export type ChatComposer = { draft: string; dictating: boolean };

export type ChatSnapshot = {
  thread?: string | null;
  title: string;
  account: string;
  status?: string | null;
  // Whether the app has a send in flight: the flag the native card swaps
  // Send for Stop on, so the page swaps the same circle.
  sending?: boolean;
  // Pictures staged in the app's composer, the open thread's unread count
  // from the service listing, and the pending approvals the panel painted.
  staged?: StagedChip[];
  unread?: number;
  approvals?: ApprovalsView;
  turn?: TurnMenu;
  // The composer the page and the native window share: the draft, typed in
  // either one or dictated into the native window, and whether a take is
  // running.
  composer?: { draft: string; dictating: boolean };
  rows: ChatRow[];
  loaded: boolean;
  error?: string | null;
};

// The saved account link as the Settings page shows it: paths only, never the
// session jar's contents.
export type SettingsLink = {
  linked: boolean;
  interpreter: string;
  session_folder: string;
};

// The updater's last answer, with the native card's own labels: the page
// offers the press the native window offers, in the same words.
export type SettingsUpdates = {
  appimage: boolean;
  status: string;
  busy: boolean;
  staged: boolean;
  check_label: string;
  action_label?: string | null;
};

// One policy row: the service's own label and value, plus the flip the chat
// panel offers when it offers one at all.
export type PolicyRow = {
  label: string;
  value: string;
  flip?: string | null;
};

// The policy the chat panel last read. `loaded` separates a panel that has not
// read yet from a service that published nothing.
export type SettingsPolicy = {
  loaded: boolean;
  rows: PolicyRow[];
  connectors: string[];
  withheld: number;
  note?: string | null;
};

// The Settings surface as this window opens it: three things the page cannot
// read on its own.
export type SettingsSnapshot = {
  link: SettingsLink;
  updates: SettingsUpdates;
  policy: SettingsPolicy;
};

// One granted folder as the Local host page shows it: the native row's own
// wording, plus the flags behind it.
export type LocalHostGrant = {
  label: string;
  root: string;
  writable: boolean;
  policy: boolean;
  text: string;
};

// One pending approval: the exact operation, path and proposed content the
// native card holds, and the countdown it recomputes on every repaint.
export type LocalHostCard = {
  // The broker's approval id: what Allow once and Deny name, never the path.
  id: string;
  operation: string;
  path: string;
  target?: string | null;
  preview?: string | null;
  deciding: boolean;
  countdown: string;
};

// The Local host page as this window opens it: the controller's read model.
export type LocalHostSnapshot = {
  identity: string;
  phase: string;
  notice: string;
  device?: string | null;
  grants: LocalHostGrant[];
  pending: LocalHostCard[];
  log: string[];
  // The saved write permission and the words for it, as the native page has
  // them: the switch's state and sentence travel as data.
  policy_on: boolean;
  policy_sentence: string;
  approvals_head: string;
  activity_head: string;
};

// One rail page as the shell lists it: the name the page components answer to
// and the title the native drawer prints.
export type ShellPage = { name: string; title: string };

// One row of the drawer's History: the thread's id, the exact words the native
// sidebar prints for it, and whether it is the open thread.
export type HistoryRow = { id: string; text: string; current: boolean };

// The shell's own payload: the page list, the account and build stamp the
// native drawer carries, and its History rows. Null History means the app's
// read of the listing failed -- unread, never an empty account. Page data is
// not here: each page asks its own snapshot.
export type ShellSnapshot = {
  pages: ShellPage[];
  account: string;
  build: string;
  history: HistoryRow[] | null;
};

// The sign-in surface as this window opens it: the app's detected interpreter,
// where it would save, the consent the app requires, and its own labels.
export type SigninSnapshot = {
  python: string;
  folder: string;
  consent: boolean;
  status: string;
  error: string;
  browser_open: boolean;
  title: string;
  hint: string;
  consent_sentence: string;
  connect_label: string;
  browser_label: string;
  cancel_label: string;
  choose_label: string;
};

// The first-run step: two lines and whether the app's setup returned. Nothing
// to press, so nothing to write.
export type FirstRunSnapshot = { head: string; sub: string; done: boolean };

// One bundle serves every window: the boot payload names the page it opens.
export type Boot =
  | (LibrarySnapshot & { page?: "library" })
  | (GoalsSnapshot & { page?: "goals" })
  | (TasksSnapshot & { page?: "tasks" })
  | (IdeasSnapshot & { page?: "ideas" })
  | (ThreadsSnapshot & { page?: "threads" })
  | (FeedSnapshot & { page?: "feed" })
  | (SearchSnapshot & { page?: "search" })
  | (SettingsSnapshot & { page?: "settings" })
  | (LocalHostSnapshot & { page?: "localhost" })
  | (ShellSnapshot & { page?: "shell" })
  | (SigninSnapshot & { page?: "signin" })
  | (FirstRunSnapshot & { page?: "firstrun" })
  | (ChatSnapshot & { page?: "chat" });
