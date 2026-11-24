//! The session notification history (Python's
//! `named_bookmarks.notification`, which lives in that module
//! upstream; it gets its own module here because the bookmark
//! port is a separate change).

/// One history entry: when, under what caption, and the
/// message text, already privacy-filtered and truncated.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NotificationEntry {
    pub time: i64,
    pub caption: String,
    pub message: String,
}

/// Append one notification to the bounded session history
/// (Python `notification`): obvious credentials are removed
/// at entry by the sharing filter, the caption is capped at
/// 240 characters and the message at 16000, and only the
/// newest 200 entries are kept.
#[must_use]
pub fn notification(
    history: &[NotificationEntry],
    caption: &str,
    message: &str,
    now: i64,
) -> Vec<NotificationEntry> {
    let entry = NotificationEntry {
        time: now,
        caption: crate::applog::sharing_excerpt(caption)
            .chars()
            .take(240)
            .collect(),
        message: crate::applog::sharing_excerpt(message)
            .chars()
            .take(16000)
            .collect(),
    };
    let mut result: Vec<NotificationEntry> = history.to_vec();
    result.push(entry);
    if result.len() > 200 {
        result.drain(..result.len() - 200);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(caption: &str) -> NotificationEntry {
        NotificationEntry {
            time: 0,
            caption: caption.to_string(),
            message: "m".to_string(),
        }
    }

    #[test]
    fn notification_appends_and_bounds_at_200() {
        let history: Vec<NotificationEntry> = (0..200).map(|i| entry(&format!("c{i}"))).collect();
        let next = notification(&history, "new", "text", 42);
        assert_eq!(next.len(), 200);
        assert_eq!(next[0].caption, "c1");
        assert_eq!(next[199].caption, "new");
        assert_eq!(next[199].time, 42);
    }

    #[test]
    fn notification_truncates_and_filters() {
        let long_caption = "x".repeat(300);
        let next = notification(&[], &long_caption, "see https://user:pw@example.com/x", 1);
        assert_eq!(next[0].caption.chars().count(), 240);
        // The address line is redacted whole by the sharing filter.
        assert_eq!(next[0].message, "[***]");
        let long_message = "y".repeat(17000);
        let next = notification(&[], "c", &long_message, 1);
        assert_eq!(next[0].message.chars().count(), 16000);
    }
}
