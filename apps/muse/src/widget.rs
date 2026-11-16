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

/// Wrap service markup in a minimal document shell.
fn document(html: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\">\
         <style>body{{font-family:system-ui,sans-serif;overflow:hidden}}</style>\
         </head><body>{html}</body></html>"
    )
}

/// A widget frame for `html` with `fallback` shown when the renderer
/// cannot. The fallback is the service's own extracted text, never an
/// error dump.
pub fn widget_frame(html: &str, fallback: Option<&str>) -> gtk4::Widget {
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
                move |result| {
                    if let Ok(value) = result
                        && let Ok(height) = value.to_string().parse::<f64>()
                    {
                        view_for_fit.set_size_request(1, height.clamp(48.0, 900.0) as i32);
                    }
                },
            );
        }
    };
    view.connect_load_changed(move |_, event| {
        if matches!(event, LoadEvent::Committed | LoadEvent::Finished) {
            fit();
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
