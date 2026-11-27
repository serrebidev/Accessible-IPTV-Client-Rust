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

/// The Scheduled Recordings filter (Python PR #46
/// `filter_recording_schedule`): a title/channel word match
/// plus one of four views — `all`, `week` (unpaused bookings
/// starting within the next seven days), `problems` (failed,
/// missed and partial) and `paused` (bookings inside a pause
/// window, plus series rules whose pause is current or upcoming
/// even when the rule itself already completed). Rust's
/// `waiting` status counts as Python's `scheduled` here: it is
/// a scheduled booking held by a conflict or a limit.
#[must_use]
pub fn filter_recording_schedule(
    jobs: &[crate::dvr::DvrJob],
    query: &str,
    view: &str,
    now: f64,
) -> Vec<crate::dvr::DvrJob> {
    use crate::dvr::{
        paused_occurrence, STATUS_CANCELED, STATUS_FAILED, STATUS_MISSED, STATUS_PARTIAL,
        STATUS_SCHEDULED, STATUS_WAITING,
    };
    let words: Vec<String> = query.to_lowercase().split_whitespace().map(str::to_string).collect();
    let local_now = chrono::DateTime::from_timestamp(now as i64, 0)
        .map(|utc| utc.with_timezone(&chrono::Local))
        .unwrap_or_else(chrono::Local::now);
    let week_end = (local_now + chrono::Duration::days(7)).timestamp() as f64;
    let today = local_now.format("%Y-%m-%d").to_string();
    let mut rows = Vec::new();
    for job in jobs {
        let scheduled = job.status == STATUS_SCHEDULED || job.status == STATUS_WAITING;
        let paused = scheduled && paused_occurrence(job);
        if view == "week"
            && !(scheduled && !paused && now <= job.start_ts && job.start_ts < week_end)
        {
            continue;
        }
        if view == "problems"
            && ![STATUS_FAILED, STATUS_MISSED, STATUS_PARTIAL].contains(&job.status.as_str())
        {
            continue;
        }
        if view == "paused" {
            let is_rule = job.repeat == "series" && job.status != STATUS_CANCELED;
            let in_window = job
                .pause_dates
                .as_ref()
                .is_some_and(|dates| dates[1] >= today);
            if !(in_window && (scheduled || is_rule)) {
                continue;
            }
        }
        let text = format!("{} {} {}", job.title, job.display_title, job.channel_name)
            .to_lowercase();
        if words.iter().all(|word| text.contains(word.as_str())) {
            rows.push(job.clone());
        }
    }
    rows
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
fn sha256_hex(bytes: &[u8]) -> String {
    let digest = sha2::Sha256::digest(bytes);
    let mut key = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(key, "{byte:02x}");
    }
    key
}

/// Python `_calendar`: one VCALENDAR holding every event, each
/// validated as a finite, forward window.
fn calendar(events: &[(String, String, String, i64, i64)], now: i64) -> Result<Vec<u8>, String> {
    let mut lines = vec![
        "BEGIN:VCALENDAR".to_string(),
        "VERSION:2.0".to_string(),
        "PRODID:-//Accessible IPTV Client//Programme//EN".to_string(),
        "CALSCALE:GREGORIAN".to_string(),
    ];
    for (key, title, channel, start, end) in events {
        if *end <= *start {
            return Err("Invalid programme times.".to_string());
        }
        lines.extend([
            "BEGIN:VEVENT".to_string(),
            format!("UID:{key}@accessible-iptv-client"),
            format!("DTSTAMP:{}", stamp(now)),
            format!("DTSTART:{}", stamp(*start)),
            format!("DTEND:{}", stamp(*end)),
            format!("SUMMARY:{}", ical_text(title)),
            format!("DESCRIPTION:{}", ical_text(channel)),
            "END:VEVENT".to_string(),
        ]);
    }
    lines.push("END:VCALENDAR".to_string());
    let folded: Vec<String> = lines.iter().map(|line| fold_ical(line)).collect();
    Ok((folded.join("\r\n") + "\r\n").into_bytes())
}

/// # Errors
/// Returns "Invalid programme times." when the window is empty.
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
    let key = sha256_hex(format!("{identity}\0{title}\0{channel}\0{start}").as_bytes());
    calendar(
        &[(key, title.to_string(), channel.to_string(), start, end)],
        now,
    )
}

/// Python `recording_calendar`: the selected upcoming bookings
/// as one calendar. Every job must be scheduled, unpaused and
/// still in the future; the UID derives from the booking id
/// alone, so an edited booking keeps its UID (and no provider
/// locator or secret enters the export).
///
/// # Errors
/// Python's messages for an ineligible selection and for an
/// invalid window.
#[allow(clippy::cast_possible_truncation)] // schedule times are whole seconds
pub fn recording_calendar(jobs: &[crate::dvr::DvrJob], now: f64) -> Result<Vec<u8>, String> {
    let mut events: Vec<(String, String, String, i64, i64)> = Vec::new();
    for job in jobs {
        if job.status != crate::dvr::STATUS_SCHEDULED
            || crate::dvr::paused_occurrence(job)
            || job.id.is_empty()
            || job.start_ts <= now
        {
            return Err("Select upcoming recordings outside the pause period.".to_string());
        }
        let key = sha256_hex(format!("booking:{}", job.id).as_bytes());
        let mut title = crate::reminders::safe_title(&job.title);
        if title.is_empty() {
            title = "Scheduled Recording".to_string();
        }
        let channel = crate::reminders::safe_title(&job.channel_name);
        events.push((key, title, channel, job.start_ts as i64, job.stop_ts as i64));
    }
    if events.is_empty() {
        return Err("Select upcoming recordings outside the pause period.".to_string());
    }
    events.sort_by(|a, b| a.3.cmp(&b.3).then_with(|| a.0.cmp(&b.0)));
    calendar(&events, now as i64)
}

/// Python `pause_preview`: the first programme start a pause
/// would suppress, and the first eligible start after it, as
/// local wall times. Either is `None` when there is none.
#[must_use]
pub fn pause_preview(
    rule: &crate::dvr::DvrJob,
    period: &[String; 2],
    bookings: &[crate::dvr::DvrJob],
    now: f64,
) -> (Option<chrono::NaiveDateTime>, Option<chrono::NaiveDateTime>) {
    let (Ok(first), Ok(last)) = (
        chrono::NaiveDate::parse_from_str(&period[0], "%Y-%m-%d"),
        chrono::NaiveDate::parse_from_str(&period[1], "%Y-%m-%d"),
    ) else {
        return (None, None);
    };
    let start_of = |job: &crate::dvr::DvrJob| {
        crate::dvr::programme_local_start(job.start_ts, job.pre_padding_minutes)
    };
    if rule.repeat == "series" {
        let mut starts: Vec<chrono::NaiveDateTime> = bookings
            .iter()
            .filter(|job| {
                (job.id == rule.id || job.series_source.as_deref() == Some(rule.id.as_str()))
                    && job.status == crate::dvr::STATUS_SCHEDULED
                    && job.start_ts > now
            })
            .filter_map(start_of)
            .collect();
        starts.sort();
        let skipped = starts
            .iter()
            .find(|t| first <= t.date() && t.date() <= last)
            .copied();
        let following = starts.iter().find(|t| t.date() > last).copied();
        return (skipped, following);
    }
    let occurrence = |day: chrono::NaiveDate| -> Option<chrono::NaiveDateTime> {
        if rule.status == crate::dvr::STATUS_SCHEDULED && rule.start_ts > now {
            if let Some(start) = start_of(rule) {
                if start.date() >= day {
                    return Some(start);
                }
            }
        }
        crate::dvr::next_time_occurrence_from(rule, now, &chrono::Local, true, Some(day))
            .ok()
            .and_then(|next| {
                crate::dvr::programme_local_start(next.start_ts, next.pre_padding_minutes)
            })
    };
    let skipped = occurrence(first).filter(|t| t.date() <= last);
    let following = if last < chrono::NaiveDate::MAX {
        occurrence(last + chrono::TimeDelta::days(1))
    } else {
        None
    };
    (skipped, following)
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
    fn schedule_filter_views_and_words() {
        // Python test_schedule_filters_exclude_paused_items_from_the_week.
        let now = 1_800_000_000.0;
        let today = chrono::DateTime::from_timestamp(now as i64, 0)
            .map(|utc| utc.with_timezone(&chrono::Local))
            .unwrap_or_else(chrono::Local::now)
            .format("%Y-%m-%d")
            .to_string();
        let mut active = booking("active", now + 60.0, now + 3600.0);
        active.title = "Hírek".to_string();
        active.channel_name = "Rádió".to_string();
        let mut paused = booking("paused", now + 60.0, now + 3600.0);
        paused.title = "Zene".to_string();
        paused.pause_dates = Some([today.clone(), today]);
        let mut partial = booking("partial", now - 3600.0, now - 60.0);
        partial.status = crate::dvr::STATUS_PARTIAL.to_string();
        partial.title = "Zene".to_string();
        let rows = vec![active, paused, partial];
        let ids = |view: &str, query: &str| {
            filter_recording_schedule(&rows, query, view, now)
                .iter()
                .map(|job| job.id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids("week", "HÍREK rádió"), ["active"]);
        assert_eq!(ids("week", ""), ["active"]);
        assert_eq!(ids("problems", ""), ["partial"]);
        assert_eq!(ids("paused", ""), ["paused"]);
        assert_eq!(ids("all", "zene"), ["paused", "partial"]);
    }

    #[test]
    fn schedule_filter_paused_view_keeps_a_completed_series_rule() {
        // Python test_pause_filter_includes_existing_series_rule_after_its_first_recording.
        let now = 1_800_000_000.0;
        let local = chrono::DateTime::from_timestamp(now as i64, 0)
            .map(|utc| utc.with_timezone(&chrono::Local))
            .unwrap_or_else(chrono::Local::now);
        let today = local.format("%Y-%m-%d").to_string();
        let later = (local + chrono::Duration::days(2))
            .format("%Y-%m-%d")
            .to_string();
        let mut series = booking("series", now - 3600.0, now - 60.0);
        series.status = crate::dvr::STATUS_COMPLETED.to_string();
        series.repeat = "series".to_string();
        series.pause_dates = Some([today, later]);
        let mut canceled = series.clone();
        canceled.id = "canceled".to_string();
        canceled.status = crate::dvr::STATUS_CANCELED.to_string();
        let mut ordinary = series.clone();
        ordinary.id = "ordinary".to_string();
        ordinary.repeat = "weekly".to_string();
        let mut expired = series.clone();
        expired.id = "expired".to_string();
        expired.pause_dates = Some(["2000-01-01".to_string(), "2000-01-02".to_string()]);
        let rows = vec![series, canceled, ordinary, expired];
        let kept: Vec<String> = filter_recording_schedule(&rows, "", "paused", now)
            .iter()
            .map(|job| job.id.clone())
            .collect();
        assert_eq!(kept, ["series"]);
    }

    fn booking(id: &str, start: f64, stop: f64) -> crate::dvr::DvrJob {
        let mut job = crate::dvr::DvrJob {
            id: id.to_string(),
            status: crate::dvr::STATUS_SCHEDULED.to_string(),
            title: "Show".to_string(),
            channel_name: "News".to_string(),
            start_ts: start,
            stop_ts: stop,
            ..crate::dvr::DvrJob::default()
        };
        job.program = serde_json::json!({"title": "Show"});
        job
    }

    #[test]
    fn recording_calendar_groups_bookings_and_keeps_uids() {
        let jobs = vec![
            booking("a", 1_850_000_000.0, 1_850_003_600.0),
            booking("b", 1_850_007_200.0, 1_850_010_800.0),
        ];
        let text = String::from_utf8(recording_calendar(&jobs, 1_849_000_000.0).expect("ics"))
            .expect("utf8");
        assert_eq!(text.matches("BEGIN:VEVENT").count(), 2);
        assert_eq!(text.matches("BEGIN:VCALENDAR").count(), 1);
        let uids = |text: &str| {
            text.replace("\r\n ", "")
                .lines()
                .filter(|line| line.starts_with("UID:"))
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        let edited: Vec<_> = jobs
            .iter()
            .map(|job| {
                let mut job = job.clone();
                job.start_ts += 30.0;
                job
            })
            .collect();
        let edited_text =
            String::from_utf8(recording_calendar(&edited, 1_849_000_000.0).expect("ics"))
                .expect("utf8");
        assert_eq!(uids(&text), uids(&edited_text));
        let mut done = jobs.clone();
        done[0].status = crate::dvr::STATUS_FAILED.to_string();
        assert!(recording_calendar(&done, 1_849_000_000.0).is_err());
        assert!(recording_calendar(&[], 1_849_000_000.0).is_err());
    }

    #[test]
    fn pause_preview_reports_skipped_and_following_starts() {
        // A daily rule starting at a fixed local wall time: build
        // it through the DVR builder so the times are real.
        let channel = crate::Channel {
            name: "News".to_string(),
            group: String::new(),
            url: "http://example.com/s".to_string(),
            tvg_id: String::new(),
            tvg_name: String::new(),
            tvg_logo: String::new(),
            unknown_attributes: std::collections::BTreeMap::new(),
        };
        let spec = crate::dvr::NewJob {
            channel: &channel,
            programme_title: "Daily",
            programme_start: 1_850_000_000,
            programme_stop: 1_850_003_600,
            format: "provider_mkv",
            pre_padding_minutes: 0,
            post_padding_minutes: 0,
            repeat: "daily",
            episode: "",
            subtitle: "",
            description: "",
        };
        let rule = crate::dvr::build_job(&spec, 0.0).expect("rule");
        let date = crate::dvr::programme_local_date(&rule)
            .expect("date")
            .format("%Y-%m-%d")
            .to_string();
        let period = [date.clone(), date];
        let (skipped, following) = pause_preview(&rule, &period, &[], 1_849_000_000.0);
        let skipped = skipped.expect("skipped");
        assert_eq!(skipped.date().format("%Y-%m-%d").to_string(), period[0]);
        let following = following.expect("following");
        assert!(following.date().format("%Y-%m-%d").to_string() > period[1]);
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
