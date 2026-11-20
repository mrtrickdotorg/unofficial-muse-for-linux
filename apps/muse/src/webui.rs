//! The web UI slice of the port.
//!
//! One real page, rendered the way the migration will render every page: a
//! WebKitGTK view (the engine the feed widgets and the space preview already
//! use in-window), a React app from `apps/muse/ui` built to a single inlined
//! HTML file, and the Rust core as the only source of data.
//!
//! Two halves meet here:
//!
//! - **Data in.** The boot payload is inlined into the document so the page
//!   paints immediately from what the native page last read.
//! - **Commands out.** A `UserContentManager` script-message handler named
//!   `muse` receives `{"id","cmd"}` from the page and answers through
//!   `window.museReply(id, reply)`. Commands read native state only; the
//!   first mutating command arrives with the first page that mutates.

use muse_account::ArtifactSummary;

/// Whether a WebView can be constructed at all (helpers staged and loading).
pub(crate) fn available() -> bool {
    crate::widget::widget_available()
}

/// The rows plus the service's own total, as the page's boot data. A title
/// containing `</script>` must not be able to close the data block; JSON's
/// own escape preserves the string's meaning.
fn boot_payload(artifacts: &[ArtifactSummary], total: u64) -> String {
    serde_json::json!({ "artifacts": artifacts, "total": total })
        .to_string()
        .replace("</", "<\\/")
}

/// The document: the built React page with the boot payload in it. Inlined
/// rather than fetched so the page renders in one load with no local server
/// and no custom URI scheme to audit.
fn document(artifacts: &[ArtifactSummary], total: u64) -> String {
    include_str!("../ui/built/library.html")
        .replace("/*__BOOT__*/", &boot_payload(artifacts, total))
}

/// One command from the page. Reading state only; unknown commands are
/// refused by name so a typo in the UI is visible in the page and the log.
fn handle_command(raw: &str) -> (serde_json::Value, serde_json::Value, String) {
    let Ok(request) = serde_json::from_str::<serde_json::Value>(raw) else {
        return (
            serde_json::json!(0),
            serde_json::json!({ "ok": false, "error": "malformed request" }),
            "malformed".into(),
        );
    };
    let id = request.get("id").cloned().unwrap_or(serde_json::json!(0));
    let command = request
        .get("cmd")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_owned();
    let reply = match command.as_str() {
        "library.snapshot" => {
            let (artifacts, total) = crate::pages::library_snapshot();
            serde_json::json!({ "ok": true, "artifacts": artifacts, "total": total })
        }
        other => serde_json::json!({ "ok": false, "error": format!("unknown command: {other}") }),
    };
    (id, reply, command)
}

/// Open the Library page in its own window, on the rows the native page last
/// read. Returns false when this build cannot render a WebView.
pub(crate) fn open_library_window(artifacts: Vec<ArtifactSummary>, total: u64) -> bool {
    if !available() {
        return false;
    }
    use webkit6::prelude::*;
    use webkit6::{NavigationPolicyDecision, PolicyDecisionType};

    // Registered before the view exists, so the handler is in place when the
    // document's first script runs.
    let manager = webkit6::UserContentManager::new();
    if !manager.register_script_message_handler("muse", None) {
        eprintln!("muse webui: the command channel could not be registered");
        return false;
    }
    let view = webkit6::WebView::builder()
        .user_content_manager(&manager)
        .build();
    if let Some(settings) = WebViewExt::settings(&view) {
        settings.set_enable_javascript(true);
        settings.set_enable_developer_extras(false);
        // A local, static page gains nothing from the accelerated path, and
        // this is the one setting that keeps it visible on a seat with no
        // working GL (an Xvfb check, a VM, a broken driver). It also makes
        // the window capturable, which the accelerated surface is not.
        settings.set_hardware_acceleration_policy(webkit6::HardwareAccelerationPolicy::Never);
    }

    // Commands: parse, answer from native state, hand the answer back to the
    // page's promise. The reply runs on this same main loop, so no lock or
    // thread hand-off is involved.
    let reply_view = view.clone();
    manager.connect_script_message_received(None, move |_, value| {
        let raw = value.to_str().to_string();
        let (id, reply, command) = handle_command(&raw);
        eprintln!("muse webui: command {command}");
        let script = format!("window.museReply({id}, {reply});");
        reply_view.evaluate_javascript(&script, None, None, gtk4::gio::Cancellable::NONE, |_| {});
    });

    // The same navigation lock the feed frames use: a toolbar or link cannot
    // turn this window into a browser. http(s) opens in the reader's own
    // browser on a user gesture; everything else is ignored, and the page
    // itself is not a navigation.
    view.connect_decide_policy(|_, decision, kind| {
        if kind != PolicyDecisionType::NavigationAction {
            return false;
        }
        let Some(navigation) = decision.downcast_ref::<NavigationPolicyDecision>() else {
            return false;
        };
        let attempted = navigation.navigation_action().and_then(|mut action| {
            let gesture = action.is_user_gesture();
            action
                .request()
                .map(|request| (request.uri().unwrap_or_default(), gesture))
        });
        if let Some((uri, gesture)) = attempted {
            if uri == "about:blank" {
                return false;
            }
            if gesture && (uri.starts_with("https://") || uri.starts_with("http://")) {
                let _ = gtk4::gio::AppInfo::launch_default_for_uri(
                    &uri,
                    None::<&gtk4::gio::AppLaunchContext>,
                );
            }
            decision.ignore();
            return true;
        }
        false
    });

    // Proof that the document rendered, independent of pixels, and that the
    // channel answered: the count of cards the page built, how many rows the
    // page holds, and any script error. Read after a beat so the React
    // effects and the first command round trip have finished.
    view.connect_load_changed(|view, event| {
        if event != webkit6::LoadEvent::Finished {
            return;
        }
        let probe = view.clone();
        gtk4::glib::timeout_add_local_once(std::time::Duration::from_millis(700), move || {
            probe.evaluate_javascript(
                "JSON.stringify({cards: document.querySelectorAll('.card').length, \
                 rows: (window.__MUSE_ROWS__ || 0), \
                 boot: (window.__MUSE_BOOT__ && window.__MUSE_BOOT__.artifacts || []).length, \
                 err: window.__MUSE_ERR__ || '', \
                 text: document.body ? document.body.innerText.slice(0, 40) : 'no body'})",
                None,
                None,
                gtk4::gio::Cancellable::NONE,
                |result| {
                    if let Ok(value) = result {
                        eprintln!("muse webui: {}", value.to_string().trim_matches('"'));
                    }
                },
            );
        });
    });

    let window = gtk4::Window::builder()
        .title("Library · web view")
        .default_width(1040)
        .default_height(760)
        .build();
    window.set_child(Some(&view));
    view.load_html(&document(&artifacts, total), Some("about:blank"));
    window.present();
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(title: &str, pinned: bool) -> ArtifactSummary {
        ArtifactSummary {
            artifact_ref: "ref".into(),
            title: title.into(),
            kind: Some("file".into()),
            format: Some("markdown".into()),
            status: None,
            placement: None,
            path: None,
            target_kind: Some("file".into()),
            slug: None,
            updated_at_ms: Some(1_700_000_000_000),
            pinned,
            can_open: true,
            can_share: false,
        }
    }

    /// The built page carries its stylesheet and script inline, and the rows
    /// arrive as the boot payload rather than as markup to scrape.
    #[test]
    fn the_document_carries_style_script_and_rows() {
        let doc = document(&[summary("Quarterly plan", true)], 76);
        assert!(doc.contains("--accent: #36a3ff"), "stylesheet inlined");
        assert!(doc.contains("__MUSE_BOOT__"), "boot block present");
        assert!(doc.contains("Quarterly plan"), "rows inlined");
        assert!(doc.contains("\"total\":76"), "service total inlined");
        assert!(doc.contains("\"pinned\":true"));
        assert!(!doc.contains("/*__BOOT__*/"));
        assert!(!doc.contains("<script src="), "no fetched script");
        assert!(
            !doc.contains("<link rel=\"stylesheet\""),
            "no fetched stylesheet"
        );
    }

    #[test]
    fn a_title_cannot_close_the_data_block() {
        let doc = document(&[summary("</script><b>x", false)], 1);
        assert!(!doc.contains("</script><b>"));
        assert!(doc.contains("<\\/script><b>"));
    }

    /// The channel answers a snapshot from the native side and refuses an
    /// unknown command by name instead of guessing.
    #[test]
    fn commands_answer_or_refuse_by_name() {
        let (id, reply, command) = handle_command(r#"{"id":7,"cmd":"library.snapshot"}"#);
        assert_eq!(id, serde_json::json!(7));
        assert_eq!(command, "library.snapshot");
        assert_eq!(reply["ok"], true);
        assert!(reply.get("artifacts").is_some());
        assert!(reply.get("total").is_some());

        let (_, reply, command) = handle_command(r#"{"id":8,"cmd":"library.delete"}"#);
        assert_eq!(command, "library.delete");
        assert_eq!(reply["ok"], false);
        assert_eq!(reply["error"], "unknown command: library.delete");

        let (id, reply, _) = handle_command("not json");
        assert_eq!(id, serde_json::json!(0));
        assert_eq!(reply["error"], "malformed request");
    }
}
