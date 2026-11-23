//! On-demand recording edits, ported from Python `recording_edits.py`:
//! per-recording padding, moving a booking to another airing, and
//! retiming a booking the guide moved. Each returns the changed jobs;
//! the caller publishes them with `DvrSchedule::apply_reviewed_edit`,
//! one transaction against the snapshot the edit was computed from.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::dvr::{self, DvrJob, STATUS_SCHEDULED, STATUS_SKIPPED, TIME_REPEATS};
use crate::epg::Programme;
use crate::Channel;

/// The changed jobs of one edit, by id.
pub type Changes = BTreeMap<String, DvrJob>;

const CHANGED: &str = "The recording schedule changed. Check the times and try again.";
const GUIDE_CHANGED: &str = "The programme guide changed. Refresh the list and try again.";
const DUPLICATE: &str = "This recording is already scheduled.";

/// Python `padding`: (before, after) minutes.
#[must_use]
pub fn padding(job: &DvrJob) -> (i64, i64) {
    (job.pre_padding_minutes, job.post_padding_minutes)
}

fn programme_time(job: &DvrJob, key: &str) -> Result<i64, String> {
    job.program
        .get(key)
        .and_then(Value::as_str)
        .and_then(crate::epg::parse_xmltv_time)
        .ok_or_else(|| "The recording times are invalid. Check the recording schedule.".to_string())
}

#[allow(clippy::cast_precision_loss)] // whole seconds
fn secs(value: i64) -> f64 {
    value as f64
}

/// Python `with_padding`: the job with its record window rebuilt
/// from the programme's own times and the given padding.
///
/// # Errors
/// Returns Python's message for padding outside 0 to 180 minutes or
/// an unreadable programme time.
pub fn with_padding(job: &DvrJob, before: i64, after: i64) -> Result<DvrJob, String> {
    if !(0..=180).contains(&before) || !(0..=180).contains(&after) {
        return Err("The recording times are invalid. Check the recording schedule.".to_string());
    }
    let (start, end) = (programme_time(job, "start")?, programme_time(job, "end")?);
    let mut job = job.clone();
    job.start_ts = secs(start - before * 60);
    job.stop_ts = secs(end + after * 60);
    job.pre_padding_minutes = before;
    job.post_padding_minutes = after;
    Ok(job)
}

fn program_start(job: &DvrJob) -> String {
    job.program
        .get("start")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// Python `edit_padding`: new padding for one pending recording. With
/// `future`, later occurrences change too: a series rule records the
/// change from this airing on and its pending episodes from here are
/// re-padded; a time repeat's successors take it. Without `future`,
/// a recurring job keeps its old padding for later occurrences
/// (`future_padding`).
///
/// # Errors
/// Returns Python's messages for a job no longer pending, a rule that
/// changed, invalid padding, and a window that has already ended.
#[allow(clippy::implicit_hasher)]
pub fn edit_padding(
    job: &DvrJob,
    before: i64,
    after: i64,
    future: bool,
    jobs: &Changes,
    now: f64,
) -> Result<Changes, String> {
    if job.status != STATUS_SCHEDULED {
        return Err(CHANGED.to_string());
    }
    let rule_id = if job.repeat == "series" {
        Some(job.id.clone())
    } else {
        job.series_source.clone()
    };
    let effective = program_start(job);
    let mut changed = Changes::new();
    if let (true, Some(rule_id)) = (future, rule_id.as_ref()) {
        let rule = jobs
            .get(rule_id)
            .filter(|r| r.repeat == "series" && r.status != dvr::STATUS_CANCELED)
            .ok_or_else(|| CHANGED.to_string())?;
        let mut retained: Vec<(String, i64, i64)> = rule
            .series_padding_changes
            .clone()
            .unwrap_or_default()
            .into_iter()
            .filter(|change| change.0 < effective)
            .collect();
        if *rule_id != job.id {
            retained.push((effective.clone(), before, after));
        }
        let mut updated = rule.clone();
        updated.series_padding_changes = (!retained.is_empty()).then_some(retained);
        changed.insert(rule_id.clone(), updated);
    }
    for old in jobs.values() {
        let later_episode = future
            && rule_id.is_some()
            && old.series_source == rule_id
            && old.status == STATUS_SCHEDULED
            && program_start(old) >= effective;
        if old.id != job.id && !later_episode {
            continue;
        }
        let mut new = with_padding(changed.get(&old.id).unwrap_or(old), before, after)?;
        if new.stop_ts <= now {
            return Err("This programme has already ended.".to_string());
        }
        let recurring = TIME_REPEATS.contains(&old.repeat.as_str()) || old.repeat == "series";
        new.future_padding = if recurring && !future {
            Some(
                old.future_padding
                    .unwrap_or([old.pre_padding_minutes, old.post_padding_minutes]),
            )
        } else {
            None
        };
        changed.insert(new.id.clone(), new);
    }
    Ok(changed)
}

/// A guide programme as a job's programme snapshot (Python merges
/// the guide row's dict into `program`).
fn programme_value(programme: &Programme) -> Value {
    serde_json::json!({
        "channel_id": programme.channel_id,
        "title": programme.title,
        "start": dvr::xmltv_from_ts(programme.start),
        "end": dvr::xmltv_from_ts(programme.stop),
        "description": programme.description,
        "episode": programme.episode,
        "subtitle": programme.subtitle,
    })
}

fn same_slot_booked(jobs: &Changes, except: &str, start_at: &str, channel: &Channel) -> bool {
    let key = crate::reminders::reminder_channel_key(channel);
    jobs.values().any(|row| {
        row.id != except
            && row.start_at == start_at
            && matches!(row.status.as_str(), "scheduled" | "recording" | "stopping")
            && crate::reminders::reminder_channel_key(&dvr::channel_from_snapshot(&row.channel))
                == key
    })
}

/// Python `move_airing`: skip this occurrence and book `programme`
/// on `channel` instead, keeping the padding. A time repeat queues
/// its next occurrence; a series episode stays in its series.
///
/// # Errors
/// Returns Python's messages for a job no longer pending, an airing
/// already started, a slot already booked, and build errors.
#[allow(clippy::implicit_hasher)]
pub fn move_airing(
    job: &DvrJob,
    channel: &Channel,
    programme: &Programme,
    format: &str,
    jobs: &Changes,
    now: f64,
) -> Result<Changes, String> {
    if job.status != STATUS_SCHEDULED {
        return Err(CHANGED.to_string());
    }
    let (before, after) = padding(job);
    let spec = dvr::NewJob {
        channel,
        programme_title: &programme.title,
        programme_start: programme.start,
        programme_stop: programme.stop,
        format,
        pre_padding_minutes: before,
        post_padding_minutes: after,
        repeat: "",
        episode: &programme.episode,
        subtitle: &programme.subtitle,
        description: &programme.description,
    };
    let mut new = dvr::build_job(&spec, now)?;
    new.program = programme_value(programme);
    if secs(programme.start) <= now {
        return Err(GUIDE_CHANGED.to_string());
    }
    if same_slot_booked(jobs, "", &new.start_at, channel) {
        return Err(DUPLICATE.to_string());
    }
    let mut changed = Changes::new();
    let mut skipped = job.clone();
    skipped.status = STATUS_SKIPPED.to_string();
    skipped.message = String::new();
    changed.insert(skipped.id.clone(), skipped);
    if TIME_REPEATS.contains(&job.repeat.as_str()) {
        let successor = dvr::next_time_occurrence(job, now, &chrono::Local)?;
        if !jobs.contains_key(&successor.id) {
            changed.insert(successor.id.clone(), successor);
        }
    }
    new.series_source = if job.repeat == "series" {
        Some(job.id.clone())
    } else {
        job.series_source.clone()
    };
    changed.insert(new.id.clone(), new);
    Ok(changed)
}

/// Python `retime_booking`: move a booking to the times the guide
/// now gives its programme, keeping every rule and padding field.
///
/// # Errors
/// Returns Python's messages for a booking that changed, a new time
/// already past or inverted, and a slot already booked.
#[allow(clippy::implicit_hasher)]
pub fn retime_booking(
    job: &DvrJob,
    programme: &Programme,
    jobs: &Changes,
    now: f64,
) -> Result<Changes, String> {
    if jobs.get(&job.id) != Some(job) || job.status != STATUS_SCHEDULED {
        return Err(CHANGED.to_string());
    }
    let (before, after) = padding(job);
    let mut moved = job.clone();
    if let (Some(program), Value::Object(fresh)) =
        (moved.program.as_object_mut(), programme_value(programme))
    {
        program.extend(fresh);
    } else {
        moved.program = programme_value(programme);
    }
    let mut new = with_padding(&moved, before, after)?;
    if new.start_ts <= now || programme.stop <= programme.start {
        return Err(GUIDE_CHANGED.to_string());
    }
    new.start_at = dvr::iso_from_ts(secs(programme.start));
    new.end_at = dvr::iso_from_ts(secs(programme.stop));
    let channel = dvr::channel_from_snapshot(&job.channel);
    if same_slot_booked(jobs, &job.id, &new.start_at, &channel) {
        return Err(DUPLICATE.to_string());
    }
    Ok(Changes::from([(new.id.clone(), new)]))
}

/// Python `warnings`: the connection-limit or overlap warnings the
/// edit's new or moved bookings raise, each once.
#[must_use]
#[allow(clippy::implicit_hasher, clippy::float_cmp)] // the moved check compares stored times exactly
pub fn warnings(config: &Value, changes: &Changes, jobs: &Changes) -> Vec<String> {
    let mut merged = jobs.clone();
    merged.extend(changes.iter().map(|(id, job)| (id.clone(), job.clone())));
    let active: Vec<crate::recording_limits::JobView> = merged
        .values()
        .filter(|j| matches!(j.status.as_str(), "scheduled" | "recording" | "stopping"))
        .map(crate::recording_limits::JobView::from_job)
        .collect();
    let mut messages = Vec::new();
    for job in changes.values() {
        let moved = jobs
            .get(&job.id)
            .is_none_or(|old| (old.start_ts, old.stop_ts) != (job.start_ts, job.stop_ts));
        if job.status != STATUS_SCHEDULED || !moved {
            continue;
        }
        let warning = crate::recording_limits::warning(
            config,
            &dvr::channel_from_snapshot(&job.channel),
            job.start_ts,
            job.stop_ts,
            &active,
            Some(&job.id),
        );
        if !warning.is_empty() && !messages.contains(&warning) {
            messages.push(warning);
        }
    }
    messages
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dvr::{DvrSchedule, STATUS_CANCELED};

    const NOW: f64 = 1_795_000_000.0;

    fn channel(key: &str, source: &str) -> Channel {
        let mut unknown = std::collections::BTreeMap::new();
        unknown.insert("playlist-id".to_string(), source.to_string());
        Channel {
            name: "Concert".to_string(),
            url: format!("https://{source}.invalid/{key}"),
            tvg_id: key.to_string(),
            unknown_attributes: unknown,
            ..Channel::default()
        }
    }

    #[allow(clippy::cast_possible_truncation)] // whole-second test times
    fn show(start: f64, episode: &str) -> Programme {
        Programme {
            channel_id: String::new(),
            start: start as i64,
            stop: start as i64 + 1800,
            title: "Concert".to_string(),
            episode: episode.to_string(),
            ..Programme::default()
        }
    }

    fn job(channel: &Channel, programme: &Programme, repeat: &str, pre: i64, post: i64) -> DvrJob {
        let spec = dvr::NewJob {
            channel,
            programme_title: &programme.title,
            programme_start: programme.start,
            programme_stop: programme.stop,
            format: "provider_mkv",
            pre_padding_minutes: pre,
            post_padding_minutes: post,
            repeat: "",
            episode: &programme.episode,
            subtitle: "",
            description: "",
        };
        let mut job = dvr::build_job(&spec, NOW).unwrap();
        job.repeat = repeat.to_string();
        job
    }

    fn schedule(tag: &str) -> (DvrSchedule, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "iptv-edits-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        (
            DvrSchedule::load(dir.join("scheduled_recordings.json"), NOW),
            dir,
        )
    }

    fn snapshot(schedule: &DvrSchedule) -> Changes {
        schedule
            .edit_snapshot()
            .into_iter()
            .map(|j| (j.id.clone(), j))
            .collect()
    }

    /// Python `test_padding_preserves_guide_archive_times_and_scopes`.
    #[test]
    #[allow(clippy::float_cmp)] // whole-second schedule values are exact in f64
    fn padding_preserves_guide_times_and_scopes() {
        for repeat in ["", "daily", "weekly", "weekdays", "series"] {
            for future in [false, true] {
                let (mut s, dir) = schedule("padding");
                let one = channel("one", "first");
                let mut first = job(&one, &show(NOW + 86_400.0, "1x01"), repeat, 2, 3);
                first.weekdays = Some(vec![0, 2, 4]);
                s.add_job(first.clone()).unwrap();
                let mut child = None;
                if repeat == "series" {
                    let mut c = job(&one, &show(NOW + 2.0 * 86_400.0, "1x2"), "", 2, 3);
                    c.series_source = Some(first.id.clone());
                    s.add_job(c.clone()).unwrap();
                    child = Some(c);
                }
                let before = s.edit_snapshot();
                let new = edit_padding(&first, 5, 20, future, &snapshot(&s), NOW).unwrap();
                s.apply_reviewed_edit(&before, new.clone(), None, NOW)
                    .unwrap();
                let stored = s.get(&first.id).unwrap().clone();
                assert_eq!(stored.program, first.program);
                assert_eq!(
                    (&stored.start_at, &stored.end_at),
                    (&first.start_at, &first.end_at)
                );
                assert_eq!(
                    (stored.start_ts, stored.stop_ts),
                    (first.start_ts - 180.0, first.stop_ts + 1020.0),
                    "{repeat} {future}"
                );
                if TIME_REPEATS.contains(&repeat) {
                    let next = dvr::next_time_occurrence(&stored, NOW, &chrono::Utc).unwrap();
                    assert_eq!(padding(&next), if future { (5, 20) } else { (2, 3) });
                }
                if let Some(child) = child {
                    let stored = s.get(&child.id).unwrap();
                    assert_eq!(padding(stored), if future { (5, 20) } else { (2, 3) });
                }
                assert!(s.apply_reviewed_edit(&before, new, None, NOW).is_err());
                let _ = std::fs::remove_dir_all(dir);
            }
        }
    }

    /// Python `test_moving_one_series_airing_keeps_rule_and_cancels_replacement_with_parent`.
    #[test]
    fn moving_a_series_airing_keeps_the_rule() {
        let (mut s, dir) = schedule("move");
        let parent = job(
            &channel("A", "first"),
            &show(NOW + 3600.0, "1x01"),
            "series",
            0,
            2,
        );
        s.add_job(parent.clone()).unwrap();
        let before = s.edit_snapshot();
        let update = move_airing(
            &parent,
            &channel("B", "first"),
            &show(NOW + 7200.0, "1x01"),
            "provider_mkv",
            &snapshot(&s),
            NOW,
        )
        .unwrap();
        s.apply_reviewed_edit(&before, update, None, NOW).unwrap();
        let child = s
            .jobs()
            .iter()
            .find(|j| j.status == STATUS_SCHEDULED)
            .unwrap()
            .clone();
        assert_eq!(child.series_source.as_deref(), Some(parent.id.as_str()));
        assert_eq!(s.get(&parent.id).unwrap().repeat, "series");
        assert_eq!(s.get(&parent.id).unwrap().status, STATUS_SKIPPED);
        s.cancel_job(&parent.id).unwrap();
        assert_eq!(s.get(&child.id).unwrap().status, STATUS_CANCELED);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Python `test_edit_from_a_series_child_keeps_earlier_airings_and_default_for_backfilled_guide`.
    #[test]
    fn editing_from_a_series_child_keeps_earlier_airings() {
        let (mut s, dir) = schedule("child");
        let a = channel("A", "first");
        let first = job(&a, &show(NOW + 3600.0, "1x01"), "series", 0, 2);
        s.add_job(first.clone()).unwrap();
        let mut episodes = Vec::new();
        for day in [1.0, 2.0, 3.0] {
            let mut episode = job(&a, &show(NOW + day * 86_400.0, ""), "", 0, 2);
            episode.series_source = Some(first.id.clone());
            s.add_job(episode.clone()).unwrap();
            episodes.push(episode);
        }
        let before = s.edit_snapshot();
        let changes = edit_padding(&episodes[1], 4, 21, true, &snapshot(&s), NOW).unwrap();
        s.apply_reviewed_edit(&before, changes, None, NOW).unwrap();
        let stored = s.get(&first.id).unwrap().clone();
        assert_eq!(padding(&stored), padding(&first));
        assert_eq!(
            dvr::series_padding_at(&stored, &program_start(&episodes[0])),
            padding(&first)
        );
        assert_eq!(
            dvr::series_padding_at(&stored, &program_start(&episodes[2])),
            (4, 21)
        );
        let paddings: Vec<(i64, i64)> = episodes
            .iter()
            .map(|e| padding(s.get(&e.id).unwrap()))
            .collect();
        assert_eq!(paddings, [padding(&first), (4, 21), (4, 21)]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    #[allow(clippy::float_cmp)] // whole-second schedule values are exact in f64
    fn retime_moves_a_booking_and_keeps_its_padding() {
        let (mut s, dir) = schedule("retime");
        let a = channel("A", "first");
        let booking = job(&a, &show(NOW + 3600.0, "1x01"), "", 2, 3);
        s.add_job(booking.clone()).unwrap();
        let moved = show(NOW + 5400.0, "1x01");
        let changes = retime_booking(&booking, &moved, &snapshot(&s), NOW).unwrap();
        let new = &changes[&booking.id];
        assert_eq!(new.start_ts, secs(moved.start) - 120.0);
        assert_eq!(new.stop_ts, secs(moved.stop) + 180.0);
        assert_eq!(new.start_at, dvr::iso_from_ts(secs(moved.start)));
        assert_eq!(
            retime_booking(&booking, &show(NOW - 60.0, ""), &snapshot(&s), NOW),
            Err(GUIDE_CHANGED.to_string())
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
