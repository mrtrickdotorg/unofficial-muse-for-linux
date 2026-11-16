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

/// The shipped usage-limit sentence, recognized case-insensitively by its
/// three load-bearing phrases. Ported from the shipped client's quota
/// fallback: the notice must surface on every at-limit send.
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
