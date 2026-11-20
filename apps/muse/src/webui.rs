//! The web UI slice of the port.
//!
//! One real page, rendered the way the migration will render every page:
//! a WebKitGTK view (the same engine the feed widgets and the space
//! preview already use in-window), a plain HTML/CSS/JS document from
//! `apps/muse/ui`, and the Rust core as the only source of data. No
//! second browser engine, no dev server, no bundler yet: the assets are
//! `include_str!`ed, so the AppImage stays one binary plus the sidecars.
//!
//! What this proves, per `docs/tauri-port-decision.md` stage S1: the web
//! renderer can hold the same rows the native page holds and look like
//! the Kyth Hub family. What it deliberately does not do yet: mutations
//! and commands from the page back into Rust (the JS-to-Rust channel),
//! which arrives with the first page that needs it.

use muse_account::ArtifactSummary;

/// Whether a WebView can be constructed at all (helpers staged and
/// loading). Callers disable the entry point instead of failing on press.
pub(crate) fn available() -> bool {
    crate::widget::widget_available()
}

/// The document: the HTML shell with the shared web stylesheet, the page
/// script, and the rows inlined as data. Inlined rather than fetched so
/// the page renders in one load with no local server and no custom URI
/// scheme to audit.
fn document(artifacts: &[ArtifactSummary]) -> String {
    let payload = serde_json::json!({
        "total": artifacts.len(),
        "artifacts": artifacts,
    })
    .to_string()
    // A title containing `</script>` must not be able to close the
    // data block; JSON's own escape preserves the string's meaning.
    .replace("</", "<\\/");
    include_str!("../ui/library.html")
        .replace("/*__STYLE__*/", include_str!("../ui/hub.css"))
        .replace("/*__SCRIPT__*/", include_str!("../ui/library.js"))
        .replace("/*__DATA__*/", &payload)
}

/// Open the Library page in its own window, on the rows the native page
/// last read. Returns false when this build cannot render a WebView.
pub(crate) fn open_library_window(artifacts: Vec<ArtifactSummary>) -> bool {
    if !available() {
        return false;
    }
    use webkit6::prelude::*;
    use webkit6::{NavigationPolicyDecision, PolicyDecisionType, WebView};

    let view = WebView::new();
    if let Some(settings) = WebViewExt::settings(&view) {
        settings.set_enable_javascript(true);
        settings.set_enable_developer_extras(false);
        // A local, static page gains nothing from the accelerated path, and
        // this is the one setting that keeps it visible on a seat with no
        // working GL (an Xvfb check, a VM, a broken driver). It also makes
        // the window capturable, which the accelerated surface is not.
        settings.set_hardware_acceleration_policy(webkit6::HardwareAccelerationPolicy::Never);
    }
    // Proof that the document rendered, independent of pixels: the count
    // of cards the page built and the first words it shows.
    view.connect_load_changed(|view, event| {
        if event != webkit6::LoadEvent::Finished {
            return;
        }
        view.evaluate_javascript(
            "JSON.stringify({cards: document.querySelectorAll('.card').length, \
             rows: (window.__MUSE_LIBRARY__ && window.__MUSE_LIBRARY__.artifacts || []).length, \
             err: window.__MUSE_ERR__ || '', text: document.body ? document.body.innerText.slice(0, 40) : 'no body'})",
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
    // The same navigation lock the feed frames use: a toolbar or link
    // cannot turn this window into a browser. http(s) opens in the
    // reader's own browser on a user gesture; everything else is
    // ignored, and the page itself is not a navigation.
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
    let window = gtk4::Window::builder()
        .title("Library · web view")
        .default_width(1040)
        .default_height(760)
        .build();
    window.set_child(Some(&view));
    view.load_html(&document(&artifacts), Some("about:blank"));
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

    /// The document carries the stylesheet, the script and the rows, and
    /// the rows arrive as data rather than as markup the page has to
    /// scrape. A title cannot close the data block.
    #[test]
    fn the_document_inlines_style_script_and_rows() {
        let doc = document(&[summary("Quarterly plan", true)]);
        assert!(doc.contains("--accent: #36a3ff"), "stylesheet inlined");
        assert!(doc.contains("__MUSE_LIBRARY__"), "script inlined");
        assert!(doc.contains("\"Quarterly plan\""), "rows inlined");
        assert!(doc.contains("\"pinned\":true"));
        assert!(!doc.contains("/*__STYLE__*/"));
        assert!(!doc.contains("/*__DATA__*/"));
    }

    #[test]
    fn a_title_cannot_close_the_data_block() {
        let doc = document(&[summary("</script><b>x", false)]);
        assert!(!doc.contains("</script><b>"));
        assert!(doc.contains("<\\/script><b>"));
    }
}
