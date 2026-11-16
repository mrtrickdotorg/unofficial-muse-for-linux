//! Chat transcript model for the rewrite: pure builders that turn
//! service history rows into render rows. GTK construction lives with
//! the view; everything here is transport-free and unit-tested.
//!
//! The transcript view lands later in this milestone; until then the
//! live app does not reference these builders yet.
#![allow(dead_code)]

use muse_account::{HistoryMessage, MessageAttachment};

/// Who sent a transcript row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
    Other(String),
}

impl Role {
    fn of(message: &HistoryMessage) -> Self {
        match message.role.as_str() {
            "user" => Self::User,
            "assistant" => Self::Assistant,
            other => Self::Other(other.to_owned()),
        }
    }
}

/// One rendered transcript row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TranscriptRow {
    /// A day divider heading a group of messages.
    DayDivider { label: String },
    /// A chat bubble with its attachments and option buttons.
    Message {
        id: String,
        role: Role,
        text: String,
        attachments: Vec<MessageAttachment>,
        options: Vec<String>,
        selected_option: Option<String>,
    },
    /// A service notice (usage limit, errors): styled as a notice line,
    /// never as a chat bubble, so it cannot read as a reply.
    Notice { text: String },
}

/// Links worth a Mac-style preview card, pulled from an assistant
/// message's text. Plain token scan, no regex crate: whitespace
/// split, trailing punctuation stripped, http(s) only, deduped and
/// capped. Pure, so the capping and trimming are unit-tested.
pub fn extract_links(text: &str, cap: usize) -> Vec<String> {
    let mut links = Vec::new();
    for token in text.split_whitespace() {
        if links.len() >= cap {
            break;
        }
        let trimmed = token
            .trim_matches(|mark: char| ".,;:!?)]}'\"".contains(mark))
            .trim_start_matches('(')
            .trim_start_matches('<');
        if (trimmed.starts_with("https://") || trimmed.starts_with("http://"))
            && trimmed.len() <= 2048
            && !links.iter().any(|known| known == trimmed)
        {
            links.push(trimmed.to_owned());
        }
    }
    links
}

/// Widget reference tokens (`[[hatch_widget:…]]`) name the live frame
/// the row already carries as an attachment or HTML unit. Showing the
/// token text beside the frame reads as corruption -- the web shows
/// only the widget. Strip them from bubble text, always: a token with
/// no carried widget still names nothing the reader can use.
pub fn strip_widget_tokens(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("[[hatch_widget:") {
        out.push_str(&rest[..start]);
        match rest[start..].find("]]") {
            Some(end) => rest = &rest[start + end + 2..],
            None => {
                out.push_str(&rest[start..]);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Escape for Pango markup: the service's text is untrusted, and a
/// stray `&` or `<` would otherwise eat the bubble.
fn escape_pango(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for mark in text.chars() {
        match mark {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(mark),
        }
    }
    out
}

/// Replace one paired delimiter with a Pango tag, left to right. An
/// opener with no closer stays literal -- an unmatched `**` is the
/// reader's text, not our formatting.
fn replace_paired(segment: &str, delimiter: &str, open: &str, close: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    let mut rest = segment;
    while let Some(start) = rest.find(delimiter) {
        let after = &rest[start + delimiter.len()..];
        match after.find(delimiter) {
            Some(end) => {
                out.push_str(&rest[..start]);
                out.push_str(open);
                out.push_str(&after[..end]);
                out.push_str(close);
                rest = &after[end + delimiter.len()..];
            }
            None => break,
        }
    }
    out.push_str(rest);
    out
}

/// Replace `[text](url)` links with Pango anchors. Malformed brackets
/// stay literal; the url is used verbatim as the target.
fn replace_links(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    let mut rest = segment;
    while let Some(open) = rest.find('[') {
        let Some(mid) = rest[open..].find("](") else {
            break;
        };
        let Some(shut) = rest[open + mid + 2..].find(')') else {
            break;
        };
        let text = &rest[open + 1..open + mid];
        let url = &rest[open + mid + 2..open + mid + 2 + shut];
        if text.is_empty() || url.is_empty() || url.contains(char::is_whitespace) {
            out.push_str(&rest[..open + 1]);
            rest = &rest[open + 1..];
            continue;
        }
        out.push_str(&rest[..open]);
        out.push_str(&format!("<a href=\"{url}\">{text}</a>"));
        rest = &rest[open + mid + 2 + shut + 1..];
    }
    out.push_str(rest);
    out
}

/// Render one text segment's inline markdown to Pango markup. Code
/// spans split out first so nothing inside backticks is formatted;
/// links, bold, then italics apply to the rest, in that order so
/// `**` pairs win over single `*` ones. An unmatched backtick stays
/// literal with the text after it rendered normally.
fn render_inline(segment: &str) -> String {
    let mut out = String::new();
    let mut rest = segment;
    let mut normal = true;
    while let Some(tick) = rest.find('`') {
        let (chunk, after) = (&rest[..tick], &rest[tick + 1..]);
        if normal {
            out.push_str(&render_normal(chunk));
        } else {
            out.push_str("<tt>");
            out.push_str(chunk);
            out.push_str("</tt>");
        }
        normal = !normal;
        rest = after;
    }
    if normal {
        out.push_str(&render_normal(rest));
    } else {
        out.push('`');
        out.push_str(&render_normal(rest));
    }
    out
}

/// Links, then bold, then guarded italics.
fn render_normal(chunk: &str) -> String {
    replace_emphasis(&replace_paired(&replace_links(chunk), "**", "<b>", "</b>"))
}

/// Single-`*` emphasis with flanking guards: the opener needs a
/// non-space after it, the closer a non-space before it, and a `*`
/// glued to a word character on the outside edge is a literal
/// (`2*3`, `snake_case`), never emphasis. Leftover `**` pairs are
/// literal too -- bold already claimed every real pair.
fn replace_emphasis(segment: &str) -> String {
    let chars: Vec<char> = segment.chars().collect();
    let mut out = String::with_capacity(segment.len());
    let mut index = 0;
    while index < chars.len() {
        if chars[index] != '*' {
            out.push(chars[index]);
            index += 1;
            continue;
        }
        if chars.get(index + 1) == Some(&'*') {
            out.push_str("**");
            index += 2;
            continue;
        }
        let after_open = chars.get(index + 1);
        let opener_ok = matches!(after_open, Some(next) if !next.is_whitespace())
            && (index == 0 || !chars[index - 1].is_alphanumeric());
        if !opener_ok {
            out.push('*');
            index += 1;
            continue;
        }
        let mut shut = None;
        let mut scan = index + 1;
        while scan < chars.len() {
            if chars[scan] == '*'
                && chars.get(scan + 1) != Some(&'*')
                && !chars[scan - 1].is_whitespace()
                && (scan + 1 >= chars.len() || !chars[scan + 1].is_alphanumeric())
            {
                shut = Some(scan);
                break;
            }
            scan += 1;
        }
        match shut {
            Some(end) => {
                out.push_str("<i>");
                out.extend(&chars[index + 1..end]);
                out.push_str("</i>");
                index = end + 1;
            }
            None => {
                out.push('*');
                index += 1;
            }
        }
    }
    out
}

/// One bubble's text to Pango markup: widget tokens stripped, lines
/// kept, headers and list markers shaped, inline markdown rendered.
/// The whole line is escaped first, so every tag in the output is one
/// this function inserted.
pub fn render_markup(text: &str) -> String {
    let stripped = strip_widget_tokens(text);
    let mut out = String::new();
    for (number, line) in stripped.split('\n').enumerate() {
        if number > 0 {
            out.push('\n');
        }
        let escaped = escape_pango(line);
        let trimmed = escaped.trim_start();
        let hashes = trimmed.chars().take_while(|mark| *mark == '#').count();
        if (1..=6).contains(&hashes) && trimmed[hashes..].starts_with(' ') {
            let body = render_inline(trimmed[hashes + 1..].trim_start());
            if hashes <= 2 {
                out.push_str(&format!("<span size=\"larger\"><b>{body}</b></span>"));
            } else {
                out.push_str(&format!("<b>{body}</b>"));
            }
            continue;
        }
        let mut bullet = None;
        for marker in ["- ", "* "] {
            if let Some(body) = trimmed.strip_prefix(marker) {
                bullet = Some(body);
                break;
            }
        }
        if bullet.is_none()
            && let Some(dot) = trimmed.find(". ")
            && trimmed[..dot].chars().all(|mark| mark.is_ascii_digit())
            && !trimmed[..dot].is_empty()
        {
            bullet = Some(&trimmed[dot + 2..]);
        }
        if let Some(body) = bullet {
            out.push_str(&format!("• {}", render_inline(body)));
            continue;
        }
        if let Some(quoted) = trimmed.strip_prefix("&gt; ") {
            out.push_str(&format!("<i>{}</i>", render_inline(quoted)));
            continue;
        }
        out.push_str(&render_inline(&escaped));
    }
    out
}

/// The shipped usage-limit sentence, recognized case-insensitively by
/// its three load-bearing phrases. Ported from the shipped client's
/// quota fallback: the notice must surface on every at-limit send.
fn is_usage_limit_message(text: &str) -> bool {
    let folded = text.to_lowercase();
    folded.contains("reached your usage limit")
        && folded.contains("muse tokens")
        && folded.contains("reset")
}

/// Group key for day dividers: the calendar date prefix of an ISO-8601
/// timestamp (`YYYY-MM-DD`), or the whole stamp when it carries no date
/// part. Rows without a timestamp join the current group.
fn day_key(timestamp: Option<&str>) -> Option<String> {
    let stamp = timestamp?;
    if let Some(date) = stamp.split('T').next()
        && date.len() == "YYYY-MM-DD".len()
    {
        return Some(date.to_owned());
    }
    Some(stamp.to_owned())
}

/// Turn service history into render rows: day dividers on date changes,
/// quota-shaped service text as notices, everything else as bubbles.
pub fn build_rows(history: &[HistoryMessage]) -> Vec<TranscriptRow> {
    let mut rows = Vec::new();
    let mut current_day: Option<String> = None;
    for message in history {
        if let Some(key) = day_key(message.timestamp.as_deref())
            && current_day.as_deref() != Some(&key)
        {
            current_day = Some(key.clone());
            rows.push(TranscriptRow::DayDivider { label: key });
        }
        let role = Role::of(message);
        if role != Role::User && is_usage_limit_message(&message.text) {
            rows.push(TranscriptRow::Notice {
                text: message.text.clone(),
            });
            continue;
        }
        rows.push(TranscriptRow::Message {
            id: message.message_id.clone(),
            role,
            text: message.text.clone(),
            attachments: message.attachments.clone(),
            options: message.options.clone(),
            selected_option: message.selected_option.clone(),
        });
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widget_tokens_strip_cleanly_with_or_without_a_closer() {
        assert_eq!(
            strip_widget_tokens("see [[hatch_widget:abc-123]] here"),
            "see  here"
        );
        assert_eq!(strip_widget_tokens("no tokens here"), "no tokens here");
        assert_eq!(
            strip_widget_tokens("dangling [[hatch_widget:abc"),
            "dangling [[hatch_widget:abc"
        );
    }

    #[test]
    fn markup_renders_the_web_subset_and_nothing_else() {
        assert_eq!(render_markup("**bold**"), "<b>bold</b>");
        assert_eq!(render_markup("*italic*"), "<i>italic</i>");
        assert_eq!(render_markup("2*3 and snake_case"), "2*3 and snake_case");
        assert_eq!(render_markup("**unclosed"), "**unclosed");
        assert_eq!(
            render_markup("`a*b` and **c**"),
            "<tt>a*b</tt> and <b>c</b>"
        );
        assert_eq!(
            render_markup("[Pot](https://example.com/pot)"),
            "<a href=\"https://example.com/pot\">Pot</a>"
        );
        assert_eq!(render_markup("a & b <c>"), "a &amp; b &lt;c&gt;");
        assert_eq!(render_markup("### Head"), "<b>Head</b>");
        assert_eq!(render_markup("- item"), "• item");
        assert_eq!(render_markup("1. first"), "• first");
        assert_eq!(render_markup("[[hatch_widget:abc]]**bold**"), "<b>bold</b>");
    }

    #[test]
    fn link_extraction_trims_caps_and_dedupes() {
        assert!(extract_links("no links here", 2).is_empty());
        assert_eq!(
            extract_links("see (https://example.com/a).", 2),
            vec!["https://example.com/a".to_owned()]
        );
        assert_eq!(
            extract_links(
                "https://a.example/x https://a.example/x http://b.example/y",
                2
            ),
            vec![
                "https://a.example/x".to_owned(),
                "http://b.example/y".to_owned()
            ]
        );
        assert_eq!(
            extract_links("ftp://c.example/z https://d.example/w", 5),
            vec!["https://d.example/w".to_owned()]
        );
    }

    fn message(id: &str, role: &str, text: &str, timestamp: Option<&str>) -> HistoryMessage {
        HistoryMessage {
            message_id: id.to_owned(),
            seq: 0,
            role: role.to_owned(),
            text: text.to_owned(),
            timestamp: timestamp.map(str::to_owned),
            status: None,
            options: Vec::new(),
            selected_option: None,
            attachments: Vec::new(),
        }
    }

    #[test]
    fn empty_history_builds_no_rows() {
        assert!(build_rows(&[]).is_empty());
    }

    #[test]
    fn day_divider_opens_each_date_and_repeats_nothing() {
        let history = vec![
            message("a", "user", "hi", Some("2026-11-10T12:00:00Z")),
            message("b", "assistant", "hello", Some("2026-11-10T12:01:00Z")),
            message("c", "user", "again", Some("2026-11-11T09:00:00Z")),
        ];
        let rows = build_rows(&history);
        assert!(matches!(
            &rows[0],
            TranscriptRow::DayDivider { label } if label == "2026-11-10"
        ));
        assert!(matches!(&rows[1], TranscriptRow::Message { id, .. } if id == "a"));
        assert!(matches!(&rows[2], TranscriptRow::Message { id, .. } if id == "b"));
        assert!(matches!(
            &rows[3],
            TranscriptRow::DayDivider { label } if label == "2026-11-11"
        ));
        assert!(matches!(&rows[4], TranscriptRow::Message { id, .. } if id == "c"));
    }

    #[test]
    fn quota_shaped_service_text_becomes_a_notice_never_a_bubble() {
        let quota = "You've reached your usage limit. Invite friends to get \
            more Muse tokens. Your usage will reset at 6:02 PM.";
        let history = vec![
            message("q", "assistant", quota, None),
            message("u", "user", quota, None),
        ];
        let rows = build_rows(&history);
        assert!(matches!(&rows[0], TranscriptRow::Notice { .. }));
        // The reader's own echo of those words is still their bubble.
        assert!(matches!(
            &rows[1],
            TranscriptRow::Message {
                role: Role::User,
                ..
            }
        ));
    }

    #[test]
    fn options_selection_and_attachments_ride_on_the_message_row() {
        let mut answered = message("o", "assistant", "pick one", None);
        answered.options = vec!["A".to_owned(), "B".to_owned()];
        answered.selected_option = Some("B".to_owned());
        answered.attachments = vec![MessageAttachment {
            filename: "shot.png".to_owned(),
            ..MessageAttachment::default()
        }];
        let rows = build_rows(&[answered]);
        assert!(matches!(
            &rows[0],
            TranscriptRow::Message {
                options,
                selected_option: Some(_),
                attachments,
                ..
            } if options.len() == 2 && attachments.len() == 1
        ));
    }
}
