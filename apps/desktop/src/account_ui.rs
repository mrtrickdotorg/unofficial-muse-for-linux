//! Native account UI. The controller can be tested without GTK or an account.
#![allow(unexpected_cfgs)]

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operation {
    Connect,
    Refresh,
    History,
    Send,
    Login,
    Cancel,
    Approvals,
    Decide,
    Permissions,
    Revoke,
    DeleteTask,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Ticket {
    epoch: u64,
    serial: u64,
    scope: Option<String>,
    limit: usize,
    operation: Operation,
}

struct Controller<A, H, T> {
    epoch: u64,
    serial: u64,
    busy: Option<Ticket>,
    /// Reply polls hold no `busy`: waiting for an answer must not block the
    /// composer, so they get their own slot and at most one runs at a time.
    polling: Option<Ticket>,
    /// A dispatch owns its own slot. The shared slot is held by reads and
    /// refreshes, so sharing it refused the composer whenever a background read
    /// happened to be running — which mid-turn is most of the time. Interjecting
    /// is the Mac behaviour; two unresolved dispatches at once are not.
    sending: Option<Ticket>,
    account: Option<A>,
    history: Vec<H>,
    tasks: Option<T>,
    scope: Option<String>,
    limit: usize,
    connection_notice: String,
    history_notice: String,
    /// Background detail (counts, bounded-window wording) shown as a tooltip so
    /// the strip above the composer stays quiet.
    history_detail: String,
    tasks_notice: String,
    send_notice: String,
    /// Live agent status, as reported by the service. Empty means nothing was
    /// reported; it is never filled in from a timer or a guess.
    live_status: String,
    /// Reply text received as `delta.text_append` events, shown while the service
    /// is still producing and dropped once the transcript carries the reply.
    live_draft: String,
    /// Highest delta `seq` already applied, so an overlapping poll cannot append
    /// the same fragment twice.
    live_delta_seq: u64,
    /// A reply is being produced and this window is listening to it. Set from
    /// the transcript (a reply row with no terminal status) or from the live
    /// stream, and cleared when the turn closes, so a window that opens in the
    /// middle of a run attaches to it instead of sitting idle.
    watching_run: bool,
    pending_send: Option<PendingSend>,
    /// Pending approvals exactly as the service last reported them. Empty means
    /// the last read found none; it never means the agent wants nothing.
    approvals: Vec<muse_account::Approval>,
    /// What the last read or decision reported, in the service's own terms.
    approval_notice: String,
    /// The request whose decision is in flight, so its card stops accepting a
    /// second press while the first is unanswered.
    deciding: Option<String>,
    /// The live status text that already caused a read of the pending list, so
    /// the same status cannot trigger a second one.
    approvals_sought: String,
    /// Grants the service last reported as still in force. Empty means the
    /// last read found none; it never means nothing was ever granted.
    grants: Vec<muse_account::ActiveGrant>,
    /// What the last grants read or revocation reported, in the service's
    /// own terms.
    grant_notice: String,
    /// The grant whose revocation is in flight, so its row stops accepting a
    /// second press while the first is unanswered.
    revoking: Option<String>,
    /// The schedule whose deletion is in flight, so its row stops accepting
    /// presses while the service is unanswered.
    deleting: Option<String>,
    /// The schedule armed for deletion by a first press. Deleting a schedule
    /// is permanent and has no undo, so the press that arms and the press
    /// that sends are two different presses.
    confirming_delete: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingSend {
    scope: Option<String>,
    text: String,
}

enum SendOutcome {
    Confirmed,
    NotSent(String),
    Uncertain(String),
}

fn account_surface_visibility(connected: bool) -> (bool, bool) {
    (connected, !connected)
}

/// The live event that carries reply text as it is produced. Fragments are raw
/// chunks ("anti", "-Sullan", " government"), so they are joined verbatim.
const DELTA_EVENT: &str = "delta.text_append";

/// Format an epoch-milliseconds expiry as a UTC date and time.
///
/// The service reports grant expiry only as epoch milliseconds; showing the
/// raw number would make a person do the conversion the app exists to do.
/// Civil-from-days is the standard days-to-date algorithm; the tests pin it
/// against known timestamps.
fn format_expiry_utc(expires_at_ms: i64) -> String {
    let days = expires_at_ms.div_euclid(86_400_000);
    let day_ms = expires_at_ms.rem_euclid(86_400_000);
    let (hour, minute) = (day_ms / 3_600_000, (day_ms % 3_600_000) / 60_000);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02} UTC")
}

/// True when the newest service event closes the turn. The status line describes
/// work in progress, so a completed assistant message ends it: leaving "Is
/// working" (or "Needs approval") on screen afterwards is a lie about the
/// present, not a stale cache.
fn history_closes_turn(rows: &[muse_account::HistoryMessage]) -> bool {
    rows.last().is_some_and(|row| {
        row.role == "assistant" && row.status.as_deref().is_some_and(reply_finished)
    })
}

/// True when the newest row is a reply still being produced.
///
/// The service leaves `status` absent while it is producing and sets a terminal
/// one when it finishes (observed live: the in-flight row at `72546` had no
/// status, the cancelled reply at `72488` ended `interrupted`). That absent
/// status is what tells a window which did not send the message that there is
/// still work to attach to.
fn reply_in_flight(rows: &[muse_account::HistoryMessage]) -> bool {
    rows.last()
        .is_some_and(|row| row.role == "assistant" && row.status.is_none())
}

/// Statuses the service uses to close a reply. `interrupted` is how a cancelled
/// run ends (observed live: `chat.cancel` left the reply at `interrupted`), so it
/// has to count as finished or a stopped turn would look permanently in progress.
fn reply_finished(status: &str) -> bool {
    const FINISHED: [&str; 4] = ["completed", "interrupted", "cancelled", "failed"];
    let status = status.trim();
    FINISHED
        .iter()
        .any(|word| status.eq_ignore_ascii_case(word))
}

/// True when a status describes presence or a finished task rather than work in
/// progress. Painting "completed" would report an end state as something the
/// agent is doing, which the transcript already states correctly.
fn settled_status(status: &str) -> bool {
    const SETTLED: [&str; 8] = [
        "online",
        "offline",
        "idle",
        "completed",
        "cancelled",
        "done",
        "interrupted",
        "failed",
    ];
    let status = status.trim();
    status.is_empty() || SETTLED.iter().any(|word| status.eq_ignore_ascii_case(word))
}

/// The line to show for one live event, if any. Only a reported status counts:
/// `delta.*` payloads carry reply text, and mirroring that above the transcript
/// would present a second, less accurate copy of the answer.
fn progress_text(event: &muse_account::LiveEvent) -> Option<String> {
    let status = event.status.as_deref()?;
    if settled_status(status) {
        return None;
    }
    Some(format!("Agent · {}", status.trim()))
}

fn presentation_text(text: &str) -> String {
    const WIDGET_MARKER: &str = "[[hatch_widget.";
    const WIDGET_FALLBACK: &str =
        "Muse returned an interactive widget that this Linux app cannot render yet.";
    if !text.contains(WIDGET_MARKER) {
        return text.to_owned();
    }

    let mut cleaned = String::with_capacity(text.len());
    let mut remainder = text;
    let mut removed_marker = false;
    loop {
        let Some(start) = remainder.find(WIDGET_MARKER) else {
            cleaned.push_str(remainder);
            break;
        };
        cleaned.push_str(&remainder[..start]);
        let marker_tail = &remainder[start + WIDGET_MARKER.len()..];
        let Some(end) = marker_tail.find("]]") else {
            removed_marker = true;
            break;
        };
        removed_marker = true;
        remainder = &marker_tail[end + 2..];
    }

    let normalized = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() && removed_marker {
        WIDGET_FALLBACK.to_owned()
    } else {
        normalized
    }
}

pub(crate) fn login_error_message(code: &str) -> &'static str {
    match code {
        "invalid_request" => "Sign-in helper rejected the selected account directory.",
        "missing_browser" | "browser_not_found" | "browser_unavailable" => {
            "No supported Chromium-compatible browser was available."
        }
        "missing_dependency" => {
            "The selected Python could not run the isolated browser sign-in helper."
        }
        "dependency_missing" | "sdk_unavailable" => {
            "The selected Python is missing the community Muse account or browser dependency."
        }
        "browser_launch_failed" => "The isolated browser could not be started.",
        "unsafe_devtools" => "The isolated browser debugging connection could not be verified.",
        "devtools_error" => "The isolated browser connection failed before sign-in completed.",
        "timeout" | "login_timeout" => "Browser sign-in expired before authentication completed.",
        "unsafe_config" | "insecure_config" => {
            "Browser sign-in needs a folder that belongs to you and is private (owner-only, no symlinks). Pick a new empty session folder under Advanced \u{2014} the default already qualifies \u{2014} or make that folder owner-only if it holds nothing else. The Muse CLI folder cannot be used for browser sign-in."
        }
        "cancelled" | "browser_closed" => {
            "Browser sign-in was cancelled or the isolated browser was closed."
        }
        "cleanup_failed" => {
            "Close the dedicated sign-in browser before trying again; browser cleanup could not be confirmed."
        }
        _ => {
            "The sign-in helper reported failure. Check the selected interpreter, isolated browser and account directory; raw output is hidden to protect credentials."
        }
    }
}

impl<A, H, T> Default for Controller<A, H, T> {
    fn default() -> Self {
        Self {
            epoch: 0,
            serial: 0,
            busy: None,
            polling: None,
            sending: None,
            account: None,
            history: Vec::new(),
            tasks: None,
            scope: None,
            limit: 50,
            connection_notice:
                "Ready when you are. Set up the connection above to load chats and reminders."
                    .into(),
            history_notice: String::new(),
            history_detail: String::new(),
            tasks_notice: "No scheduled-task history requested.".into(),
            send_notice: String::new(),
            live_status: String::new(),
            live_draft: String::new(),
            live_delta_seq: 0,
            watching_run: false,
            pending_send: None,
            approvals: Vec::new(),
            approval_notice: String::new(),
            deciding: None,
            approvals_sought: String::new(),
            grants: Vec::new(),
            grant_notice: String::new(),
            revoking: None,
            deleting: None,
            confirming_delete: None,
        }
    }
}

impl<A, H, T> Controller<A, H, T> {
    fn begin_login(&mut self, consent: bool) -> Option<Ticket> {
        if !consent || self.account.is_some() {
            self.connection_notice = "Consent to session access before opening sign-in. Disconnect first if already linked.".into();
            return None;
        }
        let ticket = self.issue(Operation::Login)?;
        self.connection_notice = "Sign in at official muse.ai in the isolated Chromium-compatible browser. No password or cookie is shown to this app.".into();
        Some(ticket)
    }

    fn finish_login(&mut self, ticket: &Ticket, result: Result<(), String>) -> bool {
        if !self.accept(ticket) {
            return false;
        }
        self.connection_notice = match result {
            Ok(()) => {
                "Signed in. Loading your chats, scheduled tasks and run history\u{2026}".into()
            }
            Err(error) => format!("Browser sign-in failed: {error}"),
        };
        true
    }

    fn begin_send(&mut self, text: &str) -> Option<Ticket> {
        self.account.as_ref()?;
        if self.pending_send.is_some() || text.trim().is_empty() {
            return None;
        }
        let ticket = self.issue(Operation::Send)?;
        self.pending_send = Some(PendingSend {
            scope: self.scope.clone(),
            text: text.into(),
        });
        self.send_notice.clear();
        Some(ticket)
    }

    /// Explains a refused send so the UI never fails silently. Every
    /// `begin_send` refusal maps to one of these actionable notices.
    fn describe_send_block(&self, text: &str) -> &'static str {
        if self.account.is_none() {
            return "Not connected. Connect your Muse session before sending.";
        }
        // In flight is not the same story as uncertain: nothing is unresolved yet,
        // the receipt simply has not arrived.
        if self.sending.is_some() && self.pending_send.is_some() {
            return "The previous message is still being dispatched. Wait for its receipt before sending.";
        }
        if self.pending_send.is_some() {
            return "A previous send is uncertain. Check its original shared history, then allow another send.";
        }
        if text.trim().is_empty() {
            return "Enter a message before sending.";
        }
        "The previous message is still being dispatched. Wait for its receipt before sending."
    }

    fn finish_send(&mut self, ticket: &Ticket, outcome: SendOutcome) -> bool {
        if !self.accept(ticket) {
            return false;
        }
        match outcome {
            SendOutcome::Confirmed => {
                // The reply landing in the transcript is the confirmation; a
                // sentence about it is one more line over the message box.
                self.pending_send = None;
                self.send_notice.clear();
            }
            SendOutcome::NotSent(error) => {
                self.pending_send = None;
                self.send_notice =
                    format!("Not sent: {error}. No automatic retry; the composer is unchanged.");
            }
            SendOutcome::Uncertain(error) => {
                let scope = ticket.scope.as_deref().unwrap_or("Main conversation");
                self.send_notice = format!(
                    "Delivery uncertain in {scope}: {error}. It may already be running. Do not repeat it; inspect this conversation's shared history before allowing another send. Original text and scope remain in memory."
                );
            }
        }
        true
    }

    fn acknowledge_send(&mut self) {
        if self.busy.is_none() && self.sending.is_none() && self.pending_send.is_some() {
            self.pending_send = None;
            self.send_notice.clear();
        }
    }

    fn finish_refresh(
        &mut self,
        ticket: &Ticket,
        account: Result<A, String>,
        history: Result<Vec<H>, String>,
        tasks: Result<T, String>,
    ) -> bool {
        if !self.accept(ticket) {
            return false;
        }
        match account {
            Ok(account) => {
                self.account = Some(account);
                self.connection_notice = "Account linked • shared conversations and tasks • refresh every 3 minutes while idle.".into();
            }
            Err(error) => {
                self.connection_notice = format!(
                    "Account refresh failed: {error}. Displayed account details are from the last successful read; disconnect to change sessions."
                )
            }
        }
        self.apply_history(history);
        match tasks {
            Ok(tasks) => {
                self.tasks = Some(tasks);
                // A fresh list is a new decision surface: an armed deletion
                // belonged to the list it was armed on.
                self.confirming_delete = None;
                self.tasks_notice = "Scheduled tasks and recent runs loaded from Muse (read-only, service-bounded list).".into();
            }
            Err(error) => {
                self.tasks_notice = format!(
                    "Scheduled-task history failed: {error}. Any displayed tasks are from the previous successful read."
                )
            }
        }
        true
    }

    fn select_thread(&mut self, scope: Option<String>) -> bool {
        if self.account.is_none()
            || self.scope == scope
            || self.sending.is_some()
            || self
                .busy
                .as_ref()
                .is_some_and(|t| matches!(t.operation, Operation::Login | Operation::Connect))
        {
            return false;
        }
        self.epoch = self.epoch.wrapping_add(1);
        self.busy = None;
        self.polling = None;
        self.scope = scope;
        self.history.clear();
        self.limit = 50;
        self.history_detail = "Selected conversation. History has not loaded yet.".into();
        self.history_notice.clear();
        true
    }

    fn begin_read(&mut self, older: bool, refresh: bool) -> Option<Ticket> {
        self.account.as_ref()?;
        if self.busy.is_some() || (older && self.limit >= 200) {
            return None;
        }
        if older {
            self.limit = (self.limit + 50).min(200);
        }
        self.issue(if refresh {
            Operation::Refresh
        } else {
            Operation::History
        })
    }

    /// Take the busy slot for a stop request. A stop is a write the user asked
    /// for, so it gets the same exclusive slot as a send: two racing stops would
    /// make the service's answer ambiguous.
    fn begin_cancel(&mut self) -> Option<Ticket> {
        self.account.as_ref()?;
        if self.busy.is_some() {
            return None;
        }
        self.serial = self.serial.wrapping_add(1);
        let ticket = Ticket {
            epoch: self.epoch,
            serial: self.serial,
            scope: self.scope.clone(),
            limit: self.limit,
            operation: Operation::Cancel,
        };
        self.busy = Some(ticket.clone());
        Some(ticket)
    }

    /// Take the busy slot for a read of the pending approvals. Listing is not
    /// deciding, but it shares the exclusive slot so a decision cannot race the
    /// list it was read from.
    fn begin_approvals(&mut self) -> Option<Ticket> {
        self.account.as_ref()?;
        self.issue(Operation::Approvals)
    }

    /// Take the busy slot for one decision, remembering which request it answers
    /// so no second answer can leave while the first is unanswered.
    fn begin_decide(&mut self, approval_id: &str) -> Option<Ticket> {
        self.account.as_ref()?;
        let ticket = self.issue(Operation::Decide)?;
        self.deciding = Some(approval_id.to_owned());
        Some(ticket)
    }

    fn begin_poll(&mut self) -> Option<Ticket> {
        self.account.as_ref()?;
        if self.busy.is_some() || self.polling.is_some() {
            return None;
        }
        self.serial = self.serial.wrapping_add(1);
        let ticket = Ticket {
            epoch: self.epoch,
            serial: self.serial,
            scope: self.scope.clone(),
            limit: self.limit,
            operation: Operation::History,
        };
        self.polling = Some(ticket.clone());
        Some(ticket)
    }

    /// True while `ticket` still owns a slot, whichever kind it is.
    fn owns(&self, ticket: &Ticket) -> bool {
        self.busy.as_ref() == Some(ticket) || self.polling.as_ref() == Some(ticket)
    }

    fn finish_history(&mut self, ticket: &Ticket, result: Result<Vec<H>, String>) -> bool {
        if !self.accept(ticket) {
            return false;
        }
        self.apply_history(result);
        true
    }

    fn finish_cancel(&mut self, ticket: &Ticket, notice: String) -> bool {
        if !self.accept(ticket) {
            return false;
        }
        self.send_notice = notice;
        true
    }

    fn finish_approvals(
        &mut self,
        ticket: &Ticket,
        result: Result<muse_account::PendingApprovals, String>,
    ) -> bool {
        if !self.accept(ticket) {
            return false;
        }
        match result {
            Ok(snapshot) => {
                self.approvals = snapshot.pending;
                self.approval_notice = if self.approvals.is_empty() {
                    "The service reports no approvals are waiting.".into()
                } else {
                    String::new()
                };
            }
            Err(error) => self.approval_notice = format!("Could not read approvals: {error}"),
        }
        true
    }

    /// Record a decision's answer. True only when the service took it, which is
    /// what lets the request leave the list; `accepted` still says nothing about
    /// whether the approved action then worked.
    fn finish_decide(
        &mut self,
        ticket: &Ticket,
        result: Result<muse_account::DecisionAnswer, String>,
    ) -> bool {
        if !self.accept(ticket) {
            return false;
        }
        self.deciding = None;
        match result {
            Ok(answer) => {
                let word = answer
                    .decision
                    .as_deref()
                    .unwrap_or(answer.requested.as_str())
                    .to_owned();
                self.approvals
                    .retain(|approval| approval.approval_id != answer.approval_id);
                self.approval_notice = if answer.accepted {
                    format!(
                        "The service accepted the decision ({word}). Check again to confirm what is still waiting."
                    )
                } else {
                    format!(
                        "The service did not accept the decision ({word}). The request is still waiting."
                    )
                };
                answer.accepted
            }
            Err(error) => {
                self.approval_notice = format!(
                    "Decision failed: {error}. The request may still be waiting; read the list again instead of pressing twice."
                );
                false
            }
        }
    }

    /// Take the busy slot for a read of the grants still in force. Listing is
    /// not revoking, but it shares the exclusive slot so a revocation cannot
    /// race the list it was read from.
    fn begin_permissions(&mut self) -> Option<Ticket> {
        self.account.as_ref()?;
        self.issue(Operation::Permissions)
    }

    /// Take the busy slot for one revocation, remembering which grant it
    /// names so no second revocation can leave while the first is unanswered.
    fn begin_revoke(&mut self, permission_id: &str) -> Option<Ticket> {
        self.account.as_ref()?;
        let ticket = self.issue(Operation::Revoke)?;
        self.revoking = Some(permission_id.to_owned());
        Some(ticket)
    }

    fn finish_permissions(
        &mut self,
        ticket: &Ticket,
        result: Result<muse_account::ActiveGrants, String>,
    ) -> bool {
        if !self.accept(ticket) {
            return false;
        }
        match result {
            Ok(snapshot) => {
                self.grants = snapshot.grants;
                self.grant_notice = if self.grants.is_empty() {
                    "The service reports no grants are in force.".into()
                } else {
                    String::new()
                };
            }
            Err(error) => self.grant_notice = format!("Could not read grants: {error}"),
        }
        true
    }

    /// Record a revocation's answer. True only when the service took it; the
    /// grant leaves the list on acceptance, and the re-read that follows is
    /// what proves whether it is actually gone — if it is still in force, it
    /// comes back in that read, in plain sight.
    fn finish_revoke(
        &mut self,
        ticket: &Ticket,
        result: Result<muse_account::RevokeAnswer, String>,
    ) -> bool {
        if !self.accept(ticket) {
            return false;
        }
        self.revoking = None;
        match result {
            Ok(answer) => {
                self.grants
                    .retain(|grant| grant.permission_id != answer.permission_id);
                self.grant_notice = if answer.accepted {
                    "The service accepted the revocation. Reading the list again to confirm the grant is gone.".into()
                } else {
                    "The service did not accept the revocation. The grant is still in force.".into()
                };
                answer.accepted
            }
            Err(error) => {
                self.grant_notice = format!(
                    "Revocation failed: {error}. The grant may still be in force; read the list again instead of pressing twice."
                );
                false
            }
        }
    }

    /// Arm one schedule for deletion. The first press only arms: deletion is
    /// permanent, so the confirming press is a separate, deliberate one, and
    /// arming a different schedule disarms the previous one.
    fn arm_delete(&mut self, job_id: &str) {
        self.confirming_delete = Some(job_id.to_owned());
        self.tasks_notice =
            "Press Delete again to permanently remove this scheduled task. This cannot be undone."
                .into();
    }

    /// Take the busy slot for one schedule deletion. Only an armed schedule
    /// can take it, so a stray first press can never reach the service.
    fn begin_delete_task(&mut self, job_id: &str) -> Option<Ticket> {
        self.account.as_ref()?;
        if self.confirming_delete.as_deref() != Some(job_id) {
            return None;
        }
        let ticket = self.issue(Operation::DeleteTask)?;
        self.confirming_delete = None;
        self.deleting = Some(job_id.to_owned());
        Some(ticket)
    }

    /// Record a deletion's answer. True only when the service took it, which
    /// is what triggers the refresh that follows: that re-read is what proves
    /// whether the schedule is actually gone — if it is still scheduled, it
    /// is still listed there, in plain sight.
    fn finish_delete_task(
        &mut self,
        ticket: &Ticket,
        result: Result<muse_account::DeleteAnswer, String>,
    ) -> bool {
        if !self.accept(ticket) {
            return false;
        }
        self.deleting = None;
        match result {
            Ok(answer) => {
                self.tasks_notice = if answer.accepted {
                    "The service accepted the deletion. Refreshing to confirm the schedule is gone."
                        .into()
                } else {
                    "The service did not accept the deletion. The schedule is still listed.".into()
                };
                answer.accepted
            }
            Err(error) => {
                self.tasks_notice = format!(
                    "Deletion failed: {error}. The schedule may still exist; refresh instead of pressing twice."
                );
                false
            }
        }
    }

    /// True the first time a live status says the agent is waiting for an
    /// approval. Keyed to that exact status text, so the alternating live reads
    /// cannot turn an unanswered request into a poll loop.
    fn claim_approval_read(&mut self) -> bool {
        if !self.live_status.to_ascii_lowercase().contains("approval") {
            // The agent stopped asking. Forgetting the key means a later request
            // phrased the same way is a new episode, not a repeat of this one.
            self.approvals_sought.clear();
            return false;
        }
        if self.approvals_sought == self.live_status {
            return false;
        }
        self.approvals_sought = self.live_status.clone();
        true
    }

    /// Attach this window to a reply that is being produced. Returns true only
    /// the first time, so repeated reads cannot restart the bounded watch.
    fn begin_watch(&mut self) -> bool {
        if self.watching_run {
            return false;
        }
        self.watching_run = true;
        true
    }

    /// Clears the live line once the transcript shows the turn has finished.
    fn note_turn_finished(&mut self) {
        self.watching_run = false;
        self.live_status.clear();
        self.approvals_sought.clear();
        // The transcript now carries the answer; keeping a draft beside it would
        // present the same reply twice, one of them possibly truncated.
        self.live_draft.clear();
        self.live_delta_seq = 0;
    }

    /// Accumulate streamed reply fragments.
    ///
    /// A repeated listen returns overlapping records, so anything at or below the
    /// last applied `seq` is skipped rather than appended twice; an event with no
    /// `seq` cannot be de-duplicated and is ignored on purpose.
    fn append_deltas(&mut self, events: &[muse_account::LiveEvent]) {
        let mut fresh: Vec<(u64, &str)> = Vec::new();
        for event in events {
            if event.event != DELTA_EVENT {
                continue;
            }
            let (Some(seq), Some(text)) = (event.seq, event.text.as_deref()) else {
                continue;
            };
            if seq > self.live_delta_seq {
                fresh.push((seq, text));
            }
        }
        fresh.sort_by_key(|(seq, _)| *seq);
        for (seq, text) in fresh {
            if seq <= self.live_delta_seq {
                continue;
            }
            self.live_delta_seq = seq;
            self.live_draft.push_str(text);
        }
        // A draft is a convenience, not an archive: bound it so a runaway stream
        // cannot grow the window without limit.
        const MAX_DRAFT_CHARS: usize = 20_000;
        let chars = self.live_draft.chars().count();
        if chars > MAX_DRAFT_CHARS {
            self.live_draft = self
                .live_draft
                .chars()
                .skip(chars - MAX_DRAFT_CHARS)
                .collect();
        }
    }

    /// A live status line only ever repeats a status an event reported. An empty
    /// read, or one that carried only settled states, leaves nothing on screen
    /// rather than reporting a finished state as present work.
    fn apply_live(&mut self, result: Result<Vec<muse_account::LiveEvent>, String>) {
        match &result {
            Ok(events) => {
                self.append_deltas(events);
                // A working status is live evidence of a run, and evidence is
                // enough to keep listening -- including for a run this window
                // never sent.
                if events.iter().any(|event| progress_text(event).is_some()) {
                    self.watching_run = true;
                }
                self.live_status = events
                    .iter()
                    .rev()
                    .find_map(progress_text)
                    .unwrap_or_default();
            }
            Err(error) => self.live_status = format!("Live status unavailable: {error}"),
        }
    }

    fn apply_history(&mut self, result: Result<Vec<H>, String>) {
        match result {
            Ok(history) => {
                self.history_detail = format!(
                    "{} messages from the latest {} service events (bounded window, not a full export).",
                    history.len(),
                    self.limit
                );
                self.history_notice.clear();
                self.history = history;
            }
            Err(error) => {
                self.history_notice = format!(
                    "Shared history failed: {error}. Any displayed messages are from the previous successful read."
                )
            }
        }
    }

    fn finish_connect(&mut self, ticket: &Ticket, result: Result<A, String>) -> bool {
        if !self.accept(ticket) {
            return false;
        }
        match result {
            Ok(account) => {
                self.account = Some(account);
                self.connection_notice =
                    "Account linked. Your chats and scheduled tasks now load from this Muse account."
                        .into();
            }
            Err(error) => self.connection_notice = format!("Connection failed: {error}"),
        }
        true
    }

    fn accept(&mut self, ticket: &Ticket) -> bool {
        let polled = self.polling.as_ref() == Some(ticket);
        let busy = self.busy.as_ref() == Some(ticket);
        let sending = self.sending.as_ref() == Some(ticket);
        if !polled && !busy && !sending {
            return false;
        }
        // Release the slot in every case: a poll or read that no longer matches
        // the scope must not keep polling from restarting.
        let current = ticket.epoch == self.epoch && ticket.scope == self.scope;
        if polled {
            self.polling = None;
        }
        if busy {
            self.busy = None;
        }
        if sending {
            self.sending = None;
        }
        current
    }

    fn disconnect(&mut self) {
        let had_pending_send = self.pending_send.is_some();
        self.polling = None;
        let epoch = self.epoch.wrapping_add(1);
        *self = Self::default();
        self.epoch = epoch;
        self.connection_notice =
            "Disconnected locally. Cloud history and other devices are unchanged.".into();
        if had_pending_send {
            self.send_notice = "A dispatched message may still execute remotely. It was not cancelled or retried. Reconnect and check its original conversation.".into();
        }
    }

    /// The service rejected the saved session. Expiry and revocation look the
    /// same from here, and neither can be fixed by retrying: drop the stale
    /// account data so no old chats are presented as current, stop background
    /// work, and put the sign-in control back in reach.
    fn session_expired(&mut self) {
        let had_pending_send = self.pending_send.is_some();
        self.polling = None;
        let epoch = self.epoch.wrapping_add(1);
        *self = Self::default();
        self.epoch = epoch;
        self.connection_notice =
            "Your Muse session expired. Sign in again to reload your chats and scheduled tasks. Nothing was sent and no history was changed."
                .into();
        if had_pending_send {
            self.send_notice = "A dispatched message may still execute remotely. It was not cancelled or retried. Reconnect and check its original conversation.".into();
        }
    }

    fn account_changed(&mut self) {
        self.disconnect();
        self.connection_notice = "The selected local Muse session changed account identity or agent. All displayed account data was cleared. Review the session and explicitly reconnect.".into();
    }

    fn begin_connect(&mut self, consent: bool) -> Option<Ticket> {
        if !consent {
            self.connection_notice =
                "Explicit consent is required; no account request was made.".into();
            return None;
        }
        if self.busy.is_some() || self.account.is_some() {
            return None;
        }
        self.issue(Operation::Connect)
    }

    /// The slot an operation must own. A dispatch takes `sending` so that a read
    /// or a refresh in flight cannot refuse the composer.
    fn slot_mut(&mut self, operation: Operation) -> &mut Option<Ticket> {
        if operation == Operation::Send {
            &mut self.sending
        } else {
            &mut self.busy
        }
    }

    fn issue(&mut self, operation: Operation) -> Option<Ticket> {
        if self.slot_mut(operation).is_some() {
            return None;
        }
        self.serial = self.serial.wrapping_add(1);
        let ticket = Ticket {
            epoch: self.epoch,
            serial: self.serial,
            scope: self.scope.clone(),
            limit: self.limit,
            operation,
        };
        *self.slot_mut(operation) = Some(ticket.clone());
        Some(ticket)
    }
}

#[cfg(not(account_controller_only))]
pub use native::{AccountPage, build};

#[cfg(not(account_controller_only))]
mod native {
    use super::*;
    use crate::account_login::{Cancellation, run_sign_in};
    use crate::local_host::{AccountContext, BoundaryHook, SharedDevice};
    use gtk::prelude::*;
    use gtk4 as gtk;
    use muse_account::{
        AccountClient, AccountError, AccountSnapshot, BridgeConfig, ErrorCode, HistoryMessage,
        TasksSnapshot,
    };
    use std::cell::{Cell, RefCell};
    #[cfg(unix)]
    use std::ffi::CString;
    use std::fs;
    use std::io::{Read, Write};
    #[cfg(unix)]
    use std::os::fd::{FromRawFd, RawFd};
    use std::path::{Path, PathBuf};
    use std::rc::Rc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, mpsc};
    #[cfg(test)]
    use std::time::Instant;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    type State = Controller<AccountSnapshot, HistoryMessage, TasksSnapshot>;

    pub struct AccountPage {
        pub root: gtk::Box,
        pub settings: gtk::Expander,
        pub cancel_login: Rc<dyn Fn()>,
        pub host_context: AccountContext,
        pub host_boundary: BoundaryHook,
        pub host_device: SharedDevice,
        #[cfg(test)]
        ui: Rc<Ui>,
    }

    struct Ui {
        state: RefCell<State>,
        client: RefCell<Option<AccountClient>>,
        connected_config: RefCell<Option<BridgeConfig>>,
        account_active: RefCell<Option<Arc<AtomicBool>>>,
        host_verified: Cell<bool>,
        host_boundary: BoundaryHook,
        host_device: SharedDevice,
        login_cancellation: RefCell<Option<Arc<Cancellation>>>,
        persistence_warning: RefCell<Option<String>>,
        rendering: Cell<bool>,
        reply_until: Cell<Option<std::time::Instant>>,
        root: gtk::Box,
        welcome: gtk::Box,
        pages: gtk::Notebook,
        settings: gtk::Expander,
        python: gtk::Entry,
        config_dir: gtk::Entry,
        consent: gtk::CheckButton,
        existing: gtk::Button,
        connect: gtk::Button,
        login: gtk::Button,
        disconnect: gtk::Button,
        connection_notice: gtk::Label,
        identity: gtk::Label,
        chip: gtk::Label,
        threads: gtk::ComboBoxText,
        threads_key: RefCell<String>,
        scope_label: gtk::Label,
        refresh: gtk::Button,
        older: gtk::Button,
        history: gtk::Box,
        scroll: gtk::ScrolledWindow,
        history_notice: gtk::Label,
        tasks: gtk::Box,
        tasks_notice: gtk::Label,
        send_notice: gtk::Label,
        acknowledge: gtk::Button,
        prompt: gtk::Entry,
        send: gtk::Button,
        cancel: gtk::Button,
        spinner: gtk::Spinner,
        live_notice: gtk::Label,
        draft: gtk::Label,
        /// Pending approval cards, one per request the service reported.
        approvals_box: gtk::Box,
        approval_notice: gtk::Label,
        approvals_refresh: gtk::Button,
        /// Grant rows, one per grant the service says is still in force.
        grants_box: gtk::Box,
        grant_notice: gtk::Label,
        grants_refresh: gtk::Button,
        /// Weak handle so a card built during render can call back into the page.
        self_weak: RefCell<std::rc::Weak<Ui>>,
        /// A watch is bounded: an absent status on an old row must not leave a
        /// background listen running for the rest of the session.
        watch_until: std::cell::Cell<Option<std::time::Instant>>,
        /// Alternates the live listen and the history poll during a watch, so
        /// neither starves the other.
        live_turn: std::cell::Cell<bool>,
    }

    enum Reply {
        Connect(Result<AccountSnapshot, String>, Option<BridgeConfig>),
        Refresh(
            Result<AccountSnapshot, String>,
            Result<Vec<HistoryMessage>, String>,
            Result<TasksSnapshot, String>,
        ),
        History(Result<Vec<HistoryMessage>, String>),
        Live(Result<Vec<muse_account::LiveEvent>, String>),
        Cancel(Result<muse_account::CancelAnswer, String>),
        Approvals(Result<muse_account::PendingApprovals, String>),
        Decide(Result<muse_account::DecisionAnswer, String>),
        Permissions(Result<muse_account::ActiveGrants, String>),
        Revoke(Result<muse_account::RevokeAnswer, String>),
        DeleteTask(Result<muse_account::DeleteAnswer, String>),
        Send(SendOutcome),
        SendConnectionError(SendOutcome),
        Login(Result<(), String>),
        AccountChanged,
        /// The saved session was rejected by the service: expired or revoked.
        AuthExpired,
    }

    fn label(text: &str) -> gtk::Label {
        let label = gtk::Label::new(Some(text));
        label.set_xalign(0.0);
        label.set_wrap(true);
        label.set_selectable(true);
        label.set_max_width_chars(110);
        label
    }

    fn home() -> PathBuf {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
    }

    fn config_home() -> Option<PathBuf> {
        if let Some(path) = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
        {
            return Some(path);
        }
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .map(|path| path.join(".config"))
    }

    fn persistent_config_home() -> Option<PathBuf> {
        config_home()
    }

    /// App-owned folder used by browser sign-in.
    fn app_account_dir() -> Option<PathBuf> {
        config_home().map(|path| path.join("unofficial-muse-for-linux/account"))
    }

    /// The Muse CLI's own folder. It is reserved for the CLI flow: the sign-in
    /// helper refuses it, so a browser sign-in must never leave it selected.
    fn cli_session_dir() -> Option<PathBuf> {
        config_home().map(|path| path.join("muse-cli"))
    }

    /// Folder a browser sign-in may use. An empty field or the CLI folder both
    /// fall back to the app's own folder; `None` means no home directory is
    /// available to derive one from.
    pub(super) fn browser_sign_in_dir(
        current: &str,
        app_dir: Option<&Path>,
        cli_dir: Option<&Path>,
    ) -> Option<String> {
        let current = current.trim();
        let cli_dir = cli_dir.map(|path| path.display().to_string());
        if current.is_empty() || Some(current.to_owned()) == cli_dir {
            return app_dir.map(|path| path.display().to_string());
        }
        Some(current.to_owned())
    }

    fn valid_saved_path(value: &str) -> bool {
        !value.is_empty()
            && value.len() <= 4096
            && !value.contains('\0')
            && Path::new(value).is_absolute()
    }

    fn saved_connection_text(python: &str, config_dir: &str) -> String {
        serde_json::json!({
            "python": python,
            "config_dir": config_dir,
        })
        .to_string()
    }

    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct SavedConnection {
        python: String,
        config_dir: String,
    }

    fn saved_connection_from_text(text: &str) -> Option<(String, String)> {
        let saved: SavedConnection = serde_json::from_str(text).ok()?;
        if !valid_saved_path(&saved.python) || !valid_saved_path(&saved.config_dir) {
            return None;
        }
        Some((saved.python, saved.config_dir))
    }

    #[cfg(unix)]
    fn current_uid() -> u32 {
        // SAFETY: geteuid has no preconditions and only reads the process credential.
        unsafe { libc::geteuid() }
    }

    #[cfg(unix)]
    fn close_fd(fd: RawFd) {
        // SAFETY: fd was returned by a successful libc open call in this module.
        unsafe {
            libc::close(fd);
        }
    }

    #[cfg(unix)]
    fn check_directory_fd(
        fd: RawFd,
        require_current_owner: bool,
        require_private_mode: bool,
    ) -> Result<(), String> {
        // SAFETY: zeroed is valid for libc::stat and fstat initializes every field.
        let mut metadata = unsafe { std::mem::zeroed::<libc::stat>() };
        // SAFETY: fd is borrowed for the duration of this fstat call and metadata is writable.
        if unsafe { libc::fstat(fd, &mut metadata) } != 0 {
            return Err("persistent config directory cannot be inspected".into());
        }
        if metadata.st_mode & libc::S_IFMT != libc::S_IFDIR {
            return Err("persistent config path is not a directory".into());
        }
        if metadata.st_mode & 0o022 != 0
            || (require_private_mode && metadata.st_mode & 0o077 != 0)
            || (!require_current_owner && metadata.st_uid != current_uid() && metadata.st_uid != 0)
            || (require_current_owner && metadata.st_uid != current_uid())
        {
            return Err("persistent config directory is not privately owned".into());
        }
        Ok(())
    }

    #[cfg(unix)]
    fn open_directory_path(path: &Path, create_missing: bool) -> Result<RawFd, String> {
        if !path.is_absolute()
            || path
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err("persistent config path is not safe".into());
        }
        let root = CString::new("/").expect("literal contains no NUL");
        // SAFETY: root is a valid NUL-terminated path and the flags request a directory fd.
        let mut current = unsafe {
            libc::open(
                root.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if current < 0 {
            return Err("persistent config root unavailable".into());
        }
        for component in path.components() {
            let std::path::Component::Normal(name) = component else {
                continue;
            };
            let Some(name) = name.to_str() else {
                close_fd(current);
                return Err("persistent config path is not valid UTF-8".into());
            };
            let name = CString::new(name).expect("filesystem component contains no NUL");
            // SAFETY: current is an open directory fd and name is NUL-terminated.
            let mut next = unsafe {
                libc::openat(
                    current,
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                )
            };
            if next < 0
                && create_missing
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT)
            {
                // SAFETY: current is an open directory fd and name is NUL-terminated.
                if unsafe { libc::mkdirat(current, name.as_ptr(), 0o700) } != 0
                    && std::io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST)
                {
                    close_fd(current);
                    return Err("could not create persistent config directory".into());
                }
                // SAFETY: current is an open directory fd and name is NUL-terminated.
                next = unsafe {
                    libc::openat(
                        current,
                        name.as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                    )
                };
            }
            if next < 0 {
                close_fd(current);
                return Err("persistent config directory unavailable".into());
            }
            if let Err(error) = check_directory_fd(next, false, false) {
                close_fd(next);
                close_fd(current);
                return Err(error);
            }
            close_fd(current);
            current = next;
        }
        if let Err(error) = check_directory_fd(current, true, false) {
            close_fd(current);
            return Err(error);
        }
        Ok(current)
    }

    #[cfg(unix)]
    fn open_child_directory(
        parent: RawFd,
        name: &str,
        create_missing: bool,
    ) -> Result<RawFd, String> {
        let name = CString::new(name).map_err(|_| "persistent config path is not safe")?;
        // SAFETY: parent is an open directory fd and name is NUL-terminated.
        let mut child = unsafe {
            libc::openat(
                parent,
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if child < 0
            && create_missing
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT)
        {
            // SAFETY: parent is an open directory fd and name is NUL-terminated.
            if unsafe { libc::mkdirat(parent, name.as_ptr(), 0o700) } != 0
                && std::io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST)
            {
                return Err("could not create persistent config directory".into());
            }
            // SAFETY: parent is an open directory fd and name is NUL-terminated.
            child = unsafe {
                libc::openat(
                    parent,
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                )
            };
        }
        if child < 0 {
            return Err("persistent config directory unavailable".into());
        }
        if let Err(error) = check_directory_fd(child, true, true) {
            close_fd(child);
            return Err(error);
        }
        Ok(child)
    }

    fn display_connection_notice(base: &str, busy: &str, warning: Option<&str>) -> String {
        format!("{base}{busy}{}", warning.unwrap_or(""))
    }

    #[cfg(unix)]
    fn load_saved_connection() -> Option<(String, String)> {
        let root = open_directory_path(&persistent_config_home()?, false).ok()?;
        let app = match open_child_directory(root, "unofficial-muse-for-linux", false) {
            Ok(fd) => fd,
            Err(_) => {
                close_fd(root);
                return None;
            }
        };
        let name = CString::new("account-connection.json").expect("literal contains no NUL");
        // SAFETY: app is an open directory fd and name is NUL-terminated.
        let file_fd = unsafe {
            libc::openat(
                app,
                name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if file_fd < 0 {
            close_fd(app);
            close_fd(root);
            return None;
        }
        // SAFETY: zeroed is valid for libc::stat and fstat initializes every field.
        let mut metadata = unsafe { std::mem::zeroed::<libc::stat>() };
        // SAFETY: file_fd is open and metadata is writable.
        let valid = unsafe { libc::fstat(file_fd, &mut metadata) } == 0
            && metadata.st_mode & libc::S_IFMT == libc::S_IFREG
            && metadata.st_uid == current_uid()
            && metadata.st_mode & 0o077 == 0
            && metadata.st_size >= 0
            && metadata.st_size <= 16 * 1024;
        if !valid {
            close_fd(file_fd);
            close_fd(app);
            close_fd(root);
            return None;
        }
        // SAFETY: file_fd is owned here and transferred to File exactly once.
        let mut file = unsafe { fs::File::from_raw_fd(file_fd) };
        let mut bytes = Vec::with_capacity(metadata.st_size as usize);
        if (&mut file)
            .take(16 * 1024 + 1)
            .read_to_end(&mut bytes)
            .is_err()
            || bytes.len() > 16 * 1024
        {
            close_fd(app);
            close_fd(root);
            return None;
        }
        drop(file);
        close_fd(app);
        close_fd(root);
        let text = String::from_utf8(bytes).ok()?;
        saved_connection_from_text(&text)
    }

    #[cfg(not(unix))]
    fn load_saved_connection() -> Option<(String, String)> {
        None
    }

    #[cfg(unix)]
    fn save_connection(python: &str, config_dir: &str) -> Result<(), String> {
        let root = open_directory_path(
            &persistent_config_home().ok_or("persistent config directory unavailable")?,
            true,
        )?;
        let app = match open_child_directory(root, "unofficial-muse-for-linux", true) {
            Ok(fd) => fd,
            Err(error) => {
                close_fd(root);
                return Err(error);
            }
        };
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temporary_name = format!(".account-connection.{nonce}.tmp");
        let temporary =
            CString::new(temporary_name.as_str()).expect("generated name contains no NUL");
        let target = CString::new("account-connection.json").expect("literal contains no NUL");
        // SAFETY: app is an open directory fd and both names are NUL-terminated.
        let file_fd = unsafe {
            libc::openat(
                app,
                temporary.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0o600,
            )
        };
        if file_fd < 0 {
            close_fd(app);
            close_fd(root);
            return Err("could not create persistent config file".into());
        }
        // SAFETY: file_fd is owned here and transferred to File exactly once.
        let mut file = unsafe { fs::File::from_raw_fd(file_fd) };
        let result = (|| {
            file.write_all(saved_connection_text(python, config_dir).as_bytes())
                .map_err(|_| "could not write persistent config file")?;
            file.sync_all()
                .map_err(|_| "could not sync persistent config file")?;
            drop(file);
            // SAFETY: app is an open directory fd and both names are NUL-terminated.
            if unsafe { libc::renameat(app, temporary.as_ptr(), app, target.as_ptr()) } != 0 {
                return Err("could not install persistent config file".into());
            }
            // The file is already installed if directory fsync is unavailable. Do not report
            // a save failure after rename, because the preference may now be visible.
            // SAFETY: app is an open directory fd.
            let _ = unsafe { libc::fsync(app) };
            Ok(())
        })();
        if result.is_err() {
            // SAFETY: app is an open directory fd and temporary is NUL-terminated.
            unsafe {
                libc::unlinkat(app, temporary.as_ptr(), 0);
            }
        }
        close_fd(app);
        close_fd(root);
        result
    }

    #[cfg(not(unix))]
    fn save_connection(_python: &str, _config_dir: &str) -> Result<(), String> {
        Err("persistent connection preferences are unavailable on this platform".into())
    }

    fn default_python() -> PathBuf {
        [
            home().join(".local/share/muse-mcp/.venv/bin/python"),
            home().join(".local/share/unofficial-muse-for-linux/account-bridge/.venv/bin/python"),
        ]
        .into_iter()
        .find(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from("/usr/bin/python3"))
    }

    /// A saved interpreter that is only the bare system fallback must not keep
    /// the bridge unusable after the community environment is installed: the
    /// system interpreter has no `muse_mcp`, which surfaces as a bare refusal.
    /// An explicit override always wins.
    pub(super) fn preferred_saved_python(saved: &str, discovered: &Path) -> String {
        let saved = saved.trim();
        let discovered = discovered.display().to_string();
        if saved.is_empty() || is_system_fallback(Path::new(saved)) {
            discovered
        } else {
            saved.to_owned()
        }
    }

    /// A bare system interpreter is the pre-bridge default, not a deliberate
    /// choice: it has no `muse_mcp`, so the bridge reports itself unavailable.
    pub(super) fn is_system_fallback(path: &Path) -> bool {
        matches!(
            path.to_str(),
            Some("/usr/bin/python3" | "/usr/local/bin/python3" | "/usr/bin/python")
        )
    }

    /// Only a bare system fallback is replaced, so an explicit override still
    /// wins over a bridge environment that happens to be installed.
    fn recovery_config(config: &BridgeConfig) -> Option<BridgeConfig> {
        if !is_system_fallback(&config.python) {
            return None;
        }
        let discovered = default_python();
        if discovered == config.python || !discovered.is_file() {
            return None;
        }
        Some(BridgeConfig {
            python: discovered,
            script: config.script.clone(),
            config_dir: config.config_dir.clone(),
        })
    }

    /// A remembered interpreter can outlive the environment it named. Retrying
    /// once with the discovered bridge environment turns a dead preference into
    /// a linked account instead of a refusal the user has to diagnose; the
    /// adopted interpreter is reported back so the saved preference is repaired.
    fn connect_with_recovery(
        config: &BridgeConfig,
        active: &Arc<AtomicBool>,
    ) -> (Result<AccountSnapshot, AccountError>, Option<BridgeConfig>) {
        let first = AccountClient::new(config.clone())
            .cancellable(active.clone())
            .connect();
        if first
            .as_ref()
            .is_err_and(|error| error.code == ErrorCode::BridgeUnavailable)
            && let Some(recovered) = recovery_config(config)
            && let Ok(snapshot) = AccountClient::new(recovered.clone())
                .cancellable(active.clone())
                .connect()
        {
            return (Ok(snapshot), Some(recovered));
        }
        (first, None)
    }

    /// The community bridge is a separate pinned install that is not bundled in
    /// the AppImage, so a missing bridge names the paths it tried and the command
    /// that fixes it rather than reporting only that it is unavailable.
    pub(super) fn connect_error_message(error: &AccountError, python: &Path) -> String {
        let text = error.to_string();
        if error.code == ErrorCode::BridgeUnavailable {
            return format!(
                "{text} Tried {}. Install the bridge with ./scripts/setup-account-bridge.sh, run in the same environment you launch this app from (a toolbox or distrobox container has its own Python), then enter the Python interpreter it prints under Advanced: interpreter and session folder.",
                python.display()
            );
        }
        text
    }

    fn script_path(name: &str) -> PathBuf {
        let mut roots = Vec::new();
        if let Some(appdir) = std::env::var_os("APPDIR")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
        {
            roots.push(appdir);
        }
        if let Ok(executable) = std::env::current_exe() {
            roots.extend(executable.ancestors().skip(1).map(Path::to_path_buf));
        }
        roots.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
        for root in &roots {
            for path in [
                root.join("usr/share/unofficial-muse-for-linux/account")
                    .join(name),
                root.join("scripts").join(name),
            ] {
                if path.is_file() {
                    return path;
                }
            }
        }
        PathBuf::from("/usr/share/unofficial-muse-for-linux/account").join(name)
    }

    pub fn build(chip: &gtk::Label) -> AccountPage {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        root.add_css_class("page-content");

        let welcome = gtk::Box::new(gtk::Orientation::Vertical, 8);
        welcome.add_css_class("welcome-card");
        let welcome_title = label("Connect your Muse account");
        welcome_title.add_css_class("welcome-title");
        welcome.append(&welcome_title);
        let welcome_copy = label(
            "Sign in to the Muse account you already use. A private browser window opens at muse.ai and only that session is saved, in this app's own folder. Your chats and scheduled tasks then load from that account. Your password is never entered in this app.",
        );
        welcome_copy.add_css_class("welcome-copy");
        welcome.append(&welcome_copy);
        let consent = gtk::CheckButton::with_label(
            "Allow this app to read this account's chats, scheduled tasks and run history.",
        );
        consent.set_halign(gtk::Align::Start);
        welcome.append(&consent);
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.add_css_class("toolbar");
        let login = gtk::Button::with_label("Sign in with Muse");
        login.add_css_class("suggested-action");
        login.add_css_class("primary-action");
        login.set_tooltip_text(Some(
            "Opens muse.ai in a private Chromium-compatible browser window, saves the session in this app's folder, then links the account. Not all installed browsers are supported.",
        ));
        let existing = gtk::Button::with_label("Use my existing Muse login");
        existing.set_tooltip_text(Some(
            "Links the Muse CLI session already on this computer instead of signing in again.",
        ));
        let connect = gtk::Button::with_label("Link account");
        connect.set_tooltip_text(Some(
            "Reads the selected session now. Tick the consent box first.",
        ));
        for button in [&login, &existing, &connect] {
            actions.append(button);
        }
        welcome.append(&actions);
        let helper = label(
            "\u{201c}Sign in with Muse\u{201d} is the usual choice: it links the account by itself once you finish signing in. \u{201c}Use my existing Muse login\u{201d} reads the Muse CLI session at ~/.config/muse-cli instead. Only one of the two folders is used at a time; interpreter or folder overrides are under Advanced.",
        );
        helper.add_css_class("dim-label");
        welcome.append(&helper);
        let disconnect_note = label(
            "Disconnecting clears only this app's memory: it does not sign you out on other devices, erase saved session files or delete cloud history.",
        );
        disconnect_note.add_css_class("dim-label");
        welcome.append(&disconnect_note);
        root.append(&welcome);

        let settings = gtk::Expander::new(Some("Advanced: interpreter and session folder"));
        settings.add_css_class("settings-disclosure");
        settings.set_expanded(false);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
        content.add_css_class("settings-content");
        content.append(&label(
            "Change these only if the defaults do not work. The account bridge needs a Python interpreter with the community muse-mcp package installed; browser sign-in needs Python 3.10+ with only the standard library. No password, token or cookie is entered in this app.",
        ));
        content.append(&label("Python executable (absolute path):"));
        let python = gtk::Entry::new();
        python.set_text(&default_python().display().to_string());
        content.append(&python);
        content.append(&label(
            "Session folder for browser sign-in (private to your user; never the Muse CLI folder):",
        ));
        let config_dir = gtk::Entry::new();
        config_dir.set_text(
            &app_account_dir()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
        );
        if let Some((saved_python, saved_config_dir)) = load_saved_connection() {
            python.set_text(&preferred_saved_python(&saved_python, &default_python()));
            config_dir.set_text(&saved_config_dir);
        }
        content.append(&config_dir);
        content.append(&label(
            "Both paths are remembered locally for the next launch. Session credentials stay in the selected folder and are never copied into this preference.",
        ));
        settings.set_child(Some(&content));
        root.append(&settings);
        let identity = label("Not linked");
        identity.add_css_class("assistant-name");
        root.append(&identity);
        let connection_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let spinner = gtk::Spinner::new();
        connection_row.append(&spinner);
        let connection_notice = label("");
        connection_notice.set_hexpand(true);
        connection_row.append(&connection_notice);
        // Kept outside the welcome card: that card is hidden while connected, and
        // unlinking must stay reachable from a linked account.
        let disconnect = gtk::Button::with_label("Disconnect");
        disconnect.set_tooltip_text(Some(
            "Stops this app from using the session. Other devices and cloud history are unchanged.",
        ));
        connection_row.append(&disconnect);
        root.append(&connection_row);

        let pages = gtk::Notebook::new();
        pages.set_vexpand(true);
        let chat = gtk::Box::new(gtk::Orientation::Vertical, 0);
        // The transcript and composer live in a centered ~768px column, like
        // the Mac/Windows client: full-width lines on a wide window are the
        // readability problem, not a feature.
        let column = gtk::Box::new(gtk::Orientation::Vertical, 10);
        column.set_halign(gtk::Align::Center);
        column.set_size_request(768, -1);
        column.set_vexpand(true);
        chat.append(&column);
        let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        toolbar.add_css_class("toolbar");
        let threads = gtk::ComboBoxText::new();
        threads.set_hexpand(true);
        threads.set_tooltip_text(Some("Main conversation or actual service-provided side chats. Archived sessions are labeled; no local/synthetic sessions are created."));
        let refresh = gtk::Button::with_label("Refresh shared data");
        let older = gtk::Button::with_label("Load older (+50, max 200)");
        toolbar.append(&threads);
        toolbar.append(&refresh);
        let approvals_refresh = gtk::Button::with_label("Check approvals");
        approvals_refresh.set_tooltip_text(Some(
            "Reads what the agent is currently asking to do. Reading never approves anything; only Allow or Deny does, one request at a time.",
        ));
        toolbar.append(&older);
        toolbar.append(&approvals_refresh);
        let grants_refresh = gtk::Button::with_label("Check allowed sites");
        grants_refresh.set_tooltip_text(Some(
            "Reads the grants the service says are still in force — what an earlier Allow always left behind. Reading never revokes anything; only a grant's own Revoke button does, one grant at a time.",
        ));
        toolbar.append(&grants_refresh);
        column.append(&toolbar);
        let scope_label = label("No conversation selected until account connection.");
        column.append(&scope_label);
        let live_notice = label("");
        live_notice.add_css_class("chat-status");
        live_notice.set_visible(false);
        column.append(&live_notice);
        let draft = label("");
        draft.add_css_class("chat-status");
        draft.set_visible(false);
        draft.set_tooltip_text(Some(
            "Reply text as the service streams it. The finished reply replaces it in the transcript.",
        ));
        column.append(&draft);
        let approvals_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
        approvals_box.set_visible(false);
        column.append(&approvals_box);
        let approval_notice = label("");
        approval_notice.add_css_class("chat-status");
        approval_notice.set_visible(false);
        column.append(&approval_notice);
        let grants_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
        grants_box.set_visible(false);
        column.append(&grants_box);
        let grant_notice = label("");
        grant_notice.add_css_class("chat-status");
        grant_notice.set_visible(false);
        column.append(&grant_notice);
        let history = gtk::Box::new(gtk::Orientation::Vertical, 10);
        let scroll = gtk::ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(true)
            .min_content_height(160)
            .build();
        scroll.set_child(Some(&history));
        column.append(&scroll);
        let history_notice = label("");
        history_notice.add_css_class("chat-status");
        column.append(&history_notice);
        let send_notice = label("");
        send_notice.add_css_class("chat-status");
        column.append(&send_notice);
        let acknowledge = gtk::Button::with_label(
            "I've checked the original shared history — allow another send",
        );
        column.append(&acknowledge);
        let composer = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        composer.add_css_class("composer");
        let prompt = gtk::Entry::new();
        prompt.set_hexpand(true);
        prompt.set_placeholder_text(Some("Message your existing Muse agent…"));
        let stop = gtk::Button::with_label("Stop agent");
        stop.set_tooltip_text(Some(
            "Asks the service to stop the run it has in flight. The service answers whether anything was actually stopped, and this window reports that answer instead of assuming it.",
        ));
        let send = gtk::Button::with_label("Send to Muse");
        send.add_css_class("suggested-action");
        send.add_css_class("primary-action");
        send.set_tooltip_text(Some(
            "Sends once to your existing Muse agent and may trigger external actions. No local files are uploaded automatically, and a send is never repeated without you.",
        ));
        composer.append(&prompt);
        composer.append(&stop);
        composer.append(&send);
        column.append(&composer);
        pages.append_page(&chat, Some(&gtk::Label::new(Some("Shared chat history"))));

        let task_page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let task_column = gtk::Box::new(gtk::Orientation::Vertical, 8);
        task_column.set_halign(gtk::Align::Center);
        task_column.set_size_request(768, -1);
        task_column.set_vexpand(true);
        task_page.append(&task_column);
        task_column.append(&label("Shared schedules & recent runs · read-only"));
        task_column.append(&label("These are the existing account's service-provided schedules and run records, not Linux-local jobs. This pane cannot create, edit, cancel or retry tasks. Use Refresh shared data or wait for the idle 30-second refresh."));
        let tasks_notice = label("");
        task_column.append(&tasks_notice);
        let tasks = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let tasks_scroll = gtk::ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(true)
            .build();
        tasks_scroll.set_child(Some(&tasks));
        task_column.append(&tasks_scroll);
        pages.append_page(
            &task_page,
            Some(&gtk::Label::new(Some("Scheduled tasks & runs"))),
        );
        root.append(&pages);

        let ui = Rc::new(Ui {
            state: RefCell::new(State::default()),
            client: RefCell::new(None),
            connected_config: RefCell::new(None),
            account_active: RefCell::new(None),
            host_verified: Cell::new(false),
            host_boundary: Rc::new(RefCell::new(None)),
            host_device: Rc::new(RefCell::new(None)),
            login_cancellation: RefCell::new(None),
            persistence_warning: RefCell::new(None),
            rendering: Cell::new(false),
            reply_until: Cell::new(None),
            root: root.clone(),
            welcome: welcome.clone(),
            pages: pages.clone(),
            settings: settings.clone(),
            python,
            config_dir,
            consent,
            existing,
            connect,
            login,
            disconnect,
            connection_notice,
            identity,
            chip: chip.clone(),
            threads,
            threads_key: RefCell::new(String::new()),
            scope_label,
            live_notice,
            draft,
            approvals_box,
            approval_notice,
            approvals_refresh,
            grants_box,
            grant_notice,
            grants_refresh,
            self_weak: RefCell::new(std::rc::Weak::new()),
            watch_until: std::cell::Cell::new(None),
            live_turn: std::cell::Cell::new(true),
            refresh,
            older,
            history,
            scroll,
            history_notice,
            tasks,
            tasks_notice,
            send_notice,
            acknowledge,
            prompt,
            send,
            cancel: stop.clone(),
            spinner,
        });
        {
            let u = Rc::clone(&ui);
            ui.existing.connect_clicked(move |_| {
                let selected = if let Some(cli_dir) = cli_session_dir() {
                    u.config_dir.set_text(&cli_dir.display().to_string());
                    true
                } else {
                    false
                };
                let chosen = u.config_dir.text().to_string();
                u.consent.set_active(false);
                u.state.borrow_mut().connection_notice = if selected {
                    format!(
                        "Using the Muse CLI session at {chosen}. Tick the consent box, then press Link account. Nothing has been read yet."
                    )
                } else {
                    "Set HOME or XDG_CONFIG_HOME before choosing a local Muse session.".into()
                };
                u.render();
            });
        }
        {
            let u = Rc::clone(&ui);
            ui.connect.connect_clicked(move |_| u.connect_account());
        }
        {
            let u = Rc::clone(&ui);
            ui.login.connect_clicked(move |_| u.sign_in());
        }
        {
            let u = Rc::clone(&ui);
            ui.disconnect.connect_clicked(move |_| {
                u.disconnect_local();
                u.consent.set_active(false);
                u.prompt.set_text("");
                u.render();
            });
        }
        {
            let u = Rc::clone(&ui);
            ui.refresh.connect_clicked(move |_| u.read(false, true));
        }
        {
            let u = Rc::clone(&ui);
            ui.older.connect_clicked(move |_| u.read(true, false));
        }
        {
            let u = Rc::clone(&ui);
            ui.threads.connect_changed(move |combo| {
                if u.rendering.get() {
                    return;
                }
                let Some(id) = combo.active_id() else {
                    return;
                };
                let scope = id.strip_prefix("thread:").map(str::to_string);
                let changed = u.state.borrow_mut().select_thread(scope);
                if changed {
                    u.prompt.set_text("");
                    u.render();
                    u.read(false, false);
                }
            });
        }
        {
            let u = Rc::clone(&ui);
            let stopper = Rc::clone(&ui);
            ui.cancel.connect_clicked(move |_| stopper.cancel_run());
            ui.send.connect_clicked(move |_| u.send_message());
            let enter = Rc::clone(&ui);
            ui.prompt.connect_activate(move |_| enter.send_message());
        }
        {
            let u = Rc::clone(&ui);
            ui.acknowledge.connect_clicked(move |_| {
                u.state.borrow_mut().acknowledge_send();
                // Never leave the uncertain text primed for an accidental resend.
                u.prompt.set_text("");
                u.render();
            });
        }
        for entry in [&ui.python, &ui.config_dir] {
            let u = Rc::clone(&ui);
            entry.connect_changed(move |_| {
                u.consent.set_active(false);
            });
        }
        {
            let u = Rc::clone(&ui);
            ui.approvals_refresh
                .connect_clicked(move |_| u.check_approvals());
        }
        {
            let u = Rc::clone(&ui);
            ui.grants_refresh
                .connect_clicked(move |_| u.check_permissions());
        }
        *ui.self_weak.borrow_mut() = Rc::downgrade(&ui);
        let weak = Rc::downgrade(&ui);
        let last_refresh = std::cell::Cell::new(std::time::Instant::now());
        gtk::glib::timeout_add_local(Duration::from_millis(1000), move || {
            let Some(ui) = weak.upgrade() else {
                return gtk::glib::ControlFlow::Break;
            };
            // No session inspection, network request or task polling before explicit connection.
            if ui.root.root().is_none() {
                return gtk::glib::ControlFlow::Break;
            }
            let now = std::time::Instant::now();
            let idle = ui.state.borrow().busy.is_none();
            let watching = ui.state.borrow().watching_run;
            if watching && ui.watch_until.get().is_none_or(|until| now >= until) {
                // The watch is bounded: a stale in-flight row must not leave a
                // background listen running for the rest of the session.
                ui.state.borrow_mut().watching_run = false;
                ui.render();
            }
            let watching = ui.state.borrow().watching_run;
            let awaiting = ui.reply_until.get().is_some_and(|until| now < until);
            if watching && idle {
                // Alternate the two bounded reads: twenty status listens must not
                // starve the history poll that decides when the turn is over.
                let live_next = !ui.live_turn.get();
                ui.live_turn.set(live_next);
                if live_next {
                    ui.poll_live();
                } else {
                    ui.poll_reply();
                }
                last_refresh.set(now);
            } else if awaiting && idle {
                ui.poll_reply();
                last_refresh.set(now);
            } else if last_refresh.get().elapsed() >= Duration::from_secs(180) {
                last_refresh.set(std::time::Instant::now());
                ui.read(false, true);
            }
            gtk::glib::ControlFlow::Continue
        });
        ui.render();
        let weak = Rc::downgrade(&ui);
        let cancel_login = Rc::new(move || {
            if let Some(ui) = weak.upgrade() {
                ui.disconnect_local();
            }
        });
        let weak = Rc::downgrade(&ui);
        let host_context = Rc::new(move || {
            let ui = weak.upgrade()?;
            if ui.state.borrow().account.is_some() && ui.host_verified.get() {
                ui.connected_config.borrow().clone()
            } else {
                None
            }
        });
        AccountPage {
            root,
            settings,
            cancel_login,
            host_context,
            host_boundary: ui.host_boundary.clone(),
            host_device: ui.host_device.clone(),
            #[cfg(test)]
            ui,
        }
    }

    impl Ui {
        fn stop_host(&self) {
            // The registered host owns transport-stop-before-broker-revoke ordering.
            let hook = self.host_boundary.borrow().clone();
            if let Some(hook) = hook {
                hook();
            }
            if let Some(device) = self.host_device.borrow_mut().take() {
                device.active.store(false, Ordering::SeqCst);
            }
        }
        fn disconnect_local(&self) {
            self.stop_host();
            self.host_verified.set(false);
            self.cancel_sign_in();
            if let Some(active) = self.account_active.borrow_mut().take() {
                active.store(false, Ordering::SeqCst);
            }
            self.state.borrow_mut().disconnect();
            self.client.borrow_mut().take();
            self.connected_config.borrow_mut().take();
        }

        fn cancel_sign_in(&self) {
            if let Some(cancellation) = self.login_cancellation.borrow_mut().take() {
                cancellation.cancel();
            }
        }

        fn config(&self) -> Result<BridgeConfig, String> {
            let python = PathBuf::from(self.python.text().as_str());
            let config_dir = PathBuf::from(self.config_dir.text().as_str());
            if !python.is_absolute() || !python.is_file() {
                return Err(
                    "Choose an existing absolute Python executable path. No request was made."
                        .into(),
                );
            }
            if !config_dir.is_absolute() {
                return Err(
                    "Choose an absolute Muse session directory. No request was made.".into(),
                );
            }
            let script = script_path("muse_account_bridge.py");
            if !script.is_file() {
                return Err("The packaged account bridge script is missing. Install it under usr/share/unofficial-muse-for-linux/account or run from a source checkout.".into());
            }
            Ok(BridgeConfig {
                python,
                script,
                config_dir,
            })
        }

        fn connect_account(self: &Rc<Self>) {
            let ticket = self
                .state
                .borrow_mut()
                .begin_connect(self.consent.is_active());
            let Some(ticket) = ticket else {
                self.render();
                return;
            };
            let config = match self.config() {
                Ok(config) => config,
                Err(error) => {
                    self.state.borrow_mut().finish_connect(&ticket, Err(error));
                    self.render();
                    return;
                }
            };
            let persistence_notice = save_connection(&self.python.text(), &self.config_dir.text())
                .err()
                .map(|error| format!(" · Connection paths were not saved locally: {error}."));
            *self.persistence_warning.borrow_mut() = persistence_notice;
            *self.connected_config.borrow_mut() = Some(config.clone());
            let active = Arc::new(AtomicBool::new(true));
            *self.account_active.borrow_mut() = Some(active.clone());
            let client = AccountClient::new(config.clone()).cancellable(active.clone());
            *self.client.borrow_mut() = Some(client.clone());
            self.state.borrow_mut().connection_notice =
                "Connecting to the explicitly selected Muse session…".into();
            self.launch(ticket, move || {
                let (result, adopted) = connect_with_recovery(&config, &active);
                Reply::Connect(
                    result.map_err(|error| connect_error_message(&error, &config.python)),
                    adopted,
                )
            });
        }

        fn sign_in(self: &Rc<Self>) {
            // The sign-in helper refuses a folder named `muse-cli`, so a browser
            // sign-in must never reuse the CLI shortcut's folder. Switching the
            // folder changes the session and therefore voids consent, exactly
            // like an edited path; the next press of this button starts signing in.
            let app_dir = app_account_dir();
            let cli_dir = cli_session_dir();
            let current = self.config_dir.text().to_string();
            let switched = browser_sign_in_dir(&current, app_dir.as_deref(), cli_dir.as_deref())
                .filter(|folder| folder != &current);
            if let Some(folder) = switched {
                self.config_dir.set_text(&folder);
                self.state.borrow_mut().connection_notice =
                    "Switched to this app's own session folder for browser sign-in. Tick the consent box, then press Sign in with Muse again.".into();
                self.render();
                return;
            }
            let ticket = self
                .state
                .borrow_mut()
                .begin_login(self.consent.is_active());
            let Some(ticket) = ticket else {
                self.render();
                return;
            };
            let config = match self.config() {
                Ok(config) => config,
                Err(error) => {
                    self.state.borrow_mut().finish_login(&ticket, Err(error));
                    self.render();
                    return;
                }
            };
            let persistence_notice = save_connection(&self.python.text(), &self.config_dir.text())
                .err()
                .map(|error| format!(" · Connection paths were not saved locally: {error}."));
            *self.persistence_warning.borrow_mut() = persistence_notice;
            let script = script_path("muse_account_login.py");
            let cancellation = Arc::new(Cancellation::default());
            *self.login_cancellation.borrow_mut() = Some(Arc::clone(&cancellation));
            self.launch(ticket, move || {
                Reply::Login(run_sign_in(
                    &config.python,
                    &script,
                    &config.config_dir,
                    &cancellation,
                ))
            });
        }

        fn expected_identity(&self) -> Option<(String, String)> {
            self.state
                .borrow()
                .account
                .as_ref()
                .map(|a| (a.identity_name.clone(), a.vm_id.clone()))
        }

        fn read(self: &Rc<Self>, older: bool, refresh: bool) {
            let Some(client) = self.client.borrow().clone() else {
                return;
            };
            let Some(identity) = self.expected_identity() else {
                return;
            };
            let Some(ticket) = self.state.borrow_mut().begin_read(older, refresh) else {
                return;
            };
            let request = ticket.clone();
            self.launch(ticket, move || {
                let account = match client.connect() {
                    Ok(account) => Ok(account),
                    // A rejected session is a state change, not a failed read.
                    Err(error) if error.code == ErrorCode::AuthRequired => {
                        return Reply::AuthExpired;
                    }
                    Err(error) => Err(error.to_string()),
                };
                match &account {
                    Ok(a)
                        if (a.identity_name.as_str(), a.vm_id.as_str())
                            != (identity.0.as_str(), identity.1.as_str()) =>
                    {
                        return Reply::AccountChanged;
                    }
                    Err(error) => {
                        return if refresh {
                            Reply::Refresh(
                                account.clone(),
                                Err(format!("account verification failed: {error}")),
                                Err(format!("account verification failed: {error}")),
                            )
                        } else {
                            Reply::History(Err(format!("account verification failed: {error}")))
                        };
                    }
                    _ => {}
                }
                if refresh {
                    let (history, tasks) = std::thread::scope(|scope| {
                        let history = scope.spawn(|| {
                            client
                                .history(request.scope.as_deref(), request.limit)
                                .map_err(|e| e.to_string())
                        });
                        let tasks = scope.spawn(|| client.tasks().map_err(|e| e.to_string()));
                        (
                            history.join().unwrap_or_else(|_| {
                                Err("History worker stopped unexpectedly.".into())
                            }),
                            tasks.join().unwrap_or_else(|_| {
                                Err("Task-history worker stopped unexpectedly.".into())
                            }),
                        )
                    });
                    Reply::Refresh(account, history, tasks)
                } else {
                    Reply::History(
                        match client.history(request.scope.as_deref(), request.limit) {
                            Err(error) if error.code == ErrorCode::AuthRequired => {
                                return Reply::AuthExpired;
                            }
                            other => other.map_err(|e| e.to_string()),
                        },
                    )
                }
            });
        }

        /// History only, and never in the `busy` slot: a poll runs in the
        /// background so the composer stays usable while an answer is awaited.
        /// Each request holds one connection and returns the moment anything
        /// newer than the transcript we already show appears.
        fn poll_reply(self: &Rc<Self>) {
            let Some(client) = self.client.borrow().clone() else {
                return;
            };
            let Some(ticket) = self.state.borrow_mut().begin_poll() else {
                return;
            };
            let request = ticket.clone();
            let seen = self
                .state
                .borrow()
                .history
                .last()
                .map(|row| row.message_id.clone());
            self.launch(ticket, move || {
                let result = client.watch_after(
                    request.scope.as_deref(),
                    request.limit,
                    seen.as_deref(),
                    Duration::from_secs(45),
                );
                // A long watch is where an expired session usually surfaces.
                match result {
                    Err(error) if error.code == ErrorCode::AuthRequired => Reply::AuthExpired,
                    other => Reply::History(other.map_err(|e| e.to_string())),
                }
            });
        }

        /// One bounded live read feeding the status line. Never the reply: the
        /// service does not deliver threaded replies as live events, so the
        /// transcript keeps polling history while this reports progress.
        /// Ask the service to stop the run in flight.
        /// Read the pending approvals. Explicit: the service is asked when the
        /// reader presses, and the answer is the service's, never a guess.
        fn check_approvals(self: &Rc<Self>) {
            let Some(client) = self.client.borrow().clone() else {
                self.state.borrow_mut().approval_notice =
                    "Not connected. Connect your Muse session before reading approvals.".into();
                self.render();
                return;
            };
            let Some(ticket) = self.state.borrow_mut().begin_approvals() else {
                return;
            };
            self.launch(ticket, move || match client.approvals() {
                Ok(snapshot) => Reply::Approvals(Ok(snapshot)),
                Err(error) if error.code == ErrorCode::AccountChanged => Reply::AccountChanged,
                Err(error) if error.code == ErrorCode::AuthRequired => Reply::AuthExpired,
                Err(error) => Reply::Approvals(Err(error.to_string())),
            });
        }

        /// Send exactly the decision the reader pressed, once.
        fn decide(self: &Rc<Self>, approval_id: String, decision: muse_account::Decision) {
            let Some(client) = self.client.borrow().clone() else {
                self.state.borrow_mut().approval_notice =
                    "Not connected. Connect your Muse session before deciding an approval.".into();
                self.render();
                return;
            };
            let Some(ticket) = self.state.borrow_mut().begin_decide(&approval_id) else {
                return;
            };
            self.launch(ticket, move || {
                match client.decide(&approval_id, decision) {
                    Ok(answer) => Reply::Decide(Ok(answer)),
                    Err(error) if error.code == ErrorCode::AccountChanged => Reply::AccountChanged,
                    Err(error) if error.code == ErrorCode::AuthRequired => Reply::AuthExpired,
                    Err(error) => Reply::Decide(Err(error.to_string())),
                }
            });
        }

        /// Read the grants still in force. Explicit: the service is asked
        /// when the reader presses, and the answer is the service's.
        fn check_permissions(self: &Rc<Self>) {
            let Some(client) = self.client.borrow().clone() else {
                self.state.borrow_mut().grant_notice =
                    "Not connected. Connect your Muse session before reading grants.".into();
                self.render();
                return;
            };
            let Some(ticket) = self.state.borrow_mut().begin_permissions() else {
                return;
            };
            self.launch(ticket, move || match client.permissions() {
                Ok(snapshot) => Reply::Permissions(Ok(snapshot)),
                Err(error) if error.code == ErrorCode::AccountChanged => Reply::AccountChanged,
                Err(error) if error.code == ErrorCode::AuthRequired => Reply::AuthExpired,
                Err(error) => Reply::Permissions(Err(error.to_string())),
            });
        }

        /// Send exactly the revocation the reader pressed, once.
        fn revoke(self: &Rc<Self>, permission_id: String) {
            let Some(client) = self.client.borrow().clone() else {
                self.state.borrow_mut().grant_notice =
                    "Not connected. Connect your Muse session before revoking a grant.".into();
                self.render();
                return;
            };
            let Some(ticket) = self.state.borrow_mut().begin_revoke(&permission_id) else {
                return;
            };
            self.launch(ticket, move || match client.revoke(&permission_id) {
                Ok(answer) => Reply::Revoke(Ok(answer)),
                Err(error) if error.code == ErrorCode::AccountChanged => Reply::AccountChanged,
                Err(error) if error.code == ErrorCode::AuthRequired => Reply::AuthExpired,
                Err(error) => Reply::Revoke(Err(error.to_string())),
            });
        }

        /// Delete one scheduled task. The first press only arms the row —
        /// deletion is permanent — and the second press sends exactly one
        /// deletion for the schedule the reader armed.
        fn delete_task(self: &Rc<Self>, job_id: String) {
            let Some(client) = self.client.borrow().clone() else {
                self.state.borrow_mut().tasks_notice =
                    "Not connected. Connect your Muse session before deleting a schedule.".into();
                self.render();
                return;
            };
            {
                let mut state = self.state.borrow_mut();
                if state.confirming_delete.as_deref() != Some(job_id.as_str()) {
                    state.arm_delete(&job_id);
                    drop(state);
                    self.render();
                    return;
                }
            }
            let Some(ticket) = self.state.borrow_mut().begin_delete_task(&job_id) else {
                return;
            };
            self.launch(ticket, move || match client.delete_task(&job_id) {
                Ok(answer) => Reply::DeleteTask(Ok(answer)),
                Err(error) if error.code == ErrorCode::AccountChanged => Reply::AccountChanged,
                Err(error) if error.code == ErrorCode::AuthRequired => Reply::AuthExpired,
                Err(error) => Reply::DeleteTask(Err(error.to_string())),
            });
        }

        fn cancel_run(self: &Rc<Self>) {
            let Some(client) = self.client.borrow().clone() else {
                self.state.borrow_mut().send_notice =
                    "Not connected. Connect your Muse session before stopping a run.".into();
                self.render();
                return;
            };
            let Some(ticket) = self.state.borrow_mut().begin_cancel() else {
                return;
            };
            self.launch(ticket, move || match client.cancel() {
                Ok(answer) => Reply::Cancel(Ok(answer)),
                Err(error) if error.code == ErrorCode::AccountChanged => Reply::AccountChanged,
                Err(error) if error.code == ErrorCode::AuthRequired => Reply::AuthExpired,
                Err(error) => Reply::Cancel(Err(error.to_string())),
            });
        }

        fn poll_live(self: &Rc<Self>) {
            let Some(client) = self.client.borrow().clone() else {
                return;
            };
            let Some(ticket) = self.state.borrow_mut().begin_poll() else {
                return;
            };
            self.launch(ticket, move || {
                let result = client.activity(Duration::from_secs(4));
                match result {
                    Err(error) if error.code == ErrorCode::AuthRequired => Reply::AuthExpired,
                    other => Reply::Live(other.map(|live| live.events).map_err(|e| e.to_string())),
                }
            });
        }

        fn send_message(self: &Rc<Self>) {
            let Some(client) = self.client.borrow().clone() else {
                self.state.borrow_mut().send_notice =
                    "Not connected. Connect your Muse session before sending.".into();
                self.render();
                return;
            };
            let Some(identity) = self.expected_identity() else {
                self.state.borrow_mut().send_notice =
                    "Not connected. Connect your Muse session before sending.".into();
                self.render();
                return;
            };
            let text = self.prompt.text().to_string();
            let Some(ticket) = self.state.borrow_mut().begin_send(&text) else {
                let reason = self.state.borrow().describe_send_block(&text);
                self.state.borrow_mut().send_notice = reason.into();
                self.render();
                return;
            };
            let request = ticket.clone();
            let device = self
                .host_device
                .borrow()
                .clone()
                .filter(|d| d.active.load(Ordering::SeqCst));
            let client = if let Some(device) = &device {
                client.cancellable(device.active.clone())
            } else {
                client
            };
            self.launch(ticket, move || {
                // The dispatch verifies the session itself, so one account check
                // rides along instead of a separate `connect` round trip that
                // also lists every conversation.
                let dispatch = match device {
                    Some(device) if device.active.load(Ordering::SeqCst) => client
                        .send_from_device_verified(
                            request.scope.as_deref(),
                            &text,
                            &device.node_id,
                            &identity.1,
                            &identity.0,
                        ),
                    Some(_) => {
                        return Reply::Send(SendOutcome::NotSent(
                            "Local host stopped before dispatch.".into(),
                        ));
                    }
                    None => client.send_verified(
                        request.scope.as_deref(),
                        &text,
                        &identity.1,
                        &identity.0,
                    ),
                };
                let result = match dispatch {
                    Ok(receipt) if !receipt.sent => {
                        SendOutcome::NotSent("Service did not accept the dispatch.".into())
                    }
                    Ok(receipt) if receipt.confirmed => SendOutcome::Confirmed,
                    Ok(_) => SendOutcome::Uncertain(
                        "Service dispatch returned without scoped-history confirmation.".into(),
                    ),
                    Err(error) if error.code == muse_account::ErrorCode::AccountChanged => {
                        return Reply::AccountChanged;
                    }
                    Err(error) => {
                        return Reply::SendConnectionError(if error.uncertain {
                            SendOutcome::Uncertain(error.to_string())
                        } else {
                            SendOutcome::NotSent(error.to_string())
                        });
                    }
                };
                Reply::Send(result)
            });
            // Status is about work in progress, so ask shortly after the
            // dispatch and once more while the answer is being produced. Each
            // attempt is bounded, and a busy slot simply skips it.
            for delay in [2_000u64, 8_000] {
                let weak = Rc::downgrade(self);
                gtk::glib::timeout_add_local(Duration::from_millis(delay), move || {
                    if let Some(ui) = weak.upgrade() {
                        ui.poll_live();
                    }
                    gtk::glib::ControlFlow::Break
                });
            }
        }

        fn launch(self: &Rc<Self>, ticket: Ticket, work: impl FnOnce() -> Reply + Send + 'static) {
            self.render();
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let reply = work();
                let _ = tx.send(reply);
            });
            let weak = Rc::downgrade(self);
            gtk::glib::timeout_add_local(Duration::from_millis(80), move || {
                let Some(ui) = weak.upgrade() else {
                    return gtk::glib::ControlFlow::Break;
                };
                if !ui.state.borrow().owns(&ticket) {
                    // A disconnected account or different thread must never receive this result.
                    return gtk::glib::ControlFlow::Break;
                }
                let reply = match rx.try_recv() {
                    Ok(reply) => reply,
                    Err(mpsc::TryRecvError::Empty) => return gtk::glib::ControlFlow::Continue,
                    Err(mpsc::TryRecvError::Disconnected) => match ticket.operation {
                        Operation::Connect => {
                            Reply::Connect(Err("Account worker stopped unexpectedly.".into()), None)
                        }
                        Operation::Login => {
                            Reply::Login(Err("Sign-in worker stopped unexpectedly.".into()))
                        }
                        Operation::Send => Reply::SendConnectionError(SendOutcome::Uncertain(
                            "Dispatch worker stopped unexpectedly.".into(),
                        )),
                        Operation::Refresh => Reply::Refresh(
                            Err("Account worker stopped unexpectedly.".into()),
                            Err("History worker stopped unexpectedly.".into()),
                            Err("Task-history worker stopped unexpectedly.".into()),
                        ),
                        Operation::History => {
                            Reply::History(Err("History worker stopped unexpectedly.".into()))
                        }
                        Operation::Cancel => {
                            Reply::Cancel(Err("Stop worker stopped unexpectedly.".into()))
                        }
                        Operation::Approvals => {
                            Reply::Approvals(Err("Approvals worker stopped unexpectedly.".into()))
                        }
                        Operation::Decide => {
                            Reply::Decide(Err("Decision worker stopped unexpectedly.".into()))
                        }
                        Operation::Permissions => {
                            Reply::Permissions(Err("Grants worker stopped unexpectedly.".into()))
                        }
                        Operation::Revoke => {
                            Reply::Revoke(Err("Revocation worker stopped unexpectedly.".into()))
                        }
                        Operation::DeleteTask => {
                            Reply::DeleteTask(Err("Deletion worker stopped unexpectedly.".into()))
                        }
                    },
                };
                ui.complete(&ticket, reply);
                gtk::glib::ControlFlow::Break
            });
        }

        fn complete(self: &Rc<Self>, ticket: &Ticket, reply: Reply) {
            if !self.state.borrow().owns(ticket) {
                return;
            }
            let mut refresh_after = false;
            let mut poll_after_cancel = false;
            let mut watch_after = false;
            // A completed browser sign-in hands straight over to the explicit
            // link step: same consent, same bridge validation, one less click.
            let mut link_after_login = false;
            let mut reread_approvals = false;
            let mut reread_grants = false;
            let mut approvals_after = false;
            let send_connection_error = matches!(&reply, Reply::SendConnectionError(_));
            let auth_expired = matches!(&reply, Reply::AuthExpired);
            let send_confirmed = matches!(&reply, Reply::Send(SendOutcome::Confirmed));
            // A dispatched message is answered by the service; poll history
            // quickly for a while so the reply appears in seconds.
            if matches!(&reply, Reply::Send(SendOutcome::Confirmed)) {
                self.reply_until
                    .set(Some(std::time::Instant::now() + Duration::from_secs(90)));
            }
            // Stop immediately on account verification errors or identity changes,
            // before any old account result is installed or rendered.
            if matches!(
                &reply,
                Reply::AccountChanged
                    | Reply::AuthExpired
                    | Reply::Refresh(Err(_), _, _)
                    | Reply::Refresh(_, Err(_), _)
                    | Reply::Refresh(_, _, Err(_))
                    | Reply::History(Err(_))
                    | Reply::Send(SendOutcome::NotSent(_))
                    | Reply::SendConnectionError(_)
            ) {
                self.host_verified.set(false);
                self.stop_host();
            }
            if matches!(
                &reply,
                Reply::Connect(Ok(_), _) | Reply::Refresh(Ok(_), Ok(_), Ok(_))
            ) {
                self.host_verified.set(true);
            }
            {
                let mut state = self.state.borrow_mut();
                match reply {
                    Reply::Connect(result, adopted) => {
                        refresh_after = result.is_ok();
                        if refresh_after && let Some(adopted) = adopted {
                            // Repair the stale preference so the next launch and
                            // every later request use the working interpreter.
                            self.python.set_text(&adopted.python.display().to_string());
                            let _ = save_connection(&self.python.text(), &self.config_dir.text());
                            if let Some(active) = self.account_active.borrow().clone() {
                                *self.client.borrow_mut() =
                                    Some(AccountClient::new(adopted.clone()).cancellable(active));
                            }
                            *self.connected_config.borrow_mut() = Some(adopted);
                            state.connection_notice =
                                "Connected with the installed account bridge environment.".into();
                        }
                        state.finish_connect(ticket, result);
                        if !refresh_after {
                            self.client.borrow_mut().take();
                        }
                    }
                    Reply::Refresh(account, history, tasks) => {
                        let closed = matches!(&history, Ok(rows) if history_closes_turn(rows));
                        let in_flight = matches!(&history, Ok(rows) if reply_in_flight(rows));
                        state.finish_refresh(ticket, account, history, tasks);
                        if closed {
                            state.note_turn_finished();
                        }
                        watch_after = in_flight && state.begin_watch();
                    }
                    Reply::History(result) => {
                        let closed = matches!(&result, Ok(rows) if history_closes_turn(rows));
                        let in_flight = matches!(&result, Ok(rows) if reply_in_flight(rows));
                        state.finish_history(ticket, result);
                        if closed {
                            state.note_turn_finished();
                        }
                        watch_after = in_flight && state.begin_watch();
                    }
                    Reply::Live(result) => {
                        state.apply_live(result);
                        // A status saying the agent is waiting is the one moment
                        // the list is worth reading unsolicited; the claim keeps
                        // that to once per status, never a standing poll.
                        approvals_after = state.claim_approval_read();
                    }
                    Reply::Cancel(result) => {
                        // The service's answer decides the wording: pressing
                        // stop is a request, never proof that work stopped.
                        let stopped = matches!(&result, Ok(answer) if answer.cancelled);
                        let notice = match &result {
                            Ok(answer) if answer.cancelled => {
                                "The service reports it stopped the current run.".to_string()
                            }
                            Ok(_) => "The service reports nothing was running to stop.".to_string(),
                            Err(error) => format!(
                                "Stop request failed: {error}. The run may still be in progress."
                            ),
                        };
                        if state.finish_cancel(ticket, notice) && stopped {
                            poll_after_cancel = true;
                        }
                    }
                    Reply::Approvals(result) => {
                        // Nothing here decides anything: a list is a report, and
                        // an empty one is not consent.
                        state.finish_approvals(ticket, result);
                    }
                    Reply::Decide(result) => {
                        // Ask the service what is still waiting rather than
                        // trusting our own copy after a write.
                        if state.finish_decide(ticket, result) {
                            reread_approvals = true;
                        }
                    }
                    Reply::DeleteTask(result) => {
                        // Acceptance is not absence: refresh asks the service
                        // what is still scheduled rather than trusting our own
                        // copy after a write.
                        if state.finish_delete_task(ticket, result) {
                            refresh_after = true;
                        }
                    }
                    Reply::Permissions(result) => {
                        // A list is a report: nothing here revokes anything.
                        state.finish_permissions(ticket, result);
                    }
                    Reply::Revoke(result) => {
                        // Acceptance is not disappearance: ask the service
                        // what is still in force rather than trusting our own
                        // copy after a write.
                        if state.finish_revoke(ticket, result) {
                            reread_grants = true;
                        }
                    }
                    Reply::Send(result) | Reply::SendConnectionError(result) => {
                        let confirmed = matches!(&result, SendOutcome::Confirmed);
                        state.finish_send(ticket, result);
                        if confirmed && state.begin_watch() {
                            // The reply we just asked for is a run to watch, same
                            // as one that was already going when we opened.
                            watch_after = true;
                        }
                        if confirmed {
                            self.prompt.set_text("");
                        }
                        refresh_after = !send_connection_error;
                    }
                    Reply::Login(result) => {
                        self.login_cancellation.borrow_mut().take();
                        let signed_in = result.is_ok();
                        link_after_login = signed_in && state.finish_login(ticket, result);
                    }
                    Reply::AccountChanged | Reply::AuthExpired => {
                        if auth_expired {
                            state.session_expired();
                        } else {
                            state.account_changed();
                        }
                        if let Some(active) = self.account_active.borrow_mut().take() {
                            active.store(false, Ordering::SeqCst);
                        }
                        self.connected_config.borrow_mut().take();
                        self.client.borrow_mut().take();
                        self.consent.set_active(false);
                        self.prompt.set_text("");
                    }
                }
            }
            self.render();
            if watch_after {
                // Bound the watch, then attach: one live listen reports the
                // status and the first fragments, and the timer keeps both the
                // listen and the history poll going until the turn closes.
                self.watch_until
                    .set(Some(std::time::Instant::now() + Duration::from_secs(600)));
                self.poll_live();
            }
            if send_confirmed {
                // A send does not change the conversation chooser, and a full
                // refresh re-verifies the account and lists every conversation
                // before the reply poll can start. Ask for the answer now.
                self.poll_reply();
            } else if refresh_after {
                self.read(false, true);
            }
            if poll_after_cancel {
                // A stop changes what the transcript will say next, so ask for
                // it rather than waiting for the idle refresh.
                self.poll_reply();
            }
            if link_after_login {
                self.connect_account();
            }
            if reread_approvals {
                self.check_approvals();
            }
            if approvals_after {
                self.check_approvals();
            }
            if reread_grants {
                self.check_permissions();
            }
        }

        fn render(&self) {
            self.rendering.set(true);
            let state = self.state.borrow();
            let connected = state.account.is_some();
            let idle = state.busy.is_none();
            let background = match state.busy.as_ref() {
                Some(ticket) => matches!(ticket.operation, Operation::History | Operation::Refresh),
                None => false,
            };
            let (conversation_visible, setup_visible) = account_surface_visibility(connected);
            self.pages.set_visible(conversation_visible);
            self.welcome.set_visible(setup_visible);
            // A linked account needs the conversation chooser, the transcript and
            // the composer. Everything about linking, the agent identity line and
            // the history/tasks tab strip only gets in the way once that is done.
            self.settings.set_visible(!connected);
            self.identity.set_visible(!connected);
            self.scope_label.set_visible(!connected);
            self.pages.set_show_tabs(!connected);
            if connected {
                self.pages.set_current_page(Some(0));
            }
            // Progress text still matters while an operation runs.
            self.connection_notice
                .set_visible(!connected || (!idle && !background));
            let settings_enabled = !connected && idle;
            for widget in [
                self.python.upcast_ref::<gtk::Widget>(),
                self.config_dir.upcast_ref(),
                self.consent.upcast_ref(),
                self.existing.upcast_ref(),
                self.connect.upcast_ref(),
                self.login.upcast_ref(),
            ] {
                widget.set_sensitive(settings_enabled);
            }
            self.disconnect.set_sensitive(connected || !idle);
            self.refresh.set_sensitive(connected && idle);
            self.older
                .set_sensitive(connected && idle && state.limit < 200);
            self.threads.set_sensitive(connected && idle);
            // A stop press only needs a connection; whether anything was
            // actually stopped is the service's answer, not this button's.
            self.cancel.set_sensitive(connected && state.busy.is_none());
            // Polling is background work: the composer stays usable, and a
            // double dispatch is still prevented by `pending_send` in begin_send.
            self.prompt
                .set_sensitive(connected && state.pending_send.is_none());
            self.send
                .set_sensitive(connected && state.pending_send.is_none());
            self.acknowledge
                .set_visible(state.pending_send.is_some() && idle);
            self.acknowledge.set_sensitive(connected && idle);
            self.spinner.set_spinning(!idle && !background);
            let busy = state
                .busy
                .as_ref()
                .map(|t| match t.operation {
                    Operation::Connect => " · connecting…",
                    Operation::Refresh => " · refreshing account, history and tasks…",
                    Operation::History => " · loading scoped history…",
                    Operation::Send => " · dispatching once…",
                    Operation::Login => " · browser sign-in in progress…",
                    Operation::Cancel => " \u{b7} stopping the run\u{2026}",
                    Operation::Approvals => " \u{b7} reading pending approvals\u{2026}",
                    Operation::Decide => " \u{b7} sending your decision\u{2026}",
                    Operation::Permissions => " \u{b7} reading grants in force\u{2026}",
                    Operation::Revoke => " \u{b7} sending your revocation\u{2026}",
                    Operation::DeleteTask => " \u{b7} deleting the scheduled task\u{2026}",
                })
                .unwrap_or("");
            let persistence_warning_guard = self.persistence_warning.borrow();
            let persistence_warning = persistence_warning_guard.as_deref();
            self.connection_notice.set_text(&display_connection_notice(
                &state.connection_notice,
                busy,
                persistence_warning,
            ));
            self.identity.set_text(
                &state
                    .account
                    .as_ref()
                    .map(|a| format!("{} · existing Muse agent {}", a.identity_name, a.vm_id))
                    .unwrap_or_else(|| "Not linked".into()),
            );
            self.chip.set_text(if connected {
                "Connected"
            } else {
                "Not connected"
            });
            let threads_key = format!(
                "{:?}|{:?}|{}",
                state.scope,
                state
                    .account
                    .as_ref()
                    .map(|a| (&a.identity_name, a.threads.len())),
                state.limit
            );
            if self.threads_key.borrow().as_str() != threads_key {
                *self.threads_key.borrow_mut() = threads_key;
                self.threads.remove_all();
                self.threads.append(Some("main"), "Main conversation");
                if let Some(account) = &state.account {
                    for thread in &account.threads {
                        let archived = if thread.archived { " [archived]" } else { "" };
                        let updated = thread
                            .updated_at
                            .as_deref()
                            .map(|s| format!(" · {s}"))
                            .unwrap_or_default();
                        self.threads.append(
                            Some(&format!("thread:{}", thread.session_id)),
                            &format!("{}{archived}{updated}", thread.title),
                        );
                    }
                }
                let selected = state
                    .scope
                    .as_ref()
                    .map(|id| format!("thread:{id}"))
                    .unwrap_or_else(|| "main".into());
                // A disappearing service thread keeps its exact scope; never silently fall back to main.
                if !self.threads.set_active_id(Some(&selected)) {
                    self.threads.append(
                        Some(&selected),
                        "Selected conversation (not in current service thread list)",
                    );
                    self.threads.set_active_id(Some(&selected));
                }
            }
            self.scope_label.set_text(&format!(
                "Conversation scope: {} · latest {} service events",
                state.scope.as_deref().unwrap_or("Main conversation"),
                state.limit
            ));
            // Standing explanations belong in tooltips. The strip above the
            // composer only speaks when something needs attention.
            self.scroll.set_tooltip_text(Some(&state.history_detail));
            self.history_notice.set_text(&state.history_notice);
            self.history_notice
                .set_visible(!state.history_notice.is_empty());
            self.live_notice.set_text(&state.live_status);
            self.live_notice.set_visible(!state.live_status.is_empty());
            // Streamed fragments, shown as they arrive and dropped the moment the
            // transcript carries the finished reply.
            self.draft.set_text(&state.live_draft);
            self.draft.set_visible(!state.live_draft.is_empty());
            self.approvals_box.set_visible(!state.approvals.is_empty());
            self.approval_notice.set_text(&state.approval_notice);
            self.approval_notice
                .set_visible(!state.approval_notice.is_empty());
            self.approvals_refresh.set_sensitive(connected && idle);
            super::super::clear_chat(&self.approvals_box);
            for approval in &state.approvals {
                let card = gtk::Box::new(gtk::Orientation::Vertical, 4);
                card.add_css_class("welcome-card");
                let heading = label(if approval.title.trim().is_empty() {
                    "The agent is asking for approval"
                } else {
                    approval.title.trim()
                });
                heading.add_css_class("assistant-name");
                card.append(&heading);
                if !approval.summary.trim().is_empty() {
                    card.append(&label(approval.summary.trim()));
                }
                let mut evidence = String::new();
                for (name, value) in [
                    ("Kind", approval.kind.as_deref()),
                    ("Offered", approval.decision.as_deref()),
                    ("Remembered as", approval.always_scope.as_deref()),
                ] {
                    if let Some(value) = value.filter(|text| !text.trim().is_empty()) {
                        evidence.push_str(&format!("{name}: {value}\n"));
                    }
                }
                // The request's own detail rows, verbatim: they are what the
                // decision is about, and summarising them would hide the action.
                for detail in &approval.details {
                    evidence.push_str(&format!("{}: {}\n", detail.label, detail.value));
                }
                if !evidence.is_empty() {
                    let facts = label(evidence.trim_end());
                    facts.add_css_class("dim-label");
                    card.append(&facts);
                }
                let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                buttons.add_css_class("toolbar");
                let deciding = state.deciding.is_some();
                let add = |text: &str, decision: muse_account::Decision, suggested: bool| {
                    let button = gtk::Button::with_label(text);
                    if suggested {
                        button.add_css_class("suggested-action");
                    }
                    // One press, one decision: nothing is ever re-sent by a timer,
                    // and every card stops accepting presses while one is in flight.
                    button.set_sensitive(!deciding);
                    let weak = self.self_weak.borrow().clone();
                    let id = approval.approval_id.clone();
                    button.connect_clicked(move |_| {
                        if let Some(ui) = weak.upgrade() {
                            ui.decide(id.clone(), decision);
                        }
                    });
                    buttons.append(&button);
                };
                add("Allow once", muse_account::Decision::AllowOnce, true);
                if approval.always_scope.is_some() {
                    // Only offered when the service published a scope to remember,
                    // so "always" cannot mean a scope this app invented.
                    add("Allow always", muse_account::Decision::AllowAlways, false);
                }
                add("Deny", muse_account::Decision::Deny, false);
                card.append(&buttons);
                self.approvals_box.append(&card);
            }
            self.grants_box.set_visible(!state.grants.is_empty());
            self.grant_notice.set_text(&state.grant_notice);
            self.grant_notice
                .set_visible(!state.grant_notice.is_empty());
            self.grants_refresh.set_sensitive(connected && idle);
            super::super::clear_chat(&self.grants_box);
            for grant in &state.grants {
                let card = gtk::Box::new(gtk::Orientation::Vertical, 4);
                card.add_css_class("welcome-card");
                let heading = label(if !grant.title.trim().is_empty() {
                    grant.title.trim()
                } else if let Some(subject) = grant
                    .policy_subject
                    .as_deref()
                    .filter(|text| !text.trim().is_empty())
                {
                    subject
                } else {
                    "A grant the service reports"
                });
                heading.add_css_class("assistant-name");
                card.append(&heading);
                // The service's own words for what this grant is and covers,
                // verbatim: they are what a revocation would end.
                let mut evidence = String::new();
                for (name, value) in [
                    ("Kind", grant.kind.as_deref()),
                    ("Covers", grant.policy_subject.as_deref()),
                    ("Lifetime", grant.lifetime.as_deref()),
                    ("Mode", grant.mode.as_deref()),
                ] {
                    if let Some(value) = value.filter(|text| !text.trim().is_empty()) {
                        evidence.push_str(&format!("{name}: {value}\n"));
                    }
                }
                if let Some(expires) = grant.expires_at_ms {
                    evidence.push_str(&format!("Expires: {}\n", format_expiry_utc(expires)));
                }
                if !evidence.is_empty() {
                    let facts = label(evidence.trim_end());
                    facts.add_css_class("dim-label");
                    card.append(&facts);
                }
                if grant.revocable {
                    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                    buttons.add_css_class("toolbar");
                    let button = gtk::Button::with_label("Revoke");
                    // One press, one revocation: nothing is ever re-sent by a
                    // timer, and every row stops accepting presses while one
                    // revocation is in flight.
                    button.set_sensitive(state.revoking.is_none());
                    let weak = self.self_weak.borrow().clone();
                    let id = grant.permission_id.clone();
                    button.connect_clicked(move |_| {
                        if let Some(ui) = weak.upgrade() {
                            ui.revoke(id.clone());
                        }
                    });
                    buttons.append(&button);
                    card.append(&buttons);
                } else {
                    let note = label("The service does not mark this grant revocable.");
                    note.add_css_class("dim-label");
                    card.append(&note);
                }
                self.grants_box.append(&card);
            }
            self.tasks_notice.set_text(&state.tasks_notice);
            self.send_notice.set_text(&state.send_notice);
            self.send_notice.set_visible(!state.send_notice.is_empty());
            let was_at_bottom = {
                let adjustment = self.scroll.vadjustment();
                adjustment.upper() - adjustment.page_size() - adjustment.value() < 48.0
            };
            super::super::clear_chat(&self.history);
            if !state.history.is_empty() {
                super::super::append_day_divider(&self.history);
            }
            for message in &state.history {
                let role = match message.role.as_str() {
                    "user" => super::super::ChatRole::User,
                    "assistant" => super::super::ChatRole::Assistant,
                    _ => super::super::ChatRole::Notice,
                };
                // No per-message telemetry line: role is visible from the
                // bubble's side, and seq/status are service bookkeeping, not
                // something a reader should have to look at 200 times. The
                // bubble shows the message's own time when it has one.
                super::super::append_chat_stamped(
                    &self.history,
                    role,
                    &super::presentation_text(&message.text),
                    message.timestamp.as_deref().and_then(short_stamp),
                );
            }
            // Newest messages are at the bottom. Follow them when the reader
            // was already there, and repeat after allocation: a fresh row only
            // enlarges the adjustment once it has been laid out.
            if was_at_bottom {
                for delay in [0u64, 60] {
                    let scroll = self.scroll.clone();
                    gtk::glib::timeout_add_local_once(Duration::from_millis(delay), move || {
                        let adjustment = scroll.vadjustment();
                        adjustment
                            .set_value((adjustment.upper() - adjustment.page_size()).max(0.0));
                    });
                }
            }
            super::super::clear_chat(&self.tasks);
            if let Some(tasks) = &state.tasks {
                self.tasks.append(&label(&format!(
                    "{} schedules · {} recent runs returned",
                    tasks.schedules.len(),
                    tasks.runs.len()
                )));
                for schedule in &tasks.schedules {
                    let item = label(&format!(
                        "{}\nID: {} · {} · timezone: {} · next run: {}",
                        schedule.title,
                        schedule.id,
                        if schedule.enabled {
                            "Enabled"
                        } else {
                            "Disabled"
                        },
                        schedule.timezone.as_deref().unwrap_or("not supplied"),
                        utc_time(schedule.next_run_at_utc)
                    ));
                    item.add_css_class("card");
                    let row = gtk::Box::new(gtk::Orientation::Vertical, 4);
                    row.append(&item);
                    // Deleting a schedule is permanent, so the button arms on
                    // the first press and only the second sends the deletion.
                    let armed = state.confirming_delete.as_deref() == Some(schedule.id.as_str());
                    let delete = gtk::Button::with_label(if armed {
                        "Press again to delete"
                    } else {
                        "Delete"
                    });
                    if armed {
                        delete.add_css_class("destructive-action");
                    }
                    delete.set_sensitive(connected && idle && state.deleting.is_none());
                    let weak = self.self_weak.borrow().clone();
                    let id = schedule.id.clone();
                    delete.connect_clicked(move |_| {
                        if let Some(ui) = weak.upgrade() {
                            ui.delete_task(id.clone());
                        }
                    });
                    row.append(&delete);
                    self.tasks.append(&row);
                }
                self.tasks
                    .append(&label("Recent task runs (no retry controls)"));
                for run in &tasks.runs {
                    let item = label(&format!(
                        "{} · job {} · {}\nScheduled: {}\n{}{}",
                        run.run_id,
                        run.job_id,
                        run.status,
                        utc_time(run.scheduled_for_utc),
                        run.result_summary
                            .as_deref()
                            .unwrap_or("No result summary supplied."),
                        run.error_text
                            .as_deref()
                            .map(|e| format!("\nRun error: {e}"))
                            .unwrap_or_default()
                    ));
                    item.add_css_class("card");
                    self.tasks.append(&item);
                }
            }
            // Nothing needs attention, so the setup panel gets out of the way.
            if connected && self.settings.is_expanded() && state.history_notice.is_empty() {
                self.settings.set_expanded(false);
            }
            self.rendering.set(false);
        }
    }

    /// The HH:MM out of a service timestamp for a bubble stamp. Anything that
    /// is not recognisably a timestamp yields None — an absent stamp is honest,
    /// a mangled one is noise.
    fn short_stamp(timestamp: &str) -> Option<String> {
        let bytes = timestamp.as_bytes();
        if bytes.len() >= 16 && bytes.get(10) == Some(&b'T') {
            return Some(timestamp[11..16].to_string());
        }
        if !timestamp.is_empty() && timestamp.len() <= 8 && !timestamp.contains(' ') {
            return Some(timestamp.to_string());
        }
        None
    }

    fn utc_time(timestamp: Option<i64>) -> String {
        timestamp
            .map(|value| {
                gtk::glib::DateTime::from_unix_utc(value)
                    .and_then(|date| date.format("%Y-%m-%d %H:%M:%S UTC"))
                    .map(|date| date.to_string())
                    .unwrap_or_else(|_| format!("Unix {value} (UTC)"))
            })
            .unwrap_or_else(|| "not supplied".into())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn short_stamp_extracts_hh_mm_and_refuses_noise() {
            assert_eq!(
                short_stamp("2026-11-09T15:49:12Z").as_deref(),
                Some("15:49")
            );
            assert_eq!(short_stamp("15:49").as_deref(), Some("15:49"));
            assert_eq!(short_stamp(""), None);
            assert_eq!(short_stamp("yesterday afternoon"), None);
            assert_eq!(short_stamp("2026-11-09 15:49:12"), None);
        }

        fn descendants(root: &gtk::Widget) -> Vec<gtk::Widget> {
            let mut result = vec![root.clone()];
            let mut child = root.first_child();
            while let Some(widget) = child {
                result.extend(descendants(&widget));
                child = widget.next_sibling();
            }
            result
        }

        fn button(root: &gtk::Box, text: &str) -> gtk::Button {
            descendants(root.upcast_ref())
                .into_iter()
                .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
                .find(|button| button.label().as_deref() == Some(text))
                .unwrap()
        }

        fn has_label(root: &gtk::Box, text: &str) -> bool {
            descendants(root.upcast_ref())
                .into_iter()
                .filter_map(|widget| widget.downcast::<gtk::Label>().ok())
                .any(|label| label.text().contains(text))
        }

        fn pump_until(mut predicate: impl FnMut() -> bool) {
            let deadline = Instant::now() + Duration::from_secs(20);
            let context = gtk::glib::MainContext::default();
            while !predicate() {
                assert!(
                    Instant::now() < deadline,
                    "GTK condition did not complete within 20 seconds"
                );
                while context.pending() {
                    context.iteration(false);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        #[test]
        #[ignore = "requires GTK display; run under xvfb-run --test-threads=1"]
        fn native_account_page_builds_and_presents() {
            // Regression gate for the Nov 9 centered-column patch, which
            // appended each column box to ITSELF (chat column and schedules
            // column). A self-parented widget makes GTK's ancestor walk
            // loop forever on the next append, so the app hung during
            // startup and never presented a window. Building the page and
            // presenting it is the whole assertion: on the broken code
            // this test never returns from build().
            gtk::init().unwrap();
            crate::install_dark_theme();
            let chip = gtk::Label::new(Some("Not linked"));
            let page = build(&chip);
            let window = gtk::Window::builder()
                .title("Account page smoke")
                .default_width(1100)
                .default_height(700)
                .build();
            window.set_child(Some(&page.root));
            window.present();
            let ctx = gtk::glib::MainContext::default();
            let start = std::time::Instant::now();
            while start.elapsed() < Duration::from_millis(750) {
                while ctx.iteration(false) {}
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(window.is_visible());
        }

        #[test]
        #[ignore = "requires GTK display; run under xvfb-run --test-threads=1"]
        fn native_account_context_device_dispatch_and_disconnect_coupling() {
            use crate::local_host::DeviceLease;
            use std::sync::atomic::{AtomicBool, Ordering};
            gtk::init().unwrap();
            let scratch = std::env::var_os("TMPDIR")
                .map(PathBuf::from)
                .unwrap()
                .join(format!(
                    "account-host-ui-{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                ));
            std::fs::create_dir(&scratch).unwrap();
            let script = scratch.join("bridge.py");
            std::fs::write(
                &script,
                r#"
import json,sys,pathlib
r=json.load(sys.stdin)
if r['op']=='send':
 pathlib.Path(__file__).with_name('sent').write_text(json.dumps(r))
 data={'sent':True,'confirmed':False,'message_id':None}
elif r['op']=='connect': data={'identity_name':'fake','vm_id':'fake-vm','threads':[]}
elif r['op']=='tasks': data={'schedules':[],'runs':[]}
else: data=[]
print(json.dumps({'ok':True,'data':data}))
"#,
            )
            .unwrap();
            let config = BridgeConfig {
                python: "/usr/bin/python3".into(),
                script,
                config_dir: scratch.clone(),
            };
            let page = build(&gtk::Label::new(None));
            assert!((page.host_context)().is_none());
            page.ui.host_verified.set(true);
            *page.ui.connected_config.borrow_mut() = Some(config.clone());
            *page.ui.client.borrow_mut() = Some(AccountClient::new(config.clone()));
            page.ui.state.borrow_mut().account = Some(AccountSnapshot {
                identity_name: "fake".into(),
                vm_id: "fake-vm".into(),
                threads: vec![],
            });
            assert_eq!((page.host_context)().unwrap().script, config.script);
            let boundary = Rc::new(Cell::new(0));
            let count = boundary.clone();
            *page.host_boundary.borrow_mut() = Some(Rc::new(move || count.set(count.get() + 1)));
            let active = Arc::new(AtomicBool::new(true));
            *page.host_device.borrow_mut() = Some(DeviceLease {
                node_id: "linux-registered-test".into(),
                active: active.clone(),
            });
            page.ui.prompt.set_text("once");
            page.ui.send_message();
            pump_until(|| page.ui.state.borrow().busy.is_none());
            let sent: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(scratch.join("sent")).unwrap())
                    .unwrap();
            assert_eq!(sent["device_id"], "linux-registered-test");
            page.ui.state.borrow_mut().acknowledge_send();
            page.host_device.borrow_mut().take();
            active.store(false, Ordering::SeqCst);
            page.ui.prompt.set_text("after stop");
            page.ui.send_message();
            pump_until(|| page.ui.state.borrow().busy.is_none());
            let sent: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(scratch.join("sent")).unwrap())
                    .unwrap();
            assert!(sent.get("device_id").is_none());
            let refresh = page.ui.state.borrow_mut().begin_read(false, true).unwrap();
            page.ui.complete(
                &refresh,
                Reply::Refresh(
                    Ok(AccountSnapshot {
                        identity_name: "fake".into(),
                        vm_id: "fake-vm".into(),
                        threads: vec![],
                    }),
                    Err("connection failed".into()),
                    Ok(TasksSnapshot {
                        schedules: vec![],
                        runs: vec![],
                    }),
                ),
            );
            assert_eq!(
                boundary.get(),
                1,
                "a failed account IO boundary must stop the host"
            );
            assert!((page.host_context)().is_none());
            let stale = refresh.clone();
            page.ui.complete(
                &stale,
                Reply::Refresh(
                    Ok(AccountSnapshot {
                        identity_name: "fake".into(),
                        vm_id: "fake-vm".into(),
                        threads: vec![],
                    }),
                    Ok(vec![]),
                    Ok(TasksSnapshot {
                        schedules: vec![],
                        runs: vec![],
                    }),
                ),
            );
            assert!(
                (page.host_context)().is_none(),
                "stale successful refresh cannot re-enable host context"
            );
            page.ui.state.borrow_mut().acknowledge_send();
            let failed_send = page
                .ui
                .state
                .borrow_mut()
                .begin_send("connection error")
                .unwrap();
            page.ui.complete(
                &failed_send,
                Reply::SendConnectionError(SendOutcome::Uncertain(
                    "connection closed after dispatch".into(),
                )),
            );
            assert_eq!(boundary.get(), 2);
            page.ui.disconnect.emit_clicked();
            assert_eq!(boundary.get(), 3);
            assert!((page.host_context)().is_none());
            assert!(page.host_device.borrow().is_none());
            std::fs::remove_dir_all(scratch).unwrap();
        }

        #[test]
        fn saved_connection_profile_round_trips_only_non_secret_paths() {
            let encoded = saved_connection_text("/usr/bin/python3", "/home/test/.config/muse");
            assert!(!encoded.contains("password"));
            assert_eq!(
                saved_connection_from_text(&encoded),
                Some(("/usr/bin/python3".into(), "/home/test/.config/muse".into()))
            );
        }

        #[test]
        fn saved_connection_profile_rejects_relative_or_malformed_values() {
            assert!(saved_connection_from_text("not-json").is_none());
            assert!(
                saved_connection_from_text(r#"{"python":"python3","config_dir":"/tmp/muse"}"#)
                    .is_none()
            );
            assert!(
                saved_connection_from_text(
                    r#"{"python":"/usr/bin/python3","config_dir":"/tmp/muse","token":"secret"}"#
                )
                .is_none()
            );
            assert!(
                saved_connection_from_text(
                    r#"{"python":"/usr/bin/python3","python":"/usr/bin/python3","config_dir":"/tmp/muse"}"#
                )
                .is_none()
            );
        }

        #[cfg(unix)]
        #[test]
        fn persistent_directories_are_private_and_nofollow() {
            use std::os::unix::fs::{PermissionsExt, symlink};

            let scratch = std::env::var_os("TMPDIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join(format!(
                    "muse-persistence-test-{}-{}",
                    std::process::id(),
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                ));
            fs::create_dir(&scratch).unwrap();
            let root_path = scratch.join("missing").join("config");
            let root = open_directory_path(&root_path, true).unwrap();
            close_fd(root);
            let mode = fs::metadata(&root_path).unwrap().permissions().mode();
            assert_eq!(mode & 0o022, 0);

            let app_path = root_path.join("unofficial-muse-for-linux");
            let root = open_directory_path(&root_path, false).unwrap();
            let app = open_child_directory(root, "unofficial-muse-for-linux", true).unwrap();
            close_fd(app);
            close_fd(root);
            fs::set_permissions(&app_path, fs::Permissions::from_mode(0o750)).unwrap();
            let root = open_directory_path(&root_path, false).unwrap();
            assert!(open_child_directory(root, "unofficial-muse-for-linux", false).is_err());
            close_fd(root);

            let link = scratch.join("config-link");
            symlink(&root_path, &link).unwrap();
            assert!(open_directory_path(&link, false).is_err());
            fs::remove_dir_all(scratch).unwrap();
        }

        #[test]
        fn persistence_failure_notice_survives_busy_state_rendering() {
            let rendered = display_connection_notice(
                "Account linked.",
                "",
                Some(" · Connection paths were not saved locally: unavailable."),
            );
            assert!(rendered.contains("Connection paths were not saved locally"));
        }

        #[test]
        fn schedule_times_are_readable_utc_without_inventing_missing_values() {
            assert_eq!(utc_time(Some(0)), "1970-01-01 00:00:00 UTC");
            assert_eq!(utc_time(None), "not supplied");
            assert!(utc_time(Some(i64::MAX)).contains("UTC"));
        }

        #[test]
        #[ignore = "requires GTK display; run this filter under xvfb-run, --test-threads=1"]
        fn native_account_page_consent_failure_and_disconnect_signal_smoke() {
            gtk::init().unwrap();
            let chip = gtk::Label::new(None);
            let page = build(&chip);
            assert!(page.ui.welcome.is_visible());
            assert!(!page.ui.pages.is_visible());
            assert!(!page.ui.settings.is_expanded());
            // The primary path lives on the card itself, so linking needs no setup step.
            assert!(has_label(
                &page.root,
                "Sign in to the Muse account you already use"
            ));
            assert!(page.ui.login.is_sensitive());
            // Regression: Disconnect once lived in the welcome card, which is
            // hidden while connected, leaving a linked account with no way out.
            let mut ancestor = page.ui.disconnect.parent();
            while let Some(widget) = ancestor {
                assert_ne!(widget, page.ui.welcome.clone().upcast::<gtk::Widget>());
                ancestor = widget.parent();
            }
            // Advanced paths stay collapsed until asked for; GTK builds the
            // expander's child on the first expansion, so open it before counting.
            page.ui.settings.set_expanded(true);
            let window = gtk::Window::builder()
                .default_width(1240)
                .default_height(900)
                .build();
            window.set_child(Some(&page.root));
            window.present();
            let controls = descendants(page.root.upcast_ref());
            let entries: Vec<_> = controls
                .iter()
                .cloned()
                .filter_map(|widget| widget.downcast::<gtk::Entry>().ok())
                .collect();
            let consent = controls
                .into_iter()
                .find_map(|widget| widget.downcast::<gtk::CheckButton>().ok())
                .unwrap();
            assert_eq!(entries.len(), 3);
            assert!(!entries[2].is_sensitive());
            button(&page.root, "Link account").emit_clicked();
            assert!(has_label(&page.root, "Explicit consent is required"));
            button(&page.root, "Use my existing Muse login").emit_clicked();
            assert_eq!(
                entries[1].text().as_str(),
                config_home().unwrap().join("muse-cli").to_str().unwrap()
            );
            assert!(!consent.is_active());
            // The reported bug: the CLI folder stayed selected, then browser sign-in
            // failed with an opaque message. It must switch back without a browser.
            button(&page.root, "Sign in with Muse").emit_clicked();
            assert_eq!(
                entries[1].text().as_str(),
                app_account_dir().unwrap().to_str().unwrap()
            );
            assert!(has_label(
                &page.root,
                "Switched to this app's own session folder"
            ));
            assert!(!consent.is_active());
            entries[0].set_text("/usr/bin/python3");
            let scratch = std::env::var_os("TMPDIR")
                .map(PathBuf::from)
                .expect("Set TMPDIR to the test scratch directory");
            let missing_config =
                scratch.join(format!("muse-ui-missing-config-{}", std::process::id()));
            assert!(!missing_config.exists());
            entries[1].set_text(missing_config.to_str().unwrap());
            consent.set_active(true);
            button(&page.root, "Link account").emit_clicked();
            assert!(!entries[0].is_sensitive());
            pump_until(|| has_label(&page.root, "Connection failed:"));
            assert!(chip.text().contains("Not connected"));
            assert!(entries[0].is_sensitive());
            assert!(!entries[2].is_sensitive());
            button(&page.root, "Link account").emit_clicked();
            button(&page.root, "Disconnect").emit_clicked();
            assert!(has_label(&page.root, "Disconnected locally"));
            assert!(!consent.is_active());
            // Run a local main-context timer to prove the GUI remains responsive after invalidation.
            let ticked = Rc::new(Cell::new(false));
            let probe = Rc::clone(&ticked);
            gtk::glib::timeout_add_local_once(Duration::from_millis(200), move || probe.set(true));
            pump_until(|| ticked.get());
            assert!(has_label(&page.root, "Disconnected locally"));
            assert!(!has_label(&page.root, "No shared history requested"));
            window.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    type State = Controller<String, String, String>;

    fn row(role: &str, status: Option<&str>) -> muse_account::HistoryMessage {
        muse_account::HistoryMessage {
            message_id: "m".into(),
            seq: 1,
            role: role.into(),
            text: String::new(),
            timestamp: None,
            status: status.map(str::to_owned),
        }
    }

    fn delta(seq: u64, text: &str) -> muse_account::LiveEvent {
        muse_account::LiveEvent {
            event: DELTA_EVENT.into(),
            seq: Some(seq),
            text: Some(text.into()),
            status: None,
            session_id: None,
        }
    }

    #[test]
    fn streamed_fragments_are_joined_and_never_applied_twice() {
        let mut state: State = Controller::default();
        state.apply_live(Ok(vec![
            delta(10, "anti"),
            delta(11, "-Sullan"),
            delta(12, " government"),
        ]));
        assert_eq!(state.live_draft, "anti-Sullan government");
        // A repeated listen returns overlapping records, so `seq` has to de-dupe
        // them; a fragment without a seq cannot be de-duped and is ignored.
        state.apply_live(Ok(vec![delta(11, "-Sullan"), delta(13, " and alliance")]));
        assert_eq!(state.live_draft, "anti-Sullan government and alliance");
        state.note_turn_finished();
        assert!(state.live_draft.is_empty());
        assert_eq!(state.live_delta_seq, 0);
    }

    #[test]
    fn a_draft_is_never_mistaken_for_a_status() {
        let mut state: State = Controller::default();
        state.apply_live(Ok(vec![delta(1, "2:55 PM in London")]));
        assert!(state.live_status.is_empty());
        assert_eq!(state.live_draft, "2:55 PM in London");
    }

    #[test]
    fn an_in_flight_reply_is_not_finished() {
        // Observed live: while a turn produces, history carries the assistant row
        // with no status at all (seq 72546), and only later a terminal one.
        for status in ["", "running", "streaming", "online"] {
            assert!(
                !reply_finished(status),
                "{status:?} must not count as finished"
            );
        }
        for status in ["completed", "Interrupted", "cancelled", "failed"] {
            assert!(reply_finished(status), "{status:?} must count as finished");
        }
    }

    #[test]
    fn only_a_finished_reply_closes_the_turn() {
        // `interrupted` is what a cancelled run ends as, observed live: the
        // reply stopped mid-list and never completed.
        assert!(history_closes_turn(&[row(
            "assistant",
            Some("interrupted")
        )]));
        assert!(history_closes_turn(&[row("assistant", Some("failed"))]));
        assert!(history_closes_turn(&[row("assistant", Some("completed"))]));
        assert!(!history_closes_turn(&[row("assistant", None)]));
        assert!(!history_closes_turn(&[row("assistant", Some("running"))]));
        assert!(!history_closes_turn(&[row("user", None)]));
        assert!(!history_closes_turn(&[]));
        // The newest row decides: once a new user message follows the finished
        // reply, the next turn is under way and the line may speak again.
        assert!(!history_closes_turn(&[
            row("assistant", Some("completed")),
            row("user", None)
        ]));
    }

    #[test]
    fn a_reply_still_being_produced_is_worth_watching() {
        // The in-flight row of a real run carried no status at all.
        assert!(reply_in_flight(&[row("assistant", None)]));
        assert!(reply_in_flight(&[
            row("user", None),
            row("assistant", None)
        ]));
        for status in ["completed", "interrupted", "cancelled", "failed"] {
            assert!(
                !reply_in_flight(&[row("assistant", Some(status))]),
                "{status:?} is a finished reply"
            );
        }
        // A prompt with no reply yet, or nothing at all, is not a run to attach to.
        assert!(!reply_in_flight(&[row("user", None)]));
        assert!(!reply_in_flight(&[]));
    }

    #[test]
    fn a_window_that_lost_the_turn_attaches_to_it_once() {
        let mut state: State = Controller::default();
        assert!(!state.watching_run);
        assert!(state.begin_watch());
        assert!(state.watching_run);
        // Later reads must not restart the watch, or its bound is never reached.
        assert!(!state.begin_watch());
        // A finished turn ends it.
        state.note_turn_finished();
        assert!(!state.watching_run);
    }

    #[test]
    fn a_reported_working_status_opens_the_watch_and_a_settled_one_does_not() {
        let mut working: State = Controller::default();
        working.apply_live(Ok(vec![event("agent.status", Some("is responding"), None)]));
        assert!(working.watching_run);
        let mut settled: State = Controller::default();
        settled.apply_live(Ok(vec![event("task.status", Some("completed"), None)]));
        assert!(!settled.watching_run);
    }

    fn event(name: &str, status: Option<&str>, text: Option<&str>) -> muse_account::LiveEvent {
        muse_account::LiveEvent {
            event: name.into(),
            seq: Some(1),
            text: text.map(str::to_owned),
            status: status.map(str::to_owned),
            session_id: None,
        }
    }

    #[test]
    fn a_status_asking_for_approval_reads_the_list_once() {
        let mut state: State = Controller::default();
        state.apply_live(Ok(vec![event(
            "agent.status",
            Some("Needs approval"),
            None,
        )]));
        assert!(state.claim_approval_read());
        // The same status arriving again is not a new request.
        assert!(!state.claim_approval_read());
        state.apply_live(Ok(vec![event("agent.status", Some("Is working"), None)]));
        assert!(!state.claim_approval_read());
        // Asking again after working is a new request, not a repeat.
        state.apply_live(Ok(vec![event(
            "agent.status",
            Some("Needs approval"),
            None,
        )]));
        assert!(state.claim_approval_read());
    }

    #[test]
    fn ordinary_progress_never_reads_approvals() {
        let mut state: State = Controller::default();
        state.apply_live(Ok(vec![event("agent.status", Some("Is working"), None)]));
        assert!(!state.claim_approval_read());
        state.note_turn_finished();
        assert!(!state.claim_approval_read());
    }

    fn approval(id: &str, scope: Option<&str>) -> muse_account::Approval {
        muse_account::Approval {
            approval_id: id.into(),
            kind: Some("egress".into()),
            decision: Some("allow_once".into()),
            always_scope: scope.map(str::to_owned),
            attention_mode: None,
            title: "Connect to example.test".into(),
            summary: "The agent wants to reach the network.".into(),
            details: vec![muse_account::ApprovalDetail {
                label: "Host".into(),
                value: "example.test:443".into(),
            }],
        }
    }

    #[test]
    fn a_decision_leaves_the_list_only_after_the_service_accepts_it() {
        let mut state: State = Controller {
            approvals: vec![approval("a-1", None), approval("a-2", Some("task"))],
            ..Controller::default()
        };
        let ticket = state.issue(Operation::Approvals).expect("ticket");
        assert!(state.finish_approvals(
            &ticket,
            Ok(muse_account::PendingApprovals {
                pending: vec![approval("a-1", None)],
            })
        ));
        assert_eq!(state.approvals.len(), 1);
        state.deciding = Some("a-1".into());
        let ticket = state.issue(Operation::Decide).expect("ticket");
        assert!(state.finish_decide(
            &ticket,
            Ok(muse_account::DecisionAnswer {
                approval_id: "a-1".into(),
                requested: "allow_once".into(),
                accepted: true,
                decision: Some("allow_once".into()),
            })
        ));
        assert!(state.approvals.is_empty());
        assert!(state.deciding.is_none());
        // The service accepting a decision is not the action succeeding.
        assert!(state.approval_notice.contains("accepted"));
        assert!(!state.approval_notice.contains("allowed"));
    }

    #[test]
    fn an_unanswered_decision_keeps_the_request_on_screen() {
        let mut state: State = Controller {
            approvals: vec![approval("a-1", None)],
            deciding: Some("a-1".into()),
            ..Controller::default()
        };
        let ticket = state.issue(Operation::Decide).expect("ticket");
        assert!(!state.finish_decide(&ticket, Err("timeout".into())));
        assert_eq!(state.approvals.len(), 1);
        assert!(state.deciding.is_none());
        // The wording has to steer a reader to re-read, not to press twice.
        assert!(state.approval_notice.contains("instead of pressing twice"));
    }

    #[test]
    fn an_empty_read_reports_itself_rather_than_claiming_consent() {
        let mut state: State = Controller {
            approvals: vec![approval("a-1", None)],
            ..Controller::default()
        };
        let ticket = state.issue(Operation::Approvals).expect("ticket");
        state.finish_approvals(&ticket, Ok(muse_account::PendingApprovals::default()));
        assert!(state.approvals.is_empty());
        assert!(state.approval_notice.contains("no approvals are waiting"));
    }

    fn grant(id: &str, revocable: bool) -> muse_account::ActiveGrant {
        muse_account::ActiveGrant {
            permission_id: id.into(),
            kind: Some("egress".into()),
            lifetime: Some("always".into()),
            mode: Some("allow".into()),
            policy_subject: Some("example.test".into()),
            expires_at_ms: None,
            revocable,
            title: "example.test".into(),
        }
    }

    #[test]
    fn an_accepted_revocation_leaves_the_list_but_only_claims_acceptance() {
        let mut state: State = Controller {
            grants: vec![grant("p-1", true), grant("p-2", false)],
            ..Controller::default()
        };
        let ticket = state.issue(Operation::Permissions).expect("ticket");
        assert!(state.finish_permissions(
            &ticket,
            Ok(muse_account::ActiveGrants {
                grants: vec![grant("p-1", true)],
            })
        ));
        assert_eq!(state.grants.len(), 1);
        state.revoking = Some("p-1".into());
        let ticket = state.issue(Operation::Revoke).expect("ticket");
        assert!(state.finish_revoke(
            &ticket,
            Ok(muse_account::RevokeAnswer {
                permission_id: "p-1".into(),
                accepted: true,
                revoked: Some(true),
            })
        ));
        assert!(state.grants.is_empty());
        assert!(state.revoking.is_none());
        // Acceptance is not disappearance: the wording sends the reader to
        // the re-read for proof, and never claims the grant is gone.
        assert!(state.grant_notice.contains("accepted"));
        assert!(state.grant_notice.contains("confirm"));
        assert!(!state.grant_notice.contains("is gone from your account"));
    }

    #[test]
    fn an_unanswered_revocation_keeps_the_grant_on_screen() {
        let mut state: State = Controller {
            grants: vec![grant("p-1", true)],
            revoking: Some("p-1".into()),
            ..Controller::default()
        };
        let ticket = state.issue(Operation::Revoke).expect("ticket");
        assert!(!state.finish_revoke(&ticket, Err("timeout".into())));
        assert_eq!(state.grants.len(), 1);
        assert!(state.revoking.is_none());
        assert!(state.grant_notice.contains("instead of pressing twice"));
    }

    #[test]
    fn an_empty_grants_read_reports_no_grants_in_force() {
        let mut state: State = Controller {
            grants: vec![grant("p-1", true)],
            ..Controller::default()
        };
        let ticket = state.issue(Operation::Permissions).expect("ticket");
        state.finish_permissions(&ticket, Ok(muse_account::ActiveGrants::default()));
        assert!(state.grants.is_empty());
        assert!(state.grant_notice.contains("no grants are in force"));
    }

    #[test]
    fn a_schedule_deletion_needs_an_armed_second_press() {
        let mut state: State = Controller {
            account: Some("account".into()),
            ..Controller::default()
        };
        // Unarmed: no ticket exists, so nothing can reach the service.
        assert!(state.begin_delete_task("job-1").is_none());
        state.arm_delete("job-1");
        assert!(state.tasks_notice.contains("Press Delete again"));
        // Arming a different schedule moves the arm; the first is safe.
        state.arm_delete("job-2");
        assert!(state.begin_delete_task("job-1").is_none());
        let ticket = state.begin_delete_task("job-2").expect("ticket");
        assert_eq!(state.deleting.as_deref(), Some("job-2"));
        assert!(state.confirming_delete.is_none());
        assert!(state.finish_delete_task(
            &ticket,
            Ok(muse_account::DeleteAnswer {
                job_id: "job-2".into(),
                accepted: true,
                deleted: Some(true),
            })
        ));
        assert!(state.deleting.is_none());
        // Acceptance is not absence: the wording sends the reader to the
        // refresh for proof, and never claims the schedule is gone.
        assert!(state.tasks_notice.contains("accepted"));
        assert!(state.tasks_notice.contains("confirm"));
    }

    #[test]
    fn an_unanswered_deletion_reports_and_clears_the_flight() {
        let mut state: State = Controller {
            account: Some("account".into()),
            ..Controller::default()
        };
        state.arm_delete("job-1");
        let ticket = state.begin_delete_task("job-1").expect("ticket");
        assert!(!state.finish_delete_task(&ticket, Err("timeout".into())));
        assert!(state.deleting.is_none());
        assert!(state.tasks_notice.contains("instead of pressing twice"));
    }

    #[test]
    fn expiry_formatting_matches_known_timestamps() {
        assert_eq!(format_expiry_utc(0), "1970-01-01 00:00 UTC");
        // 2026-11-09 14:00 EST is 19:00 UTC.
        assert_eq!(format_expiry_utc(1_794_250_800_000), "2026-11-09 19:00 UTC");
        // A leap day and a year boundary.
        assert_eq!(format_expiry_utc(1_709_164_800_000), "2024-02-29 00:00 UTC");
        assert_eq!(format_expiry_utc(1_767_225_599_000), "2025-12-31 23:59 UTC");
    }

    #[test]
    fn settled_states_are_not_painted_as_progress() {
        // Captured from a real send: the newest statuses were presence and a
        // completed task, and the strip read "Agent · completed".
        let mut state: State = Controller::default();
        state.apply_live(Ok(vec![
            event("model_lock.updated", None, None),
            event("agent.status", Some("online"), None),
            event("task.status", Some("completed"), None),
            event("task.status", Some("interrupted"), None),
            event("approvals.snapshot", None, None),
        ]));
        assert!(state.live_status.is_empty());
    }

    #[test]
    fn a_wait_for_the_user_is_painted() {
        let mut state: State = Controller::default();
        state.apply_live(Ok(vec![event(
            "agent.status",
            Some("Needs approval"),
            None,
        )]));
        assert_eq!(state.live_status, "Agent · Needs approval");
    }

    #[test]
    fn reply_text_is_never_mirrored_into_the_status_line() {
        let mut state: State = Controller::default();
        state.apply_live(Ok(vec![event(
            "delta.text",
            None,
            Some("2:55 PM in London right now (GMT)"),
        )]));
        assert!(state.live_status.is_empty());
    }

    #[test]
    fn finishing_a_turn_clears_a_stale_status_line() {
        let mut state: State = Controller::default();
        state.apply_live(Ok(vec![muse_account::LiveEvent {
            event: "agent.status".into(),
            seq: Some(1),
            text: None,
            status: Some("Is working".into()),
            session_id: None,
        }]));
        assert_eq!(state.live_status, "Agent \u{b7} Is working");
        state.note_turn_finished();
        assert!(state.live_status.is_empty());
        // A failed read is a statement about the tool, not about the agent, and
        // it must not survive as if it were live status either.
        state.apply_live(Err("timeout".into()));
        assert!(state.live_status.starts_with("Live status unavailable"));
        state.note_turn_finished();
        assert!(state.live_status.is_empty());
    }

    #[test]
    fn disconnected_account_shows_setup_and_hides_empty_conversation_surface() {
        assert_eq!(account_surface_visibility(false), (false, true));
        assert_eq!(account_surface_visibility(true), (true, false));
    }

    #[test]
    fn widget_markers_are_not_shown_as_raw_protocol_text() {
        assert_eq!(
            presentation_text("[[hatch_widget.widget-80acc675-df45-4ed0-8abc-66c93ac7a1fe]]"),
            "Muse returned an interactive widget that this Linux app cannot render yet."
        );
        assert_eq!(
            presentation_text("Hello [[hatch_widget.widget-abc]]"),
            "Hello"
        );
    }

    #[test]
    fn helper_error_codes_are_actionable_without_echoing_untrusted_output() {
        assert!(login_error_message("missing_browser").contains("Chromium"));
        assert!(login_error_message("missing_dependency").contains("Python"));
        assert!(login_error_message("unsafe_config").contains("private"));
        assert!(login_error_message("unsafe_devtools").contains("verified"));
        assert!(!login_error_message("cookie=secret-never-show").contains("secret-never-show"));
    }

    #[test]
    fn browser_sign_in_moves_off_the_muse_cli_folder() {
        let app = std::path::Path::new("/home/user/.config/unofficial-muse-for-linux/account");
        let cli = std::path::Path::new("/home/user/.config/muse-cli");
        // A leftover CLI folder must not be reused, or sign-in fails opaquely.
        assert_eq!(
            native::browser_sign_in_dir("/home/user/.config/muse-cli", Some(app), Some(cli)),
            Some(app.display().to_string())
        );
        assert_eq!(
            native::browser_sign_in_dir("  ", Some(app), Some(cli)),
            Some(app.display().to_string())
        );
        // An explicit override is honoured, and no home means no choice.
        assert_eq!(
            native::browser_sign_in_dir("/srv/muse/account", Some(app), Some(cli)),
            Some("/srv/muse/account".into())
        );
        assert_eq!(native::browser_sign_in_dir("", None, None), None);
    }

    #[test]
    fn saved_system_interpreter_yields_to_an_installed_bridge_environment() {
        let bridge = std::path::Path::new(
            "/home/user/.local/share/unofficial-muse-for-linux/account-bridge/.venv/bin/python",
        );
        // A fallback saved before the venv existed must not shadow it forever.
        assert_eq!(
            native::preferred_saved_python("/usr/bin/python3", bridge),
            bridge.display().to_string()
        );
        assert_eq!(
            native::preferred_saved_python("  ", bridge),
            bridge.display().to_string()
        );
        // A deliberate override is kept even when the default environment exists.
        assert_eq!(
            native::preferred_saved_python("/opt/py/bin/python", bridge),
            "/opt/py/bin/python"
        );
        // With no bridge installed nothing is rewritten.
        assert_eq!(
            native::preferred_saved_python(
                "/usr/bin/python3",
                std::path::Path::new("/usr/bin/python3")
            ),
            "/usr/bin/python3"
        );
    }

    #[test]
    fn missing_bridge_names_the_setup_command() {
        let unavailable = muse_account::AccountError {
            code: muse_account::ErrorCode::BridgeUnavailable,
            uncertain: false,
            detail: None,
        };
        let message =
            native::connect_error_message(&unavailable, std::path::Path::new("/usr/bin/python3"));
        assert!(message.contains("setup-account-bridge.sh"));
        assert!(message.contains("Advanced"));
        // The message must name the interpreter that was actually tried.
        assert!(message.contains("/usr/bin/python3"));
        // Other failures stay as they were: no misleading install advice.
        let timeout = muse_account::AccountError {
            code: muse_account::ErrorCode::Timeout,
            uncertain: false,
            detail: None,
        };
        assert!(
            !native::connect_error_message(&timeout, std::path::Path::new("/usr/bin/python3"))
                .contains("setup-account-bridge.sh")
        );
    }

    #[test]
    fn only_a_bare_system_interpreter_is_substituted() {
        assert!(native::is_system_fallback(std::path::Path::new(
            "/usr/bin/python3"
        )));
        // A bridge environment or an explicit override is never replaced.
        assert!(!native::is_system_fallback(std::path::Path::new(
            "/home/user/.local/share/unofficial-muse-for-linux/account-bridge/.venv/bin/python"
        )));
        assert!(!native::is_system_fallback(std::path::Path::new(
            "/opt/py/bin/python"
        )));
    }

    #[test]
    fn sign_in_requires_consent_and_does_not_auto_connect_or_poll() {
        let mut state = State::default();
        assert!(state.begin_login(false).is_none());
        let ticket = state.begin_login(true).unwrap();
        assert!(state.begin_connect(true).is_none());
        state.finish_login(&ticket, Ok(()));
        assert!(state.account.is_none());
        assert!(state.begin_read(false, true).is_none());
        // The UI hands off to the explicit link step after a successful sign-in;
        // the controller itself still never connects on its own.
        assert!(state.connection_notice.contains("Signed in"));
    }

    #[test]
    fn externally_changed_account_requires_fresh_consent_without_old_data() {
        let mut state = connected();
        state.history.push("private".into());
        state.tasks = Some("private tasks".into());
        let old = state.begin_read(false, true).unwrap();
        state.account_changed();
        assert!(state.account.is_none());
        assert!(state.history.is_empty());
        assert!(state.tasks.is_none());
        assert!(state.connection_notice.contains("changed"));
        assert!(!state.finish_refresh(
            &old,
            Ok("new identity".into()),
            Ok(vec!["new data".into()]),
            Ok("new tasks".into())
        ));
        assert!(state.begin_read(false, true).is_none());
    }

    fn connected() -> State {
        let mut state = State::default();
        let ticket = state.begin_connect(true).unwrap();
        state.finish_connect(&ticket, Ok("account".into()));
        state
    }

    #[test]
    fn a_reply_poll_holds_its_own_slot_and_never_blocks_sending() {
        let mut state = connected();
        let poll = state.begin_poll().unwrap();
        // The composer is not gated by a poll, so a send can start behind it.
        assert!(state.busy.is_none());
        let send = state.begin_send("hello").unwrap();
        assert!(matches!(send.operation, Operation::Send));
        assert!(state.begin_poll().is_none(), "one poll at a time");
        // The poll resolving must not clear the send that is still running.
        assert!(state.finish_history(&poll, Ok(Vec::new())));
        assert!(state.polling.is_none());
        assert!(
            state.busy.is_none(),
            "a dispatch must not occupy the read slot"
        );
        assert!(
            state.sending.is_some(),
            "the poll resolving must not clear the send that is still running"
        );
    }

    #[test]
    fn disconnected_controller_never_polls_or_dispatches() {
        let mut state = State::default();
        assert!(state.begin_read(false, true).is_none());
        assert!(state.begin_read(true, false).is_none());
        assert!(state.begin_send("must not leave this device").is_none());
        assert!(state.busy.is_none());
    }

    #[test]
    fn returning_to_same_scope_still_rejects_old_epoch() {
        let mut state = connected();
        let old_main = state.begin_read(false, false).unwrap();
        state.select_thread(Some("other".into()));
        state.select_thread(None);
        let new_main = state.begin_read(false, false).unwrap();
        assert!(!state.finish_history(&old_main, Ok(vec!["stale same-scope message".into()])));
        assert_eq!(state.busy.as_ref(), Some(&new_main));
        state.finish_history(&new_main, Ok(vec!["fresh".into()]));
        assert_eq!(state.history, vec!["fresh"]);
    }

    #[test]
    fn expired_session_drops_account_data_and_points_at_sign_in() {
        let mut state = connected();
        state.history.push("previous history".into());
        let ticket = state.begin_read(false, true).unwrap();
        state.session_expired();
        assert!(state.account.is_none());
        assert!(state.history.is_empty());
        assert!(state.busy.is_none());
        assert!(state.connection_notice.contains("Sign in again"));
        // The in-flight read can never reinstall stale data after the reset.
        assert!(!state.finish_history(&ticket, Ok(vec!["stale".into()])));
        assert!(state.history.is_empty());
    }

    #[test]
    fn expired_session_keeps_the_uncertain_dispatch_warning() {
        let mut state = connected();
        state.begin_send("private message").unwrap();
        state.session_expired();
        assert!(state.pending_send.is_none());
        assert!(state.send_notice.contains("not cancelled or retried"));
    }

    #[test]
    fn disconnect_during_dispatch_clears_private_text_and_rejects_completion() {
        let mut state = connected();
        let old = state.begin_send("private message").unwrap();
        state.disconnect();
        assert!(state.pending_send.is_none());
        assert!(state.send_notice.contains("not cancelled or retried"));
        assert!(!state.finish_send(&old, SendOutcome::Confirmed));
        assert!(state.account.is_none());
        assert!(state.history.is_empty());
    }

    #[test]
    fn confirmed_and_known_unsent_results_do_not_trigger_another_dispatch() {
        let mut state = connected();
        let first = state.begin_send("once").unwrap();
        state.finish_send(&first, SendOutcome::Confirmed);
        assert!(state.pending_send.is_none());
        assert!(state.busy.is_none());
        // A confirmed send leaves no standing text above the composer.
        assert!(state.send_notice.is_empty());
        let second = state.begin_send("explicit second attempt").unwrap();
        state.finish_send(
            &second,
            SendOutcome::NotSent("authentication failed before dispatch".into()),
        );
        assert!(state.pending_send.is_none());
        assert!(state.busy.is_none());
        assert!(state.send_notice.contains("Not sent:"));
        assert!(!state.finish_send(&first, SendOutcome::Confirmed));
    }

    #[test]
    fn refresh_surfaces_each_failure_and_retains_last_successful_data() {
        let mut state = connected();
        state.history.push("previous history".into());
        state.tasks = Some("previous tasks".into());
        let ticket = state.begin_read(false, true).unwrap();
        state.finish_refresh(
            &ticket,
            Err("authentication required".into()),
            Err("network timeout".into()),
            Err("task service unavailable".into()),
        );
        assert!(state.connection_notice.contains("authentication required"));
        assert!(state.history_notice.contains("network timeout"));
        assert!(state.tasks_notice.contains("task service unavailable"));
        assert_eq!(state.history, vec!["previous history"]);
        assert_eq!(state.tasks.as_deref(), Some("previous tasks"));
        assert!(state.busy.is_none());
    }

    #[test]
    fn uncertain_send_retains_scope_blocks_resend_and_survives_history_refresh() {
        let mut state = connected();
        state.select_thread(Some("original-thread".into()));
        let ticket = state.begin_send("original unmodified text ").unwrap();
        assert!(state.begin_send("duplicate").is_none());
        assert!(!state.select_thread(Some("different-thread".into())));
        state.finish_send(&ticket, SendOutcome::Uncertain("deadline exceeded".into()));
        assert_eq!(
            state.pending_send.as_ref().unwrap(),
            &PendingSend {
                scope: Some("original-thread".into()),
                text: "original unmodified text ".into()
            }
        );
        assert!(state.send_notice.contains("deadline exceeded"));
        assert!(state.begin_send("duplicate").is_none());
        let refresh = state.begin_read(false, true).unwrap();
        state.finish_refresh(
            &refresh,
            Ok("account".into()),
            Ok(vec![]),
            Ok("tasks".into()),
        );
        assert!(state.pending_send.is_some());
        assert!(state.send_notice.contains("deadline exceeded"));
        state.acknowledge_send();
        assert!(state.begin_send("a deliberate new message").is_some());
    }

    #[test]
    fn blocked_send_explains_why_instead_of_silently_doing_nothing() {
        // Regression for the user-visible "chat doesn't work" report: every
        // begin_send refusal must map to an actionable notice, never silence.
        let mut disconnected = State::default();
        assert!(disconnected.begin_send("hello").is_none());
        assert!(
            disconnected
                .describe_send_block("hello")
                .contains("Connect")
        );
        let mut ready = connected();
        assert!(ready.begin_send("   ").is_none());
        assert!(ready.describe_send_block("   ").contains("Enter"));
        let first = ready.begin_send("first").unwrap();
        // Two unresolved dispatches at once are still refused: the receipt of
        // the first decides whether the second would duplicate it.
        assert!(ready.begin_send("second").is_none());
        assert!(
            ready
                .describe_send_block("second")
                .contains("still being dispatched")
        );
        ready.finish_send(&first, SendOutcome::Confirmed);
        // Interjection: a background read holding the shared slot must not refuse
        // the composer, which is what "a request is already running" did mid-turn.
        let read = ready.begin_read(false, false).unwrap();
        assert!(ready.busy.is_some(), "the read must own the shared slot");
        let interjection = ready
            .begin_send("steer it")
            .expect("a running read must not block a send");
        assert_eq!(interjection.operation, Operation::Send);
        assert!(ready.finish_send(&interjection, SendOutcome::Confirmed));
        assert!(ready.finish_history(&read, Ok(vec![])));
        let _busy = ready.begin_send("after").unwrap();
        ready.finish_send(&_busy, SendOutcome::Uncertain("deadline exceeded".into()));
        assert!(
            ready
                .describe_send_block("second")
                .contains("allow another send")
        );
    }

    #[test]
    fn history_window_expands_only_explicitly_is_capped_and_resets_per_thread() {
        let mut state = connected();
        for expected in [100, 150, 200] {
            let ticket = state.begin_read(true, false).unwrap();
            assert_eq!(ticket.limit, expected);
            assert!(state.begin_read(true, false).is_none());
            assert_eq!(state.limit, expected);
            state.finish_history(&ticket, Ok(vec![]));
            let refresh = state.begin_read(false, true).unwrap();
            assert_eq!(refresh.limit, expected);
            state.finish_history(&refresh, Ok(vec![]));
        }
        assert!(state.begin_read(true, false).is_none());
        state.select_thread(Some("second".into()));
        assert_eq!(state.limit, 50);
    }

    #[test]
    fn thread_switch_clears_history_and_rejects_inflight_old_thread_results() {
        let mut state = connected();
        state.history.push("main message".into());
        let main = state.begin_read(false, false).unwrap();
        assert!(state.select_thread(Some("actual-service-session".into())));
        assert!(state.history.is_empty());
        let selected = state.begin_read(false, false).unwrap();
        assert_eq!(selected.scope.as_deref(), Some("actual-service-session"));
        assert!(!state.finish_history(&main, Ok(vec!["stale main".into()])));
        assert_eq!(state.busy.as_ref(), Some(&selected));
        assert!(state.finish_history(&selected, Ok(vec!["thread message".into()])));
        assert_eq!(state.history, vec!["thread message"]);
    }

    #[test]
    fn disconnect_clears_account_data_and_rejects_previous_account_completion() {
        let mut state = State::default();
        let old = state.begin_connect(true).unwrap();
        assert!(state.finish_connect(&old, Ok("first account".into())));
        state.history.push("private old message".into());
        state.tasks = Some("old tasks".into());
        state.disconnect();
        assert!(state.account.is_none());
        assert!(state.history.is_empty());
        assert!(state.tasks.is_none());
        let fresh = state.begin_connect(true).unwrap();
        assert!(!state.finish_connect(&old, Ok("stale account".into())));
        assert_eq!(state.busy.as_ref(), Some(&fresh));
        assert!(state.finish_connect(&fresh, Ok("second account".into())));
        assert_eq!(state.account.as_deref(), Some("second account"));
    }

    #[test]
    fn connection_locks_other_connect_attempts_until_completion() {
        let mut state = State::default();
        let ticket = state.begin_connect(true).unwrap();
        assert_eq!(state.busy.as_ref(), Some(&ticket));
        assert!(state.begin_connect(true).is_none());
    }

    #[test]
    fn connect_requires_explicit_session_access_consent() {
        let mut state = State::default();
        assert!(state.begin_connect(false).is_none());
        assert!(state.busy.is_none());
        assert!(state.account.is_none());
    }
}
