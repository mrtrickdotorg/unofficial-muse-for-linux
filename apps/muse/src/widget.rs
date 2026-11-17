//! Live widget frames for the rewrite: the Mac client renders a
//! unit's `widget_html` in a locked-down frame and falls back to the
//! extracted text when the renderer cannot. Same shapes here: the
//! frame shows service markup, never browses.
//!
//! The frame is a renderer, not a browser: the only navigation it
//! exists for is its own `about:blank` document. A link the reader
//! clicks hands its URL to the system browser; anything else is
//! dropped. JavaScript stays on so computed content (countdowns,
//! timelines) renders the way the Mac shows it.

use std::rc::Rc;

/// A measured pixel height out of a JS evaluation answer. The
/// answer's string form varies (bare digits, quoted strings), so this
/// keeps only the number instead of trusting one shape; `None` logs
/// the raw answer once so a silent frame can never hide again.
fn js_height(raw: &str) -> Option<f64> {
    let digits: String = raw
        .chars()
        .filter(|mark| mark.is_ascii_digit() || *mark == '.')
        .collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse::<f64>().ok()
}

/// Wrap service markup in a minimal dark document shell. The shell
/// kills the defaults that would frame the unit in white: the
/// WebView paints white where nothing covers it, so the ground and
/// the body margin are set, not left to the UA stylesheet.
fn document(html: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"> \
         <style>html,body{{margin:0;padding:0;background:#1e1e1e;color:#e4e6eb}} \
         body{{font-family:system-ui,sans-serif;overflow:hidden}} \
         img{{max-width:100%}}</style> \
         </head><body>{html}</body></html>"
    )
}

/// True when constructing a WebView is safe. Inside the AppImage the
/// helpers must be staged (see `WEBKIT_STAGING`) and actually
/// executable; a frame without working helpers aborts the process, so
/// callers show the fallback text instead. Outside the bundle the
/// system manages its own helpers.
fn helpers_present(dir: &std::path::Path) -> bool {
    dir.join("WebKitNetworkProcess").is_file()
}

/// Whether the staged helpers load: probed once per process by
/// running the network helper and watching what happens to the child.
/// A child that dies with 127 (the loader's "cannot open shared
/// object file") or on a signal means broken helpers; anything else —
/// including a child still alive at the deadline — means the
/// libraries resolve and frames are safe to build. The probe child
/// can never take this process down, which is the whole point: an
/// `ld.so` failure aborts whoever loads, so only a disposable child
/// may attempt it.
fn helpers_healthy() -> bool {
    use std::sync::atomic::{AtomicI8, Ordering};
    static HEALTH: AtomicI8 = AtomicI8::new(-1);
    match HEALTH.load(Ordering::Relaxed) {
        1 => return true,
        0 => return false,
        _ => {}
    }
    let healthy = probe_helper_health();
    HEALTH.store(if healthy { 1 } else { 0 }, Ordering::Relaxed);
    if !healthy {
        eprintln!("muse: WebKit helpers failed their load probe, graphics stay text-only");
    }
    healthy
}

fn probe_helper_health() -> bool {
    use std::process::Command;
    // Every helper WebKit will spawn: the loader failure names the
    // web process, and a network-only probe once passed while the web
    // helper died. Each runs as a disposable scrubbed child; the first
    // failure names itself in the log.
    for helper in [
        "WebKitWebProcess",
        "WebKitNetworkProcess",
        "WebKitGPUProcess",
    ] {
        let binary = std::path::Path::new(crate::WEBKIT_STAGING).join(helper);
        // Scrubbed exactly the way WebKit spawns its helpers: the probe
        // must resolve libraries through RUNPATH alone. Inheriting this
        // process's LD_LIBRARY_PATH (which names the bundle's lib dir)
        // would pass a probe the real spawn then fails.
        let mut child = match Command::new(&binary).env_clear().arg("--help").spawn() {
            Ok(child) => child,
            Err(_) => {
                eprintln!("muse: WebKit helper {helper} would not start");
                return false;
            }
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(800);
        let healthy = loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    if status.success() {
                        break true;
                    }
                    // 127 is the loader giving up; a signal is a crash.
                    // Any other quick exit means the binary ran.
                    #[cfg(unix)]
                    {
                        use std::os::unix::process::ExitStatusExt as _;
                        if status.signal().is_some() {
                            break false;
                        }
                    }
                    break status.code() != Some(127);
                }
                Ok(None) => {
                    if std::time::Instant::now() >= deadline {
                        let _ = child.kill();
                        break true;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(_) => break false,
            }
        };
        if !healthy {
            eprintln!("muse: WebKit helper {helper} failed its load probe");
            return false;
        }
    }
    true
}

pub(crate) fn widget_available() -> bool {
    if std::env::var_os("APPIMAGE").is_none() {
        return true;
    }
    helpers_present(std::path::Path::new(crate::WEBKIT_STAGING)) && helpers_healthy()
}

/// A widget frame for `html` with `fallback` shown when the renderer
/// cannot. The fallback is the service's own extracted text, never an
/// error dump.
pub fn widget_frame(html: &str, fallback: Option<&str>) -> gtk4::Widget {
    if !widget_available() {
        // Visible in the launch log: the next crash report can tell a
        // missing stage from a renderer failure.
        eprintln!("muse: WebKit helpers unavailable, showing text instead of a graphic");
        let label = gtk4::Label::new(fallback);
        label.set_wrap(true);
        label.set_xalign(0.0);
        return label.upcast();
    }
    use webkit6::prelude::*;
    use webkit6::{LoadEvent, NavigationPolicyDecision, PolicyDecisionType, WebView};

    let area = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
    let view = WebView::new();
    view.set_hexpand(true);
    // Width request stays at 1: a WebView's minimum follows its
    // content, and a wide widget would set the whole window's minimum
    // past the screen. The frame gets whatever width the card gives it.
    view.set_size_request(1, 120);
    if let Some(settings) = WebViewExt::settings(&view) {
        settings.set_enable_javascript(true);
        settings.set_enable_developer_extras(false);
    }
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
    let fallback_text = fallback.map(str::to_owned);
    let fallback: Rc<dyn Fn()> = {
        let area = area.clone();
        Rc::new(move || {
            while let Some(child) = area.first_child() {
                area.remove(&child);
            }
            match &fallback_text {
                Some(text) => {
                    let label = gtk4::Label::new(Some(text));
                    label.set_wrap(true);
                    label.set_xalign(0.0);
                    area.append(&label);
                }
                None => area.set_visible(false),
            }
        })
    };
    let fit = {
        let view = view.clone();
        move || {
            let view_for_fit = view.clone();
            view.evaluate_javascript(
                "String(Math.max(document.body ? document.body.scrollHeight : 0, \
                 document.documentElement ? document.documentElement.scrollHeight : 0))",
                None,
                None,
                gtk4::gio::Cancellable::NONE,
                move |result| match result {
                    Ok(value) => match js_height(&value.to_string()) {
                        Some(height) => {
                            view_for_fit.set_size_request(1, height.clamp(48.0, 1400.0) as i32);
                        }
                        None => {
                            eprintln!("muse: widget height unreadable: {value}");
                        }
                    },
                    Err(_) => {
                        eprintln!("muse: widget height measure failed");
                    }
                },
            );
        }
    };
    // Content often lays out after the load events fire (styles,
    // sub-resources, computed rows): poll the settled height a few
    // times instead of trusting one early measure, or tall units stay
    // clipped. Idempotent: a settled frame just re-applies its size.
    let settle = {
        let view = view.clone();
        move || {
            let owner = view.clone();
            let ticks = std::rc::Rc::new(std::cell::Cell::new(0));
            gtk4::glib::timeout_add_local(std::time::Duration::from_millis(400), move || {
                let seen = ticks.get() + 1;
                ticks.set(seen);
                let view_for_settle = owner.clone();
                owner.evaluate_javascript(
                    "String(Math.max(document.body ? document.body.scrollHeight : 0, \
                         document.documentElement ? document.documentElement.scrollHeight : 0))",
                    None,
                    None,
                    gtk4::gio::Cancellable::NONE,
                    move |result| {
                        if let Ok(value) = result
                            && let Some(height) = js_height(&value.to_string())
                        {
                            view_for_settle.set_size_request(1, height.clamp(48.0, 1400.0) as i32);
                        }
                    },
                );
                if seen >= 8 {
                    gtk4::glib::ControlFlow::Break
                } else {
                    gtk4::glib::ControlFlow::Continue
                }
            });
        }
    };
    view.connect_load_changed(move |_, event| {
        if matches!(event, LoadEvent::Committed | LoadEvent::Finished) {
            fit();
            if matches!(event, LoadEvent::Finished) {
                settle();
            }
        }
    });
    {
        let fallback = Rc::clone(&fallback);
        view.connect_load_failed(move |_, _, _, _| {
            fallback();
            true
        });
    }
    view.load_html(&document(html), Some("about:blank"));
    area.append(&view);
    area.upcast()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heights_read_through_quotes_and_wrappers() {
        assert_eq!(js_height("432"), Some(432.0));
        assert_eq!(js_height("\"432\""), Some(432.0));
        assert_eq!(js_height("JsValue(432)"), Some(432.0));
        assert_eq!(js_height(""), None);
        assert_eq!(js_height("null"), None);
    }

    #[test]
    fn helpers_count_only_with_the_network_process() {
        let base = std::env::temp_dir().join(format!("muse-webkit-probe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("probe dir");
        assert!(!helpers_present(&base));
        std::fs::write(base.join("WebKitNetworkProcess"), b"x").expect("probe helper");
        assert!(helpers_present(&base));
        let _ = std::fs::remove_dir_all(&base);
    }
}
