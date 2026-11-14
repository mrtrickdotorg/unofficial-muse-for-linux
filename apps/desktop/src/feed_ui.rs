//! The Feed page: the connected account's feed, rendered the way the
//! web feed renders it — a per-post emoji avatar, the title with a
//! relative timestamp, the body as rich text whose links actually
//! open, and the hero image inline when the unit carries one.
//!
//! Read-mostly, like the other parity surfaces: reacting to or
//! reordering a unit are writes and stay on the Muse side, while
//! Discuss starts a chat with the post quoted (the send itself stays
//! the reader's own press). The page reads the recorded `feed.list`
//! route through the bridge when the shell shows it (and on the
//! Refresh press), and the notice reports the service's own totals,
//! so an unrecognized payload shape reads as a client gap, never as
//! an empty feed.
//!
//! The bridge projects the visual fields under the gateway's own
//! names, confirmed by the feed shape probe on Nov 12, 2026: the
//! unit's emoji avatar (`icon`, from `emoji`), its hero image
//! (`image_url`, from `attachment.hero_image_url`) and its own
//! relative timestamp (from `created_at_ms`). A prompt card heads
//! the page when the same read could fetch the feed's generating
//! prompt: its text, a paused note, and whether an edition is being
//! generated right now.

use crate::local_host::AccountContext;
use crate::rich_text;
use gtk::prelude::*;
use gtk4 as gtk;

/// Widget frames (the WebView renderer) can be switched off for
/// the whole process by main() when the AppImage's WebKit helper
/// processes could not be staged: without them a WebView traps
/// the app instead of degrading, so cards use the text view.
pub static WIDGET_FRAMES_ENABLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(true);
use muse_account::{AccountClient, AccountError, ListingItem, ServiceListing};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub struct FeedPage {
    pub root: gtk::Box,
    /// Loads (or re-reads) the feed. Fired by the shell when the page's
    /// tab is opened, so the surface shows its contents on the click
    /// that opens it; the button remains for a manual re-read.
    pub load: Rc<dyn Fn()>,
}

const EMPTY_COPY: &str = "Nothing in the feed yet — the service reported an empty list.";
/// The nominal width a hero image is scaled against when its height
/// is chosen: the reading column at its cap, minus card chrome. The
/// picture itself stretches with the window; only the height is set
/// from the image's aspect at this width, capped so a tall image
/// cannot swamp the page.
const IMAGE_COLUMN_PX: f64 = 820.0;
const IMAGE_MAX_HEIGHT: i32 = 430;
const IMAGE_MIN_HEIGHT: i32 = 120;
const IMAGE_MAX_BYTES: usize = 12 * 1024 * 1024;

struct Ui {
    epoch: Cell<u64>,
    busy: Cell<bool>,
    context: AccountContext,
    /// Starts a discussion of a post: the shell switches to the chat
    /// page with the post quoted in the composer.
    on_discuss: Rc<dyn Fn(String)>,
    list: gtk::ListBox,
    notice: gtk::Label,
    spinner: gtk::Spinner,
    /// Decoded hero images by URL; `None` marks a URL that failed, so
    /// a broken image is attempted once, not on every render.
    /// A message a completed reaction leaves for the re-read its
    /// success triggers, so the outcome survives the refresh.
    after_read: RefCell<Option<String>>,
    textures: RefCell<HashMap<String, Option<gtk::gdk::Texture>>>,
    inflight: RefCell<HashSet<String>>,
    waiting: RefCell<HashMap<String, Vec<gtk::Picture>>>,
    image_tx: mpsc::Sender<(String, Option<Vec<u8>>)>,
    image_rx: RefCell<mpsc::Receiver<(String, Option<Vec<u8>>)>>,
}

fn label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.set_selectable(true);
    label.set_max_width_chars(110);
    label
}

/// The post body the way the web shows it: the unit's markdown with
/// its inline links live. Clicking one hands it to the desktop's URI
/// launcher rather than GTK's default handler.
fn rich_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(None);
    label.set_markup(&rich_text::inline_markup(text));
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.set_selectable(true);
    label.set_max_width_chars(110);
    label.connect_activate_link(|_, uri| {
        let _ = gtk::gio::AppInfo::launch_default_for_uri(uri, None::<&gtk::gio::AppLaunchContext>);
        gtk::glib::Propagation::Stop
    });
    label
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Days since 1970-01-01 for a civil date (Howard Hinnant's
/// algorithm), so date arithmetic needs no calendar crate.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let adjusted = if month <= 2 { year - 1 } else { year };
    let era = if adjusted >= 0 {
        adjusted
    } else {
        adjusted - 399
    } / 400;
    let year_of_era = adjusted - era * 400;
    let month_prime = (month + 9) % 12;
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn parse_ymd(text: &str) -> Option<(i64, i64, i64)> {
    let date = text.get(0..10)?;
    if date.as_bytes().get(4) != Some(&b'-') || date.as_bytes().get(7) != Some(&b'-') {
        return None;
    }
    let mut parts = date.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some((year, month, day))
}

/// Parse an ISO-8601 timestamp (`YYYY-MM-DDTHH:MM[:SS[.fff]][Z|±HH:MM]`)
/// into unix seconds. A missing offset reads as UTC, which is what
/// the service stamps. Date-only text returns `None` here; the caller
/// handles it as a day, not an instant.
pub(crate) fn parse_iso_unix(text: &str) -> Option<i64> {
    let (year, month, day) = parse_ymd(text)?;
    let rest = text.get(10..)?;
    if rest.is_empty() {
        return None;
    }
    let rest = rest.strip_prefix(['T', ' '])?;
    let hour: i64 = rest.get(0..2)?.parse().ok()?;
    if rest.get(2..3)? != ":" {
        return None;
    }
    let minute: i64 = rest.get(3..5)?.parse().ok()?;
    let mut idx = 5;
    let mut second: i64 = 0;
    if rest.get(idx..idx + 1) == Some(":") {
        second = rest.get(idx + 1..idx + 3)?.parse().ok()?;
        idx += 3;
    }
    if rest.get(idx..idx + 1) == Some(".") {
        let mut end = idx + 1;
        while rest
            .get(end..end + 1)
            .is_some_and(|ch| ch.chars().all(|c| c.is_ascii_digit()))
        {
            end += 1;
        }
        idx = end;
    }
    let tail = rest.get(idx..)?;
    let offset: i64 = if tail.is_empty() || tail == "Z" || tail == "z" {
        0
    } else {
        let sign: i64 = match tail.get(0..1)? {
            "+" => 1,
            "-" => -1,
            _ => return None,
        };
        let digits: String = tail[1..].chars().filter(|ch| *ch != ':').collect();
        if digits.len() < 2 || !digits.chars().all(|ch| ch.is_ascii_digit()) {
            return None;
        }
        let offset_hour: i64 = digits.get(0..2)?.parse().ok()?;
        let offset_minute: i64 = if digits.len() >= 4 {
            digits.get(2..4)?.parse().ok()?
        } else {
            0
        };
        sign * (offset_hour * 3600 + offset_minute * 60)
    };
    if !(0..=23).contains(&hour) || !(0..=59).contains(&minute) || !(0..=60).contains(&second) {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second - offset)
}

fn month_day(year: i64, month: i64, day: i64) -> String {
    let _ = year;
    format!("{} {}", MONTHS[(month - 1) as usize], day)
}

fn relative_day(day_days: i64, today_days: i64, ymd: (i64, i64, i64)) -> String {
    match today_days - day_days {
        0 => "Today".to_string(),
        1 => "Yesterday".to_string(),
        _ => month_day(ymd.0, ymd.1, ymd.2),
    }
}

fn relative_instant(unix: i64, now: i64) -> String {
    let delta = now - unix;
    if delta >= 0 {
        if delta < 90 {
            return "just now".to_string();
        }
        if delta < 3600 {
            return format!("{}m ago", delta / 60);
        }
        if delta < 86_400 {
            return format!("{}h ago", delta / 3600);
        }
        if delta < 172_800 {
            return "Yesterday".to_string();
        }
        if delta < 7 * 86_400 {
            return format!("{}d ago", delta / 86_400);
        }
    }
    if let Ok(when) = gtk::glib::DateTime::from_unix_local(unix) {
        return month_day(
            when.year() as i64,
            when.month() as i64,
            when.day_of_month() as i64,
        );
    }
    String::new()
}

/// The web feed's relative timestamp ("1h ago") for a bridge stamp:
/// a full ISO instant when the unit carries one, else the day date
/// the bridge fell back to. A bare epoch (seconds or milliseconds,
/// the stringified form the bridge's generic stamp keys once produced
/// for feed units) also resolves. Anything else unparseable is shown
/// as-is — a raw service stamp beats a guessed one.
pub(crate) fn relative_stamp(stamp: &str, now: i64) -> String {
    let text = stamp.trim();
    if text.len() == 10
        && let Some((year, month, day)) = parse_ymd(text)
    {
        if let Ok(today) = gtk::glib::DateTime::now_local() {
            let today_days = days_from_civil(
                today.year() as i64,
                today.month() as i64,
                today.day_of_month() as i64,
            );
            return relative_day(
                days_from_civil(year, month, day),
                today_days,
                (year, month, day),
            );
        }
        return month_day(year, month, day);
    }
    if let Some(unix) = parse_iso_unix(text) {
        return relative_instant(unix, now);
    }
    if !text.is_empty()
        && text.bytes().all(|byte| byte.is_ascii_digit())
        && let Ok(value) = text.parse::<i64>()
    {
        let unix = if value > 100_000_000_000 {
            value / 1000
        } else {
            value
        };
        return relative_instant(unix, now);
    }
    text.to_string()
}

fn fetch_image_bytes(url: &str) -> Option<Vec<u8>> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent("unofficial-muse-for-linux")
        .build()
        .ok()?;
    let response = client.get(url).send().ok()?;
    if !response.status().is_success() {
        return None;
    }
    let bytes = response.bytes().ok()?;
    if bytes.len() > IMAGE_MAX_BYTES {
        return None;
    }
    Some(bytes.to_vec())
}

/// Decode downloaded image bytes (png/jpeg/webp/gif) into a texture
/// through the pixbuf loader, the decode path this GTK binding
/// exposes. Anything undecodable is a failed image, never an error
/// worth surfacing on the page.
fn decode_texture(bytes: &[u8]) -> Option<gtk::gdk::Texture> {
    let loader = gdk_pixbuf::PixbufLoader::new();
    loader.write(bytes).ok()?;
    loader.close().ok()?;
    let pixbuf = loader.pixbuf()?;
    Some(gtk::gdk::Texture::for_pixbuf(&pixbuf))
}

fn apply_texture(picture: &gtk::Picture, texture: &gtk::gdk::Texture) {
    let width = f64::from(texture.width().max(1));
    let height = f64::from(texture.height().max(1));
    // The picture is exactly the image's display box: natural size,
    // capped at the reading column's width, at the image's own
    // aspect — never stretched across the card with the image
    // floating centered in a dark field (Patrick's Nov 12 verdict
    // on the first cut: "black squares, centered giant images").
    let display_width = width.min(IMAGE_COLUMN_PX);
    let display_height = (display_width * height / width) as i32;
    picture.set_size_request(
        display_width as i32,
        display_height.clamp(IMAGE_MIN_HEIGHT, IMAGE_MAX_HEIGHT),
    );
    picture.set_paintable(Some(texture));
    picture.set_visible(true);
}

impl Ui {
    fn set_busy(&self, busy: bool) {
        self.busy.set(busy);
        self.spinner.set_spinning(busy);
        self.spinner.set_visible(busy);
    }

    /// Show `url`'s hero image in `picture` once it is available:
    /// immediately from the texture cache, or after a one-off
    /// background download whose result the drain applies to every
    /// picture waiting on that URL.
    fn attach_image(self: &Rc<Self>, picture: gtk::Picture, url: &str) {
        if let Some(cached) = self.textures.borrow().get(url) {
            if let Some(texture) = cached {
                apply_texture(&picture, texture);
            }
            return;
        }
        self.waiting
            .borrow_mut()
            .entry(url.to_string())
            .or_default()
            .push(picture);
        if self.inflight.borrow_mut().insert(url.to_string()) {
            let tx = self.image_tx.clone();
            let owned = url.to_string();
            std::thread::spawn(move || {
                let bytes = fetch_image_bytes(&owned);
                let _ = tx.send((owned, bytes));
            });
        }
    }

    fn drain_images(&self) {
        let mut landed = Vec::new();
        {
            let rx = self.image_rx.borrow();
            while let Ok(message) = rx.try_recv() {
                landed.push(message);
            }
        }
        for (url, bytes) in landed {
            self.inflight.borrow_mut().remove(&url);
            let texture = bytes.and_then(|data| decode_texture(&data));
            let waiting = self.waiting.borrow_mut().remove(&url);
            self.textures.borrow_mut().insert(url, texture.clone());
            if let (Some(texture), Some(pictures)) = (&texture, waiting) {
                for picture in &pictures {
                    apply_texture(picture, texture);
                }
            }
        }
    }

    fn feed_card(self: &Rc<Self>, item: &ListingItem) -> gtk::Box {
        let card = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        card.add_css_class("card");
        if let Some(icon) = &item.icon {
            let avatar = label(icon);
            avatar.add_css_class("feed-icon");
            avatar.set_valign(gtk::Align::Start);
            card.append(&avatar);
        }
        let content = gtk::Box::new(gtk::Orientation::Vertical, 4);
        content.set_hexpand(true);
        let title_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let title = label(&item.title);
        title.add_css_class("feed-title");
        title.set_hexpand(true);
        title_row.append(&title);
        if let Some(stamp) = &item.stamp {
            let when = label(&relative_stamp(stamp, now_unix()));
            when.add_css_class("dim-label");
            when.set_valign(gtk::Align::Start);
            when.set_wrap(false);
            title_row.append(&when);
        }
        content.append(&title_row);
        let meta: Vec<&str> = [item.kicker.as_deref(), item.category.as_deref()]
            .into_iter()
            .flatten()
            .collect();
        if !meta.is_empty() {
            let meta_label = label(&meta.join(" · "));
            meta_label.add_css_class("dim-label");
            content.append(&meta_label);
        }
        if let Some(body) = &item.body
            && body != &item.title
        {
            content.append(&rich_label(body));
        }
        if let Some(url) = &item.image_url {
            let picture = gtk::Picture::new();
            picture.add_css_class("feed-image");
            picture.set_hexpand(false);
            picture.set_halign(gtk::Align::Start);
            picture.set_visible(false);
            picture.set_size_request(-1, 180);
            content.append(&picture);
            self.attach_image(picture, url);
        }
        let live_html = item
            .widget_html
            .as_deref()
            .filter(|_| WIDGET_FRAMES_ENABLED.load(std::sync::atomic::Ordering::Relaxed));
        if let Some(html) = live_html {
            // The widget attachment, rendered live in a locked
            // frame; the text view below is its fallback.
            content.append(&widget_frame(item, html));
        } else if let Some(text) = &item.widget_text {
            let widget = gtk::Box::new(gtk::Orientation::Vertical, 4);
            widget.add_css_class("feed-widget");
            fill_widget_text(&widget, text);
            content.append(&widget);
        }
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let discuss = gtk::Button::with_label("Discuss");
        {
            let ui = Rc::clone(self);
            let item = item.clone();
            discuss.connect_clicked(move |_| {
                (ui.on_discuss)(discuss_quote(&item));
            });
        }
        actions.append(&discuss);
        if let Some(unit_id) = &item.id {
            let react = gtk::Button::with_label("❤");
            react.set_widget_name(&format!("feed-react:{unit_id}"));
            react.set_tooltip_text(Some("React to this post"));
            crate::a11y::name(&react, "React to this post");
            {
                let ui = Rc::clone(self);
                let unit_id = unit_id.clone();
                react.connect_clicked(move |_| ui.react(&unit_id));
            }
            actions.append(&react);
        }
        content.append(&actions);
        card.append(&content);
        card
    }

    /// The prompt card that heads the feed when the bridge read the
    /// prompt on the same pass: what generates this feed, whether it
    /// is paused, and whether a new edition is being generated right
    /// now. Display-only — editing the prompt or asking for a new
    /// edition are writes and stay on the Muse side.
    fn prompt_card(&self, page: &ServiceListing) -> Option<gtk::Box> {
        let prompt = page.feed_prompt.as_ref()?;
        let card = gtk::Box::new(gtk::Orientation::Vertical, 4);
        card.add_css_class("card");
        let heading = label("Your feed prompt");
        heading.add_css_class("feed-title");
        card.append(&heading);
        if let Some(text) = &prompt.text {
            card.append(&label(text));
        }
        let mut notes: Vec<String> = Vec::new();
        if !prompt.enabled {
            notes.push("Paused".to_string());
        }
        if let Some(status) = &page.feed_status {
            if status.refresh_in_progress {
                notes.push("Generating the next edition…".to_string());
            } else if let Some(kind) = &status.next_edition_kind {
                notes.push(format!("Next edition: {kind}"));
            }
        }
        if !notes.is_empty() {
            let note = label(&notes.join(" · "));
            note.add_css_class("dim-label");
            card.append(&note);
        }
        Some(card)
    }

    fn render(self: &Rc<Self>, page: &ServiceListing) {
        while let Some(row) = self.list.first_child() {
            self.list.remove(&row);
        }
        // Pictures from the previous render are gone with their rows;
        // in-flight downloads still land in the texture cache.
        self.waiting.borrow_mut().clear();
        if let Some(card) = self.prompt_card(page) {
            self.list.append(&card);
        }
        for item in &page.items {
            self.list.append(&self.feed_card(item));
        }
    }

    fn refresh(self: &Rc<Self>) {
        if self.busy.get() {
            self.notice
                .set_text("A read is already running. Wait for it to finish.");
            return;
        }
        let Some(config) = (self.context)() else {
            self.notice.set_text(
                "Connect your Muse account first: this surface belongs to the connected account. Nothing was read.",
            );
            return;
        };
        self.epoch.set(self.epoch.get().wrapping_add(1));
        let epoch = self.epoch.get();
        self.set_busy(true);
        self.notice.set_text("Reading…");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = AccountClient::new(config).feed();
            let _ = tx.send(result);
        });
        self.poll(rx, epoch);
    }

    fn poll(self: &Rc<Self>, rx: mpsc::Receiver<Result<ServiceListing, AccountError>>, epoch: u64) {
        let weak = Rc::downgrade(self);
        gtk::glib::timeout_add_local(Duration::from_millis(80), move || {
            let Some(ui) = weak.upgrade() else {
                return gtk::glib::ControlFlow::Break;
            };
            match rx.try_recv() {
                Ok(result) => {
                    if ui.epoch.get() != epoch {
                        return gtk::glib::ControlFlow::Break;
                    }
                    ui.set_busy(false);
                    ui.finish(result);
                    gtk::glib::ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => gtk::glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    if ui.epoch.get() == epoch {
                        ui.set_busy(false);
                        ui.notice.set_text(
                            "The account worker vanished before answering. Nothing was read.",
                        );
                    }
                    gtk::glib::ControlFlow::Break
                }
            }
        });
    }

    /// Send one ❤ reaction for one unit, once. The recorded
    /// consumer's body is exactly {"reaction": "❤"}; whether the
    /// unit then shows it is proven by the re-read that follows,
    /// never by the reaction's own answer.
    fn react(self: &Rc<Self>, unit_id: &str) {
        if self.busy.get() {
            self.notice
                .set_text("A read is already running. Wait for it to finish.");
            return;
        }
        let Some(config) = (self.context)() else {
            self.notice.set_text(
                "Connect your Muse account first: this surface belongs to the connected account. Nothing was sent.",
            );
            return;
        };
        self.epoch.set(self.epoch.get().wrapping_add(1));
        let epoch = self.epoch.get();
        self.set_busy(true);
        self.notice.set_text("Sending your ❤…");
        let unit_id = unit_id.to_string();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = AccountClient::new(config).feed_react(&unit_id, "❤");
            let _ = tx.send(result);
        });
        let weak = Rc::downgrade(self);
        gtk::glib::timeout_add_local(Duration::from_millis(80), move || {
            let Some(ui) = weak.upgrade() else {
                return gtk::glib::ControlFlow::Break;
            };
            match rx.try_recv() {
                Ok(result) => {
                    if ui.epoch.get() != epoch {
                        return gtk::glib::ControlFlow::Break;
                    }
                    ui.set_busy(false);
                    match result {
                        Ok(_) => {
                            *ui.after_read.borrow_mut() =
                                Some("❤ sent. The feed above has been re-read.".to_string());
                            ui.refresh();
                        }
                        Err(error) => {
                            ui.notice.set_text(&format!(
                                "Reaction failed: {}. Nothing else was changed.",
                                crate::library_ui::read_failure(error.code)
                            ));
                        }
                    }
                    gtk::glib::ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => gtk::glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    if ui.epoch.get() == epoch {
                        ui.set_busy(false);
                        ui.notice.set_text(
                            "The account worker vanished before answering. Nothing was sent.",
                        );
                    }
                    gtk::glib::ControlFlow::Break
                }
            }
        });
    }

    fn finish(self: &Rc<Self>, result: Result<ServiceListing, AccountError>) {
        match result {
            Ok(page) => {
                self.render(&page);
                if let Some(message) = self.after_read.borrow_mut().take() {
                    self.notice.set_text(&message);
                } else {
                    self.notice
                        .set_text(&crate::service_ui::summary_text(&page, EMPTY_COPY));
                }
            }
            Err(error) => {
                self.notice.set_text(&format!(
                    "Read failed: {}. Nothing was changed.",
                    crate::library_ui::read_failure(error.code)
                ));
            }
        }
    }
}

/// The fallback view of a widget attachment: the text the bridge
/// extracted from its markup, captioned so the degradation is
/// honest, one rich label per line so its links stay live.
fn fill_widget_text(area: &gtk::Box, text: &str) {
    let caption = label("Widget · text view");
    caption.add_css_class("dim-label");
    area.append(&caption);
    for line in text.lines() {
        area.append(&rich_label(line));
    }
}

/// The chrome a widget fragment renders inside: the inset's own
/// dark surface and the app's font, no margins of ours — the
/// fragment brings its own layout, exactly as it does on the web.
fn widget_document(html: &str) -> String {
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <style>html,body{{margin:0;padding:0;background:transparent;color:#f4f4f6;\
         font-family:Inter,system-ui,sans-serif;overflow:hidden}}</style>\
         </head><body>{html}</body></html>"
    )
}

/// A widget attachment rendered live: a WebView frame locked down
/// to a renderer. Navigation never happens inside it — a link the
/// reader clicks hands its URL to the system browser, and anything
/// else is dropped — so Discuss stays the native button below the
/// card and the frame can never become a browser. When the frame
/// cannot render (load failure, a dead web process), the card
/// falls back to the extracted text view.
fn widget_frame(item: &ListingItem, html: &str) -> gtk::Box {
    use webkit6::prelude::*;
    use webkit6::{LoadEvent, NavigationPolicyDecision, PolicyDecisionType, WebView};

    let area = gtk::Box::new(gtk::Orientation::Vertical, 4);
    area.add_css_class("feed-widget");
    let view = WebView::new();
    view.set_background_color(&gtk::gdk::RGBA::new(0.063, 0.063, 0.075, 1.0));
    view.set_hexpand(true);
    // The width request stays at 1 on purpose, here and in the
    // height fit below: a WebView's own minimum follows its content,
    // and a wide widget would otherwise set the whole window's
    // minimum — the shell blows out past the screen. The frame gets
    // whatever width the card gives it; the content clips inside.
    view.set_size_request(1, 120);
    if let Some(settings) = WebViewExt::settings(&view) {
        // JavaScript stays on: countdowns and timelines compute
        // themselves at display time, like the web's "in 7 days".
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
                // The widget document itself (load_html's base):
                // the one navigation the frame exists for.
                return false;
            }
            if gesture && (uri.starts_with("https://") || uri.starts_with("http://")) {
                let _ = gtk::gio::AppInfo::launch_default_for_uri(
                    &uri,
                    None::<&gtk::gio::AppLaunchContext>,
                );
            }
            decision.ignore();
            return true;
        }
        false
    });
    let fit = {
        let view = view.clone();
        move || {
            let view = view.clone();
            let view_for_fit = view.clone();
            view.evaluate_javascript(
                "String(Math.max(document.body ? document.body.scrollHeight : 0, \
                 document.documentElement ? document.documentElement.scrollHeight : 0))",
                None,
                None,
                gtk::gio::Cancellable::NONE,
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
    let fallback: Rc<dyn Fn()> = {
        let area = area.clone();
        let text = item.widget_text.clone();
        Rc::new(move || {
            while let Some(child) = area.first_child() {
                area.remove(&child);
            }
            match &text {
                Some(text) => fill_widget_text(&area, text),
                None => area.set_visible(false),
            }
        })
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
    view.connect_web_process_terminated(move |_, _| fallback());
    view.load_html(&widget_document(html), None);
    area.append(&view);
    area
}

/// The composer text a Discuss press starts from: the post, quoted
/// on one line (the composer is a single-line entry), for the reader
/// to extend with their question or send as-is.
fn discuss_quote(item: &ListingItem) -> String {
    let mut quote = format!("About this feed post: \u{201c}{}\u{201d}", item.title);
    if let Some(body) = &item.body {
        let flat = body.split_whitespace().collect::<Vec<_>>().join(" ");
        if !flat.is_empty() {
            quote.push_str(" — ");
            quote.push_str(&flat);
        }
    }
    if quote.chars().count() > 420 {
        format!("{}…", quote.chars().take(419).collect::<String>())
    } else {
        quote
    }
}

pub fn feed(context: AccountContext, on_discuss: Rc<dyn Fn(String)>) -> FeedPage {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
    root.add_css_class("page-content");
    // The page reads in the shell's reading column, like Chat: on a
    // wide window the cards cap at the column instead of stretching
    // sparse across the whole screen (Patrick's Nov 12 verdict).
    let column = gtk::Box::new(gtk::Orientation::Vertical, 12);
    column.set_halign(gtk::Align::Center);
    column.set_vexpand(true);
    column.set_size_request(crate::READING_COLUMN_START_PX, -1);
    root.append(&column);
    let heading = label("Feed");
    heading.add_css_class("page-heading");
    column.append(&heading);
    let intro = label(
        "Briefings and updates your Muse published to the connected account's feed. Links open in your browser, and Discuss starts a chat with the post quoted. Widget attachments render live, like the web. The list loads when you open this page.",
    );
    intro.add_css_class("page-subtitle");
    column.append(&intro);

    let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    toolbar.add_css_class("toolbar");
    let refresh = gtk::Button::with_label("Refresh Feed");
    refresh.add_css_class("suggested-action");
    refresh.add_css_class("primary-action");
    let spinner = gtk::Spinner::new();
    spinner.set_visible(false);
    toolbar.append(&refresh);
    toolbar.append(&spinner);
    column.append(&toolbar);

    let notice = label("Nothing read yet.");
    column.append(&notice);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_min_content_height(220);
    scroll.set_vexpand(true);
    scroll.set_child(Some(&list));
    column.append(&scroll);

    crate::bind_reading_column(root.upcast_ref(), &column);

    let (image_tx, image_rx) = mpsc::channel();
    let ui = Rc::new(Ui {
        epoch: Cell::new(0),
        busy: Cell::new(false),
        context,
        on_discuss,
        list,
        notice,
        spinner,
        after_read: RefCell::new(None),
        textures: RefCell::new(HashMap::new()),
        inflight: RefCell::new(HashSet::new()),
        waiting: RefCell::new(HashMap::new()),
        image_tx,
        image_rx: RefCell::new(image_rx),
    });
    {
        let ui = Rc::clone(&ui);
        refresh.connect_clicked(move |_| ui.refresh());
    }
    {
        // Hero images land off-thread; this drain is the page's one
        // recurring beat, applying finished downloads to their cards.
        let weak = Rc::downgrade(&ui);
        gtk::glib::timeout_add_local(Duration::from_millis(150), move || {
            let Some(ui) = weak.upgrade() else {
                return gtk::glib::ControlFlow::Break;
            };
            ui.drain_images();
            gtk::glib::ControlFlow::Continue
        });
    }

    let load = {
        let weak = Rc::downgrade(&ui);
        Rc::new(move || {
            if let Some(ui) = weak.upgrade() {
                ui.refresh();
            }
        }) as Rc<dyn Fn()>
    };
    FeedPage { root, load }
}

#[cfg(test)]
mod tests {
    use super::{days_from_civil, parse_iso_unix, relative_day, relative_instant, relative_stamp};

    #[test]
    fn civil_days_match_the_unix_epoch() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1970, 1, 2), 1);
        assert_eq!(
            days_from_civil(2000, 3, 1) - days_from_civil(2000, 2, 28),
            2
        );
        assert_eq!(days_from_civil(2026, 11, 12), 20_769);
    }

    #[test]
    fn iso_instants_parse_with_offsets_and_fractions() {
        let utc = parse_iso_unix("2026-11-12T12:00:00Z").unwrap();
        assert_eq!(utc, 20_769 * 86_400 + 12 * 3600);
        assert_eq!(parse_iso_unix("2026-11-12T07:00:00-05:00"), Some(utc));
        assert_eq!(parse_iso_unix("2026-11-12T12:00:00.250Z"), Some(utc));
        assert_eq!(parse_iso_unix("2026-11-12 12:00"), Some(utc));
        // Date-only text is a day, not an instant.
        assert_eq!(parse_iso_unix("2026-11-12"), None);
        assert_eq!(parse_iso_unix("not a stamp"), None);
        assert_eq!(parse_iso_unix("2026-13-12T12:00:00Z"), None);
    }

    #[test]
    fn epoch_digit_stamps_resolve_instead_of_printing_raw() {
        let now = 1_794_504_781;
        assert_eq!(relative_stamp("1794504781613", now), "just now");
        assert_eq!(relative_stamp("1794501181", now), "1h ago");
        assert_eq!(relative_stamp("not a stamp", now), "not a stamp");
    }

    #[test]
    fn relative_instants_read_like_the_web_feed() {
        let now = 20_769 * 86_400 + 13 * 3600;
        assert_eq!(relative_instant(now - 30, now), "just now");
        assert_eq!(relative_instant(now - 12 * 60, now), "12m ago");
        assert_eq!(relative_instant(now - 3600, now), "1h ago");
        assert_eq!(relative_instant(now - 3 * 3600, now), "3h ago");
        assert_eq!(relative_instant(now - 30 * 3600, now), "Yesterday");
        assert_eq!(relative_instant(now - 3 * 86_400, now), "3d ago");
    }

    #[test]
    fn relative_days_name_today_and_yesterday() {
        let today = days_from_civil(2026, 11, 12);
        assert_eq!(relative_day(today, today, (2026, 11, 12)), "Today");
        assert_eq!(relative_day(today - 1, today, (2026, 11, 11)), "Yesterday");
        assert_eq!(relative_day(today - 9, today, (2026, 11, 3)), "Nov 3");
    }
}

#[cfg(test)]
mod native {
    use gtk4::prelude::*;
    use std::rc::Rc;

    #[test]
    #[ignore = "needs a display; run under Xvfb"]
    fn feed_page_builds_and_presents() {
        gtk4::init().unwrap();
        let page = super::feed(Rc::new(|| None), Rc::new(|_| {}));
        let window = gtk4::Window::new();
        window.set_child(Some(&page.root));
        window.present();
    }

    #[test]
    #[ignore = "requires GTK display; run under xvfb-run --test-threads=1"]
    fn feed_cards_render_widget_text_and_discuss() {
        // A widget attachment shows as the text it carries, in an
        // inset card, and every post carries a Discuss button that
        // hands the shell the post's quote.
        use gtk4::prelude::*;
        gtk4::init().unwrap();
        let (image_tx, image_rx) = std::sync::mpsc::channel();
        let captured: Rc<std::cell::RefCell<Vec<String>>> =
            Rc::new(std::cell::RefCell::new(Vec::new()));
        let sink = Rc::clone(&captured);
        let ui = Rc::new(super::Ui {
            epoch: std::cell::Cell::new(0),
            busy: std::cell::Cell::new(false),
            context: Rc::new(|| None),
            on_discuss: Rc::new(move |text| sink.borrow_mut().push(text)),
            list: gtk4::ListBox::new(),
            notice: gtk4::Label::new(None),
            spinner: gtk4::Spinner::new(),
            after_read: std::cell::RefCell::new(None),
            textures: std::cell::RefCell::new(std::collections::HashMap::new()),
            inflight: std::cell::RefCell::new(std::collections::HashSet::new()),
            waiting: std::cell::RefCell::new(std::collections::HashMap::new()),
            image_tx,
            image_rx: std::cell::RefCell::new(image_rx),
        });
        let item = muse_account::ListingItem {
            id: Some("u1".into()),
            title: "Your UFP proposal is on the CFO's desk".into(),
            body: Some("Chris emailed that the proposal has reached the CFO.".into()),
            kind: None,
            stamp: Some("2026-11-12T14:00:00+00:00".into()),
            icon: Some("🪑".into()),
            kicker: None,
            category: None,
            image_url: None,
            widget_text: Some(
                "Decision attempt expected Friday\nUFP Lead SOC offer\nProposal submitted\nSee [the notes](https://example.com/notes) today".into(),
            ),
            widget_html: None,
        };
        let page = muse_account::ServiceListing {
            total: 1,
            recognized: 1,
            skipped: 0,
            items: vec![item],
            feed_prompt: None,
            feed_status: None,
        };
        ui.render(&page);
        let mut texts: Vec<String> = Vec::new();
        let mut discuss: Option<gtk4::Button> = None;
        fn walk(
            widget: &gtk4::Widget,
            texts: &mut Vec<String>,
            discuss: &mut Option<gtk4::Button>,
        ) {
            if let Some(label) = widget.downcast_ref::<gtk4::Label>() {
                texts.push(label.text().to_string());
            }
            let found = widget
                .downcast_ref::<gtk4::Button>()
                .filter(|button| button.label().as_deref() == Some("Discuss"));
            if let Some(button) = found {
                *discuss = Some(button.clone());
            }
            let mut child = widget.first_child();
            while let Some(current) = child {
                let next = current.next_sibling();
                walk(&current, texts, discuss);
                child = next;
            }
        }
        walk(ui.list.upcast_ref(), &mut texts, &mut discuss);
        let joined = texts.join("\n");
        assert!(
            joined.contains("UFP Lead SOC offer"),
            "widget text missing from the card: {joined}"
        );
        assert!(
            joined.contains("Widget · text view"),
            "widget caption missing: {joined}"
        );
        assert!(
            joined.contains("See the notes today"),
            "widget line links render as their text (markup stripped): {joined}"
        );
        let discuss = discuss.expect("the card carries a Discuss button");
        // The unit carries an id, so it also carries its ❤ button,
        // named for the unit it reacts to.
        fn named(root: &gtk4::Widget, name: &str) -> bool {
            if root.widget_name() == name {
                return true;
            }
            let mut child = root.first_child();
            while let Some(current) = child {
                let next = current.next_sibling();
                if named(&current, name) {
                    return true;
                }
                child = next;
            }
            false
        }
        assert!(
            named(ui.list.upcast_ref(), "feed-react:u1"),
            "the card carries its reaction button"
        );
        discuss.emit_clicked();
        let captured = captured.borrow();
        assert_eq!(captured.len(), 1);
        assert!(
            captured[0].contains("Your UFP proposal is on the CFO's desk")
                && captured[0].contains("Chris emailed"),
            "the discuss quote carries the post: {}",
            captured[0]
        );
    }

    #[test]
    fn discuss_quotes_are_one_line_and_capped() {
        let item = muse_account::ListingItem {
            id: None,
            title: "A post".into(),
            body: Some("line one\nline   two".into()),
            kind: None,
            stamp: None,
            icon: None,
            kicker: None,
            category: None,
            image_url: None,
            widget_text: None,
            widget_html: None,
        };
        assert_eq!(
            super::discuss_quote(&item),
            "About this feed post: \u{201c}A post\u{201d} — line one line two"
        );
        let long = muse_account::ListingItem {
            body: Some("word ".repeat(300)),
            ..item
        };
        assert_eq!(super::discuss_quote(&long).chars().count(), 420);
    }

    #[test]
    fn widget_document_wraps_the_fragment_verbatim() {
        let document = super::widget_document("<div id=\"w\">body</div>");
        assert!(document.contains("<div id=\"w\">body</div>"));
        assert!(document.contains("overflow:hidden"), "{document}");
        assert!(document.contains("background:transparent"), "{document}");
    }

    #[test]
    #[ignore = "requires a display and the WebKit runtime; run under Xvfb"]
    fn feed_widget_frame_renders_markup_and_fits_height() {
        use gtk4::prelude::*;
        use std::cell::RefCell;
        use std::rc::Rc;
        use webkit6::prelude::*;

        gtk4::init().expect("gtk init");
        let html = "<div style=\"font-family:sans-serif;color:#eee;width:1600px\">\
            <div style=\"font-size:13px\">Decision attempt expected Friday</div>\
            <div style=\"font-size:20px;font-weight:700\">UFP Lead SOC offer</div>\
            <div style=\"height:6px;background:#3ddc84;width:50%;margin:12px 0\"></div>\
            <div id=\"count\" style=\"font-size:18px\"></div>\
            <a href=\"https://example.com/notes\">Read the offer notes</a>\
            <script>document.getElementById('count').textContent =\
             'in 7 days (computed by JS)';</script></div>";
        let item = muse_account::ListingItem {
            id: None,
            title: "UFP Lead SOC offer".into(),
            body: Some("Decision attempt expected Friday".into()),
            kind: None,
            stamp: None,
            icon: None,
            kicker: None,
            category: None,
            image_url: None,
            widget_text: Some("UFP Lead SOC offer".into()),
            widget_html: Some(html.into()),
        };
        let frame = super::widget_frame(&item, html);
        let window = gtk4::Window::builder()
            .default_width(800)
            .default_height(600)
            .child(&frame)
            .build();
        window.present();
        let view = frame
            .first_child()
            .and_downcast::<webkit6::WebView>()
            .expect("the frame's child is the web view");
        // A 1600px-wide widget must not set the shell's floor: the
        // frame's measured minimum stays card-sized regardless of
        // the content's own width.
        let (minimum_width, _, _, _) = frame.measure(gtk4::Orientation::Horizontal, -1);
        assert!(
            minimum_width <= 420,
            "frame minimum width {minimum_width} would blow out the window"
        );

        let probe: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
        let context = gtk4::glib::MainContext::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        let mut saw_marker = false;
        while std::time::Instant::now() < deadline {
            while context.iteration(false) {}
            {
                let view = view.clone();
                let probe = Rc::clone(&probe);
                view.evaluate_javascript(
                    "document.body ? document.body.innerText : ''",
                    None,
                    None,
                    gtk4::gio::Cancellable::NONE,
                    move |result| {
                        if let Ok(value) = result {
                            *probe.borrow_mut() = value.to_string();
                        }
                    },
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
            if probe.borrow().contains("in 7 days (computed by JS)") {
                saw_marker = true;
                break;
            }
        }
        assert!(
            saw_marker,
            "the frame never rendered the JS-computed line; last text: {:?}",
            probe.borrow()
        );
        let fitted = view.height_request();
        assert!(
            (100..=900).contains(&fitted),
            "frame height {fitted} is not a plausible fitted content height"
        );
    }
}
