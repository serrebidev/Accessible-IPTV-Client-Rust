//! The exit-time work check (second-round audit N07): closing
//! the app can silently lose pending reminders, scheduled
//! recordings that have not started, and a download in flight,
//! so Python asks before exiting in each of those states
//! (`main.py` `on_close`, `planning_ui.py`
//! `_confirm_reminder_exit`). The decision and the message
//! text live here, wx-free; the adapter only gathers the live
//! state and shows the boxes in the returned order. A declined
//! box vetoes the exit, and later boxes are not shown.

use crate::dvr::{DvrJob, STATUS_SCHEDULED, STATUS_WAITING};
use crate::reminders::{Reminder, ReminderStatus};

/// One confirmation to show, in Python's order: reminders,
/// then recordings, then the download.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExitPrompt {
    pub title: &'static str,
    pub message: String,
    /// Python's reminder box defaults to No (`wx.NO_DEFAULT`):
    /// an accidental Enter must not drop the reminders.
    pub no_default: bool,
}

/// Reminders that still need the app alive: scheduled or ready
/// and not yet past their end.
#[must_use]
pub fn pending_reminder_count(reminders: &[Reminder], now: i64) -> usize {
    reminders
        .iter()
        .filter(|r| {
            matches!(r.status, ReminderStatus::Scheduled | ReminderStatus::Ready) && r.end > now
        })
        .count()
}

/// DVR jobs that would never start if the app closed now,
/// earliest first. Python filters to `scheduled`; the Rust-only
/// `waiting` state (a job held by a connection conflict, issue
/// #134) is in the same position — not started, gone on exit —
/// so it is included. PR #46: a paused time-repeat booking is
/// listed by its next eligible occurrence after the pause, not
/// by the occurrence the pause swallows; paused series rules
/// and one-off bookings drop out (their next start is a guide
/// matter, not a clock one).
#[must_use]
pub fn upcoming_recording_jobs(jobs: &[DvrJob], now: f64) -> Vec<DvrJob> {
    let mut upcoming: Vec<DvrJob> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for job in jobs {
        if job.status != STATUS_SCHEDULED && job.status != STATUS_WAITING {
            continue;
        }
        let shown = if crate::dvr::paused_occurrence(job) {
            if !crate::dvr::TIME_REPEATS.contains(&job.repeat.as_str()) {
                continue;
            }
            let Some(dates) = &job.pause_dates else {
                continue;
            };
            let Some(last) = chrono::NaiveDate::parse_from_str(&dates[1], "%Y-%m-%d").ok() else {
                continue;
            };
            if last > chrono::NaiveDate::MAX - chrono::Duration::days(8) {
                continue;
            }
            let earliest = last + chrono::Duration::days(1);
            let Ok(next) = crate::dvr::next_time_occurrence_from(
                job,
                now,
                &chrono::Local,
                true,
                Some(earliest),
            ) else {
                continue;
            };
            if crate::dvr::paused_occurrence(&next) {
                continue;
            }
            next
        } else {
            job.clone()
        };
        // Python keys the list by the source job's id: one row
        // per booking even if the store somehow lists it twice.
        if !seen.contains(&job.id) {
            seen.push(job.id.clone());
            upcoming.push(shown);
        }
    }
    upcoming.sort_by(|a, b| a.start_ts.total_cmp(&b.start_ts));
    upcoming
}

/// Python's `_schedule_window_label`: the job's local window,
/// `YYYY-MM-DD HH:MM - YYYY-MM-DD HH:MM`.
#[must_use]
#[allow(clippy::cast_possible_truncation)] // schedule times are whole seconds
pub fn schedule_window_label(job: &DvrJob) -> String {
    let fmt = |ts: f64| {
        chrono::DateTime::from_timestamp(ts as i64, 0).map_or_else(
            || "Unknown time".to_string(),
            |utc| {
                utc.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M")
                    .to_string()
            },
        )
    };
    format!("{} - {}", fmt(job.start_ts), fmt(job.stop_ts))
}

/// The prompts for one exit attempt, in the order Python asks
/// them. Empty means nothing would be lost: exit silently.
#[must_use]
pub fn exit_prompts(
    reminders: &[Reminder],
    jobs: &[DvrJob],
    recording_active: bool,
    download_active: bool,
    now: i64,
) -> Vec<ExitPrompt> {
    exit_prompts_with_active(reminders, jobs, recording_active, download_active, now, &[])
}

/// [`exit_prompts`] with the titles of the recordings an exit
/// would stop (PR #44): Python asks about them by name, in
/// their own box, between the reminders and the scheduled
/// recordings. Catch-up downloads are not recordings for this
/// box; the caller leaves them out.
#[must_use]
#[allow(clippy::cast_precision_loss)] // epoch seconds
pub fn exit_prompts_with_active(
    reminders: &[Reminder],
    jobs: &[DvrJob],
    recording_active: bool,
    download_active: bool,
    now: i64,
    active_titles: &[String],
) -> Vec<ExitPrompt> {
    let mut prompts = Vec::new();
    let pending = pending_reminder_count(reminders, now);
    if pending > 0 {
        prompts.push(ExitPrompt {
            title: "Programme Reminders",
            message: format!(
                "Pending programme reminders: {pending}.\n\nNotifications require the application to remain running. Exit anyway?"
            ),
            no_default: true,
        });
    }
    if !active_titles.is_empty() {
        prompts.push(ExitPrompt {
            title: "Recordings in progress",
            message: format!(
                "Exiting will stop these recordings:\n{}\n\nExit anyway?",
                active_titles.join("\n")
            ),
            no_default: true,
        });
    }
    let upcoming = upcoming_recording_jobs(jobs, now as f64);
    if !upcoming.is_empty() {
        let names = upcoming
            .iter()
            .map(|job| {
                let title = if !job.display_title.is_empty() {
                    job.display_title.as_str()
                } else if !job.title.is_empty() {
                    job.title.as_str()
                } else {
                    "?"
                };
                format!("{title}\n{}", schedule_window_label(job))
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        let count = upcoming.len();
        let message = if recording_active {
            format!(
                "A recording is in progress and {count} recording is still scheduled.\n\n{names}\n\nThe app must stay open to start them. Exit anyway?"
            )
        } else {
            format!(
                "{count} recording is scheduled but has not started yet.\n\n{names}\n\nThe app must stay open to start it. Exit anyway?"
            )
        };
        prompts.push(ExitPrompt {
            title: "Scheduled recordings",
            message,
            no_default: false,
        });
    }
    if download_active {
        prompts.push(ExitPrompt {
            title: "Download in progress",
            message: "A download is still in progress.\n\nIf you exit now it will stop, and only the part captured so far is kept.\n\nExit anyway?"
                .to_string(),
            no_default: false,
        });
    }
    prompts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reminders::ReminderStatus;

    #[test]
    fn active_recordings_get_their_own_prompt_between_reminders_and_jobs() {
        let titles = vec!["News at Six".to_string(), "Film".to_string()];
        let prompts = exit_prompts_with_active(&[], &[], true, false, 100, &titles);
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0].title, "Recordings in progress");
        assert_eq!(
            prompts[0].message,
            "Exiting will stop these recordings:\nNews at Six\nFilm\n\nExit anyway?"
        );
        assert!(prompts[0].no_default);
        // No active recordings: no box (the plain entry point agrees).
        assert!(exit_prompts(&[], &[], true, false, 100).is_empty());
    }

    fn reminder(status: ReminderStatus, end: i64) -> Reminder {
        Reminder {
            id: "r".to_string(),
            channel_key: "k".to_string(),
            title: "Show".to_string(),
            channel: "Chan".to_string(),
            start: end - 3600,
            end,
            lead: 300,
            status,
            notify_at: None,
            guide_fingerprint: None,
            episode_fingerprint: None,
        }
    }

    fn job(status: &str, title: &str, start_ts: f64) -> DvrJob {
        DvrJob {
            id: format!("j{start_ts}"),
            status: status.to_string(),
            title: title.to_string(),
            channel_name: "Chan".to_string(),
            display_title: title.to_string(),
            channel: serde_json::json!({}),
            program: serde_json::json!({}),
            format: "ts".to_string(),
            start_at: String::new(),
            end_at: String::new(),
            start_ts,
            stop_ts: start_ts + 3600.0,
            pre_padding_minutes: 0,
            post_padding_minutes: 0,
            created_at: String::new(),
            started_late: false,
            wait_reason: String::new(),
            recording_id: None,
            output_path: String::new(),
            message: String::new(),
            repeat: String::new(),
            ..DvrJob::default()
        }
    }

    #[test]
    fn pending_reminders_count_only_live_states() {
        let now = 1_000_000;
        let reminders = vec![
            reminder(ReminderStatus::Scheduled, now + 60),
            reminder(ReminderStatus::Ready, now + 60),
            reminder(ReminderStatus::Notified, now + 60),
            reminder(ReminderStatus::Expired, now + 60),
            reminder(ReminderStatus::Scheduled, now - 1),
        ];
        assert_eq!(pending_reminder_count(&reminders, now), 2);
    }

    #[test]
    fn prompts_follow_pythons_order_and_wording() {
        let now = 1_700_000_000;
        let reminders = vec![reminder(ReminderStatus::Scheduled, now + 600)];
        let jobs = vec![
            job(STATUS_SCHEDULED, "Later Show", 1_700_007_200.0),
            job("completed", "Done Show", 1_700_000_060.0),
            job(STATUS_WAITING, "First Show", 1_700_003_600.0),
        ];
        let prompts = exit_prompts(&reminders, &jobs, false, true, now);
        assert_eq!(prompts.len(), 3);
        assert_eq!(prompts[0].title, "Programme Reminders");
        assert!(prompts[0].no_default);
        assert!(prompts[0]
            .message
            .contains("Pending programme reminders: 1."));
        assert_eq!(prompts[1].title, "Scheduled recordings");
        assert!(prompts[1]
            .message
            .starts_with("2 recording is scheduled but has not started yet."));
        // Earliest first in the names block, window after title.
        let first_pos = prompts[1].message.find("First Show").unwrap();
        let later_pos = prompts[1].message.find("Later Show").unwrap();
        assert!(first_pos < later_pos);
        assert!(prompts[1].message.contains(" - "));
        assert_eq!(prompts[2].title, "Download in progress");
    }

    #[test]
    fn active_recording_changes_the_recordings_wording() {
        let now = 1_700_000_000;
        let jobs = vec![job(STATUS_SCHEDULED, "Show", 1_700_000_060.0)];
        let prompts = exit_prompts(&[], &jobs, true, false, now);
        assert_eq!(prompts.len(), 1);
        assert!(prompts[0]
            .message
            .starts_with("A recording is in progress and 1 recording is still scheduled."));
    }

    #[test]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::float_cmp
    )] // test epochs
    fn leaving_lists_the_next_unpaused_recurrence() {
        // Python test_leaving_lists_next_unpaused_recurrence_instead_of_paused_occurrence.
        use chrono::TimeZone as _;
        let now = 1_800_000_000.0;
        let local_now = chrono::Local
            .timestamp_opt(now as i64, 0)
            .single()
            .expect("local now");
        let future_date = local_now.date_naive() + chrono::Duration::days(2);
        let noon = future_date
            .and_hms_opt(12, 0, 0)
            .expect("noon")
            .and_local_timezone(chrono::Local)
            .single()
            .expect("local noon");
        let start = noon.timestamp() as f64;
        let mut row = job(STATUS_SCHEDULED, "News", start);
        row.repeat = "daily".to_string();
        row.stop_ts = start + 600.0;
        let date_text = future_date.format("%Y-%m-%d").to_string();
        row.pause_dates = Some([date_text.clone(), date_text]);
        let mut one_off = row.clone();
        one_off.id = "one-off".to_string();
        one_off.repeat = String::new();
        let original_start = row.start_ts;
        let upcoming = upcoming_recording_jobs(&[row, one_off], now);
        assert_eq!(upcoming.len(), 1);
        let shown_date = chrono::Local
            .timestamp_opt(upcoming[0].start_ts as i64, 0)
            .single()
            .expect("shown")
            .date_naive();
        assert_eq!(shown_date, future_date + chrono::Duration::days(1));
        assert_eq!(original_start, start, "the stored job is not moved");
    }

    #[test]
    fn nothing_at_risk_means_no_prompts() {
        let now = 1_700_000_000;
        let jobs = vec![job("completed", "Done", 1_700_000_060.0)];
        assert!(exit_prompts(&[], &jobs, false, false, now).is_empty());
        // A running recording alone does not prompt: Python only
        // warns about recordings through the scheduled-jobs box.
        assert!(exit_prompts(&[], &[], true, false, now).is_empty());
    }
}
