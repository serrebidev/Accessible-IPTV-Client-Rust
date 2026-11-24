//! Planning exports (port of Python `planning_tools.py`):
//! the calendar (.ics) export for a programme or reminder,
//! and the atomic file write every export uses.

use sha2::Digest;

/// RFC 5545 TEXT escaping (Python `ical_text`): control
/// characters other than tab and line breaks are eliminated,
/// then backslash, line breaks, semicolons and commas are
/// escaped, in Python's order.
#[must_use]
pub fn ical_text(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .filter(|c| (*c as u32) >= 32 || matches!(c, '\n' | '\r' | '\t'))
        .collect();
    cleaned
        .replace('\\', "\\\\")
        .replace("\r\n", "\\n")
        .replace(['\r', '\n'], "\\n")
        .replace(';', "\\;")
        .replace(',', "\\,")
}

/// Fold one content line to 75 octets per physical line
/// (Python `fold_ical`), never splitting a UTF-8 character;
/// continuation lines start with one space.
#[must_use]
pub fn fold_ical(line: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut length = 0usize;
    for c in line.chars() {
        let size = c.len_utf8();
        if length + size > 75 {
            lines.push(std::mem::take(&mut current));
            current.push(' ');
            length = 1;
        }
        current.push(c);
        length += size;
    }
    lines.push(current);
    lines.join("\r\n")
}

fn stamp(epoch: i64) -> String {
    chrono::TimeZone::timestamp_opt(&chrono::Utc, epoch, 0)
        .single()
        .map(|dt| dt.format("%Y%m%dT%H%M%SZ").to_string())
        .unwrap_or_default()
}

/// One programme as a VCALENDAR (Python `programme_calendar`).
/// The UID hashes the identity with the title, channel and
/// start, so no provider locator or secret enters the export.
///
/// # Errors
/// Python's message when the end is not after the start.
pub fn programme_calendar(
    title: &str,
    channel: &str,
    start: i64,
    end: i64,
    identity: &str,
    now: i64,
) -> Result<Vec<u8>, String> {
    if end <= start {
        return Err("Invalid programme times.".to_string());
    }
    let key_bytes = format!("{identity}\0{title}\0{channel}\0{start}");
    let digest = sha2::Sha256::digest(key_bytes.as_bytes());
    let mut key = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(key, "{byte:02x}");
    }
    let lines = [
        "BEGIN:VCALENDAR".to_string(),
        "VERSION:2.0".to_string(),
        "PRODID:-//Accessible IPTV Client//Programme//EN".to_string(),
        "CALSCALE:GREGORIAN".to_string(),
        "BEGIN:VEVENT".to_string(),
        format!("UID:{key}@accessible-iptv-client"),
        format!("DTSTAMP:{}", stamp(now)),
        format!("DTSTART:{}", stamp(start)),
        format!("DTEND:{}", stamp(end)),
        format!("SUMMARY:{}", ical_text(title)),
        format!("DESCRIPTION:{}", ical_text(channel)),
        "END:VEVENT".to_string(),
        "END:VCALENDAR".to_string(),
    ];
    let folded: Vec<String> = lines.iter().map(|line| fold_ical(line)).collect();
    Ok((folded.join("\r\n") + "\r\n").into_bytes())
}

/// Replace a file only with a complete export (Python
/// `atomic_export`): write to a temporary file in the same
/// directory, flush, then rename over the target; an existing
/// file survives a failure.
///
/// # Errors
/// The OS error text when the write or rename fails.
pub fn atomic_export(path: &std::path::Path, content: &[u8]) -> Result<(), String> {
    let directory = path.parent().map_or_else(
        || std::path::PathBuf::from("."),
        std::path::Path::to_path_buf,
    );
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let temporary = directory.join(format!(".iptv-export-{}-{stamp}", std::process::id()));
    let result = (|| -> Result<(), String> {
        std::fs::write(&temporary, content).map_err(|e| e.to_string())?;
        std::fs::rename(&temporary, path).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ical_text_escapes_in_python_order() {
        assert_eq!(ical_text("a;b,c\\d\ne"), "a\\;b\\,c\\\\d\\ne");
        assert_eq!(ical_text("x\u{1}y"), "xy");
        assert_eq!(ical_text("a\r\nb"), "a\\nb");
    }

    #[test]
    fn fold_ical_breaks_at_75_octets_without_splitting() {
        let line = "x".repeat(80);
        let folded = fold_ical(&line);
        let physical: Vec<&str> = folded.split("\r\n").collect();
        assert_eq!(physical.len(), 2);
        assert_eq!(physical[0].len(), 75);
        assert!(physical[1].starts_with(' '));
        // A multi-byte character at the boundary moves whole.
        let line = format!("{}é{}", "x".repeat(74), "y".repeat(10));
        let folded = fold_ical(&line);
        let physical: Vec<&str> = folded.split("\r\n").collect();
        assert_eq!(physical[0].len(), 74);
        assert!(physical[1].starts_with(" é"));
    }

    #[test]
    fn programme_calendar_shape_and_uid() {
        let bytes = programme_calendar(
            "My Show",
            "Chan; One",
            1_800_000_000,
            1_800_003_600,
            "ident",
            1_799_000_000,
        )
        .expect("calendar");
        let text = String::from_utf8(bytes).expect("utf8");
        assert!(text.starts_with("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n"));
        assert!(text.contains("DTSTART:20270115T080000Z\r\n"));
        assert!(text.contains("SUMMARY:My Show\r\n"));
        assert!(text.contains("DESCRIPTION:Chan\\; One\r\n"));
        // The 91-octet UID line folds mid-token, as Python's
        // does; unfolding restores it.
        let unfolded = text.replace("\r\n ", "");
        assert!(unfolded.contains("@accessible-iptv-client\r\n"));
        assert!(text.contains("\r\n "));
        assert!(text.ends_with("END:VCALENDAR\r\n"));
        assert_eq!(
            programme_calendar("T", "C", 100, 100, "", 0),
            Err("Invalid programme times.".to_string())
        );
    }

    #[test]
    fn atomic_export_writes_and_replaces() {
        let dir = std::env::temp_dir().join(format!("iptv-export-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("out.ics");
        atomic_export(&path, b"one").expect("write");
        assert_eq!(std::fs::read(&path).expect("read"), b"one");
        atomic_export(&path, b"two").expect("replace");
        assert_eq!(std::fs::read(&path).expect("read"), b"two");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
