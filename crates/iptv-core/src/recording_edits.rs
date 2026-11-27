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

pub use crate::epg::xmltv_id;

/// Python `programme_reminders.media_identity` (the canonical
/// portal-aware identity in `reminders`).
fn media_identity(channel: &Channel) -> crate::reminders::MediaIdentity {
    crate::reminders::media_identity(channel)
}

fn fold(text: &str) -> String {
    text.trim().to_lowercase()
}

/// A guide channel for a playlist channel: its id, and whether it
/// comes from the user's own guide assignment (C10) rather than
/// automatic matching.
pub type GuideFor<'a> = dyn Fn(&Channel) -> Option<(String, bool)> + 'a;

/// One booking the guide moved: the job, the guide's programme for
/// it now, and whether the episode identity matches exactly.
pub type GuideChange = (DvrJob, Programme, bool);

/// Python `changed_guide_times`: for each pending guide booking (not
/// manual, not a time repeat, not started), the programmes on its own
/// guide channel within six hours of its original start that have the
/// same title, a compatible episode, and new times; an unchanged
/// broadcast clears the booking's candidates.
///
/// # Errors
/// Returns the store's error text.
pub fn changed_guide_times(
    store: &crate::epgstore::EpgStore,
    jobs: &[DvrJob],
    now: f64,
    guide_for: &GuideFor<'_>,
    canceled: &dyn Fn() -> bool,
) -> Result<Vec<GuideChange>, String> {
    let mut results = Vec::new();
    let mut channels: BTreeMap<String, Option<(String, bool)>> = BTreeMap::new();
    for job in jobs {
        if canceled() {
            break;
        }
        if job.status != STATUS_SCHEDULED
            || job.manual == Some(true)
            || TIME_REPEATS.contains(&job.repeat.as_str())
            || job.start_ts <= now
        {
            continue;
        }
        let (Ok(start), Ok(end)) = (programme_time(job, "start"), programme_time(job, "end"))
        else {
            continue;
        };
        let channel = dvr::channel_from_snapshot(&job.channel);
        let key = crate::reminders::reminder_channel_key(&channel);
        let Some((cid, assigned)) = channels
            .entry(key)
            .or_insert_with(|| guide_for(&channel))
            .clone()
        else {
            continue;
        };
        let booked_id = job
            .program
            .get("channel_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        if !booked_id.is_empty() && booked_id != cid {
            continue;
        }
        if let Some(fingerprint) = job
            .extra
            .get("guide_fingerprint")
            .and_then(Value::as_str)
            .filter(|fp| !fp.is_empty())
        {
            if crate::reminders::identity_digest(&cid) != fingerprint {
                continue;
            }
        }
        let declared = channel.tvg_id.trim();
        if !declared.is_empty()
            && !assigned
            && booked_id.is_empty()
            && xmltv_id(&cid).to_lowercase() != xmltv_id(declared).to_lowercase()
        {
            continue; // A similarly named replacement station is not the booked source.
        }
        let title = fold(
            job.program
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or(&job.title),
        );
        let identity = job
            .extra
            .get("episode_fingerprint")
            .and_then(Value::as_str)
            .filter(|fp| !fp.is_empty())
            .map_or_else(
                || {
                    crate::reminders::identity_digest(
                        dvr::program_episode_key(&job.program)
                            .as_deref()
                            .unwrap_or(""),
                    )
                },
                str::to_string,
            );
        let near = 6 * 3600;
        let mut candidates = Vec::new();
        for row in store.programmes_starting(&cid, start - near, start + near)? {
            if fold(&row.title) != title {
                continue;
            }
            let matched = crate::reminders::identity_digest(
                dvr::program_episode_key(&programme_value(&row))
                    .as_deref()
                    .unwrap_or(""),
            );
            if !identity.is_empty() && !matched.is_empty() && identity != matched {
                continue;
            }
            if (row.start, row.stop) == (start, end) {
                candidates.clear();
                break; // unchanged broadcast, even when a later repeat also matches
            }
            let exact = !identity.is_empty() && matched == identity;
            if row.start >= row.stop || secs(row.start - job.pre_padding_minutes * 60) <= now {
                continue;
            }
            if exact || (row.start < end && row.stop > start) {
                candidates.push((job.clone(), row, exact));
            }
        }
        results.extend(candidates);
    }
    Ok(results)
}

/// A playlist channel under its guide channel, keyed as Python's
/// `(channel_key, media_identity)`.
type GuideEntry = (String, crate::reminders::MediaIdentity, Channel);

/// One other airing: the playlist channel, the programme, and whether
/// the episode number matches exactly.
pub type Airing = (Channel, Programme, bool);

/// Python `other_airings`: the same programme over the next 14 days
/// on any playlist channel the guide covers, numbered episode matches
/// first, then title-only possible repeats; the booking's own slot on
/// its own source is left out. Returns up to `maximum` and whether
/// more were found.
///
/// # Errors
/// Returns the store's error text.
#[allow(clippy::too_many_lines)] // one walk, as in Python
pub fn other_airings(
    store: &crate::epgstore::EpgStore,
    job: &DvrJob,
    channels: &[Channel],
    now: f64,
    guide_for: &GuideFor<'_>,
    canceled: &dyn Fn() -> bool,
    maximum: usize,
) -> Result<(Vec<Airing>, bool), String> {
    #[allow(clippy::cast_possible_truncation)] // whole seconds
    let from = now as i64;
    let to = from + 14 * 86_400;
    let original_channel = dvr::channel_from_snapshot(&job.channel);
    let mut original = job.program.clone();
    if let (Some((source, _)), Ok(start), Ok(end)) = (
        guide_for(&original_channel),
        programme_time(job, "start"),
        programme_time(job, "end"),
    ) {
        let title = original
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or(&job.title)
            .to_string();
        if let Some(info) = store.programme_details(&source, start, end, &title)? {
            for (key, value) in [
                ("subtitle", info.subtitle),
                ("episode", info.episode),
                ("description", info.description),
            ] {
                let missing = original
                    .get(key)
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .is_empty();
                if missing && !value.is_empty() && original.is_object() {
                    original[key] = value.into();
                }
            }
        }
    }
    let identity = dvr::program_episode_key(&original);
    let title = fold(
        original
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or(&job.title),
    );
    let original_start = original
        .get("start")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let rows = store.programmes_titled(&title, from, to)?;
    if rows.is_empty() {
        return Ok((Vec::new(), false));
    }
    let mut by_guide: BTreeMap<String, Vec<GuideEntry>> = BTreeMap::new();
    for channel in channels {
        if canceled() {
            return Ok((Vec::new(), false));
        }
        let kind = channel
            .unknown_attributes
            .get("kind")
            .map_or("", String::as_str);
        if !crate::reminders::playable(channel) || matches!(kind, "series" | "movie") {
            continue;
        }
        if let Some((gid, _)) = guide_for(channel) {
            let key = (
                crate::reminders::reminder_channel_key(channel),
                media_identity(channel),
            );
            let entries = by_guide.entry(gid).or_default();
            if !entries.iter().any(|(k, m, _)| (k, m) == (&key.0, &key.1)) {
                entries.push((key.0, key.1, channel.clone()));
            }
        }
    }
    let source_of = |c: &Channel| {
        c.unknown_attributes
            .get("provider-id")
            .or_else(|| c.unknown_attributes.get("playlist-id"))
            .cloned()
            .unwrap_or_default()
    };
    let original_source = source_of(&original_channel);
    let numbered = identity.as_deref().is_some_and(|i| i.starts_with("ep:"));
    let (mut confirmed, mut possible): (Vec<Airing>, Vec<Airing>) = (Vec::new(), Vec::new());
    let mut total = 0_usize;
    let finish = |confirmed: Vec<Airing>, possible: Vec<Airing>, more: bool| {
        let mut all = confirmed;
        all.extend(possible);
        all.truncate(maximum);
        (all, more)
    };
    for row in rows {
        if canceled() {
            return Ok((Vec::new(), false));
        }
        if fold(&row.title) != title || row.start >= row.stop {
            continue;
        }
        let Some(entries) = by_guide.get(&row.channel_id) else {
            continue;
        };
        let candidate = dvr::program_episode_key(&programme_value(&row));
        if let (true, Some(candidate), Some(identity)) = (numbered, &candidate, &identity) {
            if candidate.starts_with("ep:") && identity != candidate {
                let (old_episode, old_part) =
                    identity.split_once(":part").unwrap_or((identity, ""));
                let (new_episode, new_part) =
                    candidate.split_once(":part").unwrap_or((candidate, ""));
                if old_episode != new_episode || (!old_part.is_empty() && !new_part.is_empty()) {
                    continue;
                }
            }
        }
        let exact = numbered && identity == candidate;
        let row_start = dvr::xmltv_from_ts(row.start);
        for (_, _, channel) in entries {
            if row_start == original_start {
                let other_source = source_of(channel);
                if original_source.is_empty()
                    || other_source.is_empty()
                    || original_source == other_source
                {
                    continue;
                }
                if crate::reminders::playable(&original_channel)
                    && media_identity(channel) == media_identity(&original_channel)
                {
                    continue;
                }
            }
            total += 1;
            let group = if exact { &mut confirmed } else { &mut possible };
            if group.len() < maximum {
                group.push((channel.clone(), row.clone(), exact));
            }
            if total > maximum && (confirmed.len() == maximum || !numbered) {
                return Ok(finish(confirmed, possible, true));
            }
        }
    }
    Ok(finish(confirmed, possible, total > maximum))
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

    fn guide_store(rows: &[(&str, f64, &str)]) -> crate::epgstore::EpgStore {
        let mut store = crate::epgstore::EpgStore::open(std::path::Path::new(":memory:")).unwrap();
        let mut programmes: std::collections::HashMap<String, Vec<Programme>> =
            std::collections::HashMap::new();
        for (channel, start, episode) in rows {
            let mut row = show(*start, episode);
            row.channel_id = (*channel).to_string();
            programmes
                .entry((*channel).to_string())
                .or_default()
                .push(row);
        }
        let channels = programmes
            .keys()
            .map(|id| crate::epg::EpgChannel {
                id: id.clone(),
                display_name: "Concert".to_string(),
            })
            .collect();
        store
            .import_source(
                "k",
                &crate::epg::EpgGuide {
                    channels,
                    programmes,
                },
                1_000,
            )
            .unwrap();
        store
    }

    fn by_tvg_id(channel: &Channel) -> Option<(String, bool)> {
        (!channel.tvg_id.is_empty()).then(|| (channel.tvg_id.clone(), false))
    }

    /// Python `test_find_confirm_and_move_one_airing_atomically`: the
    /// booking's own slot and a different episode are left out, the
    /// numbered match comes first, then the unnumbered airing; an
    /// airing beyond 14 days is not offered.
    #[test]
    fn other_airings_rank_numbered_matches_first() {
        let store = guide_store(&[
            ("A", NOW + 3600.0, "1x01"),
            ("A", NOW + 7200.0, "1x02"),
            ("B", NOW + 3.0 * 3600.0, "S01E01"),
            ("B", NOW + 4.0 * 3600.0, ""),
            ("B", NOW + 400.0 * 3600.0, "1x01"),
        ]);
        let (one, two) = (channel("A", "first"), channel("B", "second"));
        let parent = job(&one, &show(NOW + 3600.0, "1x01"), "weekly", 0, 2);
        let (results, more) = other_airings(
            &store,
            &parent,
            &[one, two],
            NOW,
            &by_tvg_id,
            &|| false,
            100,
        )
        .unwrap();
        assert!(!more);
        let got: Vec<(String, bool)> = results
            .iter()
            .map(|(c, _, exact)| (c.tvg_id.clone(), *exact))
            .collect();
        assert_eq!(got, [("B".to_string(), true), ("B".to_string(), false)]);
    }

    #[test]
    fn guide_changes_find_moved_bookings_only() {
        let a = channel("A", "first");
        let booked = job(&a, &show(NOW + 7200.0, "1x01"), "", 0, 2);
        // The episode moved 30 minutes later.
        let store = guide_store(&[("A", NOW + 9000.0, "1x01")]);
        let changes = changed_guide_times(
            &store,
            std::slice::from_ref(&booked),
            NOW,
            &by_tvg_id,
            &|| false,
        )
        .unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].1.start, show(NOW + 9000.0, "").start);
        assert!(changes[0].2);
        // The original slot is still in the guide: no change, even
        // though a later airing matches too.
        let store = guide_store(&[("A", NOW + 7200.0, "1x01"), ("A", NOW + 9000.0, "1x01")]);
        assert!(changed_guide_times(
            &store,
            std::slice::from_ref(&booked),
            NOW,
            &by_tvg_id,
            &|| false
        )
        .unwrap()
        .is_empty());
        // Manual and clock-repeat bookings keep their own times.
        let store = guide_store(&[("A", NOW + 9000.0, "1x01")]);
        let mut manual = booked.clone();
        manual.manual = Some(true);
        let mut daily = booked.clone();
        daily.repeat = "daily".to_string();
        assert!(
            changed_guide_times(&store, &[manual, daily], NOW, &by_tvg_id, &|| false)
                .unwrap()
                .is_empty()
        );
        // Without an episode identity only an overlapping slot counts.
        let untitled = job(&a, &show(NOW + 7200.0, ""), "", 0, 2);
        let store = guide_store(&[("A", NOW + 7800.0, ""), ("A", NOW + 20_000.0, "")]);
        let changes = changed_guide_times(&store, &[untitled], NOW, &by_tvg_id, &|| false).unwrap();
        assert_eq!(changes.len(), 1);
        assert!(!changes[0].2);
        assert_eq!(changes[0].1.start, show(NOW + 7800.0, "").start);
    }

    #[test]
    fn xmltv_id_strips_only_the_app_qualifier() {
        let qualified = format!("epg:{}:bbc.one.uk", "a".repeat(40));
        assert_eq!(xmltv_id(&qualified), "bbc.one.uk");
        assert_eq!(xmltv_id("epg:short:bbc.one.uk"), "epg:short:bbc.one.uk");
        assert_eq!(xmltv_id("bbc.one.uk"), "bbc.one.uk");
    }
}
