//! Periodic recording-space checks, grouped by destination
//! filesystem (Python `storage_monitor.py`).
//!
//! The check runs while recordings are active. Recordings on
//! the same drive share one warning; if automatic stopping is
//! enabled, every non-already-stopping recording on a
//! critically full drive is stopped gracefully. The check
//! cannot reserve space or guarantee file closure on a full
//! or disconnected drive.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use crate::config::Config;

/// One mebibyte in bytes.
pub const MIB: u64 = 1024 * 1024;

/// Smallest accepted threshold value, MiB (Python's spin range).
pub const MIN_MIB: i64 = 16;

/// Largest accepted threshold value, MiB (Python's spin range).
pub const MAX_MIB: i64 = 1_048_576;

/// Default warning threshold, MiB.
pub const DEFAULT_WARNING_MIB: u64 = 1024;

/// Default stop threshold, MiB.
pub const DEFAULT_STOP_MIB: u64 = 256;

/// What one drive currently needs reporting, for the app's
/// one-warning-per-drive rule (Python's `notified` states).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpaceState {
    Warning,
    Stopping,
    Unavailable,
}

/// The four space-monitor preferences, as read from the
/// config (Python `storage_monitor.settings`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Preferences {
    pub enabled: bool,
    pub warning_mib: u64,
    pub stop_mib: u64,
    pub auto_stop: bool,
}

fn number(config: &Config, key: &str, default: u64) -> u64 {
    // Python `type(value) is int and 16 <= value <= 1048576`:
    // booleans, floats and strings fall back to the default.
    match config.value(key).and_then(serde_json::Value::as_i64) {
        Some(value) if (MIN_MIB..=MAX_MIB).contains(&value) => {
            u64::try_from(value).unwrap_or(default)
        }
        _ => default,
    }
}

/// Python `settings`: the warning threshold is never below
/// the stop threshold, monitoring is on unless explicitly
/// disabled (`is not False`), and automatic stopping is on
/// only for a real JSON true (`is True`).
#[must_use]
pub fn settings(config: &Config) -> Preferences {
    let stop_mib = number(config, "recording_stop_free_mib", DEFAULT_STOP_MIB);
    let warning_mib = number(config, "recording_warn_free_mib", DEFAULT_WARNING_MIB).max(stop_mib);
    let enabled = !matches!(
        config.value("recording_monitor_space"),
        Some(serde_json::Value::Bool(false))
    );
    let auto_stop = matches!(
        config.value("recording_stop_low_space"),
        Some(serde_json::Value::Bool(true))
    );
    Preferences {
        enabled,
        warning_mib,
        stop_mib,
        auto_stop,
    }
}

type DriveKey = (Option<u64>, String);
type Group<'a> = (DriveKey, PathBuf, Vec<&'a SpaceRecording>);

/// One active recording for the check: its id, the file it
/// is writing, and whether a stop is already in progress.
#[derive(Clone, Debug)]
pub struct SpaceRecording {
    pub id: String,
    pub path: PathBuf,
    pub stopping: bool,
}

/// Python's group key: `(st_dev, drive)`, falling back to
/// `(None, directory)` when the directory cannot be stat'ed.
/// The stable Windows API has no volume serial, and the
/// drive letter already names the volume.
#[cfg(windows)]
fn drive_key(dir: &Path) -> (Option<u64>, String) {
    (None, drive_prefix(dir))
}

#[cfg(all(not(windows), unix))]
fn drive_key(dir: &Path) -> (Option<u64>, String) {
    use std::os::unix::fs::MetadataExt;
    match std::fs::metadata(dir) {
        Ok(meta) => (Some(meta.dev()), drive_prefix(dir)),
        Err(_) => (None, dir.to_string_lossy().into_owned()),
    }
}

#[cfg(all(not(windows), not(unix)))]
fn drive_key(dir: &Path) -> (Option<u64>, String) {
    let _ = dir;
    (None, drive_prefix(dir))
}

/// Python `os.path.splitdrive(directory)[0].casefold()`: the
/// drive letter on Windows, empty elsewhere.
fn drive_prefix(dir: &Path) -> String {
    match dir.components().next() {
        Some(Component::Prefix(prefix)) => prefix.as_os_str().to_string_lossy().to_lowercase(),
        _ => String::new(),
    }
}

fn parent_dir(path: &Path) -> PathBuf {
    // Python `os.path.dirname(os.path.abspath(...))`: a bare
    // file name resolves against the current directory.
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
}

/// The stateful monitor: remembers what each drive was last
/// told, so a drive warns once (Python `SpaceMonitor`).
#[derive(Clone, Default)]
pub struct SpaceMonitor {
    notified: HashMap<(Option<u64>, String), SpaceState>,
}

impl SpaceMonitor {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget every drive's last report (Python clears
    /// `notified` when monitoring is off or no recording is
    /// left).
    pub fn clear(&mut self) {
        self.notified.clear();
    }

    /// Check every drive holding an active recording once.
    ///
    /// `free_bytes` reports the free bytes for a directory,
    /// or `None` when the filesystem cannot be queried.
    /// `notify` receives the state, the directory, the free
    /// space in whole MiB and the recording count on that
    /// drive; `stop` receives each recording id to stop
    /// gracefully.
    pub fn check(
        &mut self,
        recordings: &[SpaceRecording],
        prefs: &Preferences,
        free_bytes: impl Fn(&Path) -> Option<u64>,
        notify: &mut impl FnMut(SpaceState, &Path, u64, usize),
        stop: &mut impl FnMut(&str),
    ) {
        if !prefs.enabled {
            self.notified.clear();
            return;
        }
        let mut groups: Vec<Group<'_>> = Vec::new();
        for rec in recordings {
            let dir = parent_dir(&rec.path);
            let key = drive_key(&dir);
            match groups.iter_mut().find(|(existing, _, _)| *existing == key) {
                Some((_, _, rows)) => rows.push(rec),
                None => groups.push((key, dir, vec![rec])),
            }
        }
        self.notified
            .retain(|key, _| groups.iter().any(|(existing, _, _)| existing == key));
        for (key, dir, rows) in &groups {
            let Some(free) = free_bytes(dir) else {
                if self.notified.get(key) != Some(&SpaceState::Unavailable) {
                    self.notified.insert(key.clone(), SpaceState::Unavailable);
                    notify(SpaceState::Unavailable, dir, 0, rows.len());
                }
                continue;
            };
            let state = if prefs.auto_stop && free < prefs.stop_mib * MIB {
                SpaceState::Stopping
            } else if free < prefs.warning_mib * MIB {
                SpaceState::Warning
            } else {
                // Python `free >= warning * MIB * 1.1`: only a
                // real recovery forgets the warning, unless
                // the last report on this drive was an
                // unavailable query.
                let recovered =
                    u128::from(free) * 10 >= u128::from(prefs.warning_mib) * u128::from(MIB) * 11;
                if recovered || self.notified.get(key) == Some(&SpaceState::Unavailable) {
                    self.notified.remove(key);
                }
                continue;
            };
            if state == SpaceState::Stopping {
                for rec in rows {
                    if !rec.stopping {
                        stop(&rec.id);
                    }
                }
            }
            if self.notified.get(key) != Some(&state) {
                self.notified.insert(key.clone(), state);
                notify(state, dir, free / MIB, rows.len());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn prefs() -> Preferences {
        Preferences {
            enabled: true,
            warning_mib: 1024,
            stop_mib: 256,
            auto_stop: true,
        }
    }

    fn two_rows() -> Vec<SpaceRecording> {
        (0..2)
            .map(|i| SpaceRecording {
                id: i.to_string(),
                path: PathBuf::from(format!("{i}")),
                stopping: false,
            })
            .collect()
    }

    type Message = (SpaceState, u64, usize);

    fn run(
        monitor: &mut SpaceMonitor,
        recordings: &[SpaceRecording],
        free: Option<u64>,
    ) -> (Vec<Message>, Vec<String>) {
        let mut messages = Vec::new();
        let mut stopped = Vec::new();
        monitor.check(
            recordings,
            &prefs(),
            |_| free,
            &mut |state, _dir, free_mib, count| messages.push((state, free_mib, count)),
            &mut |id: &str| stopped.push(id.to_string()),
        );
        (messages, stopped)
    }

    #[test]
    fn settings_use_defaults() {
        let config = Config::default();
        assert_eq!(
            settings(&config),
            Preferences {
                enabled: true,
                warning_mib: 1024,
                stop_mib: 256,
                auto_stop: false,
            }
        );
    }

    #[test]
    fn settings_validate_ranges_and_keep_warning_above_stop() {
        let config = Config::from_json(json!({
            "recording_warn_free_mib": 2048,
            "recording_stop_free_mib": 512,
            "recording_monitor_space": false,
            "recording_stop_low_space": true,
        }))
        .expect("object");
        assert_eq!(
            settings(&config),
            Preferences {
                enabled: false,
                warning_mib: 2048,
                stop_mib: 512,
                auto_stop: true,
            }
        );
        // Out-of-range, non-int and bound values follow Python.
        for (key, value, expected_warn, expected_stop) in [
            ("recording_warn_free_mib", json!(15), 1024, 256),
            ("recording_warn_free_mib", json!(1_048_577), 1024, 256),
            ("recording_warn_free_mib", json!(1024.0), 1024, 256),
            ("recording_warn_free_mib", json!(true), 1024, 256),
            ("recording_warn_free_mib", json!(16), 256, 256),
            ("recording_warn_free_mib", json!(1_048_576), 1_048_576, 256),
            ("recording_stop_free_mib", json!(2048), 2048, 2048),
            ("recording_stop_free_mib", json!("1024"), 1024, 256),
        ] {
            let mut config = Config::default();
            config.set_value(key, value);
            let prefs = settings(&config);
            assert_eq!(prefs.warning_mib, expected_warn, "{key}");
            assert_eq!(prefs.stop_mib, expected_stop, "{key}");
        }
    }

    #[test]
    fn settings_read_only_json_bools() {
        // Python `is not False` / `is True`: only a real JSON
        // false disables, only a real JSON true enables stopping.
        let mut config = Config::default();
        config.set_value("recording_monitor_space", json!(0));
        config.set_value("recording_stop_low_space", json!(1));
        let prefs = settings(&config);
        assert!(prefs.enabled);
        assert!(!prefs.auto_stop);
        config.set_value("recording_monitor_space", json!("false"));
        config.set_value("recording_stop_low_space", json!("yes"));
        let prefs = settings(&config);
        assert!(prefs.enabled);
        assert!(!prefs.auto_stop);
        config.set_value("recording_monitor_space", json!(false));
        config.set_value("recording_stop_low_space", json!(true));
        let prefs = settings(&config);
        assert!(!prefs.enabled);
        assert!(prefs.auto_stop);
    }

    #[test]
    fn warns_once_per_drive_and_stops_all_affected_recordings() {
        // Python test_space_monitor_warns_once_per_drive_and_stops_all_affected_recordings.
        let mut monitor = SpaceMonitor::new();
        let mut recordings = two_rows();
        let (messages, stopped) = run(&mut monitor, &recordings, Some(512 * MIB));
        assert_eq!(
            messages,
            vec![(SpaceState::Warning, 512, 2)],
            "one warning for both recordings"
        );
        assert!(stopped.is_empty());
        let (messages, _) = run(&mut monitor, &recordings, Some(512 * MIB));
        assert!(messages.is_empty(), "second check stays quiet");
        let (messages, stopped) = run(&mut monitor, &recordings, Some(128 * MIB));
        assert_eq!(stopped, vec!["0".to_string(), "1".to_string()]);
        assert_eq!(messages, vec![(SpaceState::Stopping, 128, 2)]);
        for (rec, stopped_id) in recordings.iter_mut().zip(&stopped) {
            rec.stopping = rec.id == *stopped_id;
        }
        let (messages, stopped) = run(&mut monitor, &recordings, Some(128 * MIB));
        assert!(messages.is_empty());
        assert!(stopped.is_empty());
    }

    #[test]
    fn query_failure_warns_without_stopping() {
        // Python test_space_query_failure_warns_without_stopping.
        let mut monitor = SpaceMonitor::new();
        let recordings = two_rows();
        let (messages, stopped) = run(&mut monitor, &recordings, None);
        assert_eq!(messages, vec![(SpaceState::Unavailable, 0, 2)]);
        assert!(stopped.is_empty());
        let (messages, stopped) = run(&mut monitor, &recordings, None);
        assert!(messages.is_empty(), "unavailable warns once");
        assert!(stopped.is_empty());
    }

    #[test]
    fn disabled_clears_the_notified_state() {
        let mut monitor = SpaceMonitor::new();
        let recordings = two_rows();
        let (messages, _) = run(&mut monitor, &recordings, Some(512 * MIB));
        assert_eq!(messages.len(), 1);
        monitor.check(
            &recordings,
            &Preferences {
                enabled: false,
                ..prefs()
            },
            |_| Some(512 * MIB),
            &mut |_, _, _, _| {},
            &mut |_| {},
        );
        let (messages, _) = run(&mut monitor, &recordings, Some(512 * MIB));
        assert_eq!(
            messages,
            vec![(SpaceState::Warning, 512, 2)],
            "re-enabled monitoring warns again"
        );
    }

    #[test]
    fn only_a_real_recovery_forgets_the_warning() {
        let mut monitor = SpaceMonitor::new();
        let recordings = two_rows();
        run(&mut monitor, &recordings, Some(512 * MIB));
        // Just under 1.1x the warning threshold: still "warned".
        let (messages, _) = run(&mut monitor, &recordings, Some(1125 * MIB));
        assert!(messages.is_empty());
        let (messages, _) = run(&mut monitor, &recordings, Some(512 * MIB));
        assert!(messages.is_empty(), "still warned, no re-notify");
        // Past 1.1x: forgotten, so the next dip warns again.
        let (messages, _) = run(&mut monitor, &recordings, Some(1200 * MIB));
        assert!(messages.is_empty());
        let (messages, _) = run(&mut monitor, &recordings, Some(512 * MIB));
        assert_eq!(
            messages,
            vec![(SpaceState::Warning, 512, 2)],
            "warns again after a full recovery"
        );
    }

    #[test]
    fn an_unavailable_drive_recovers_without_the_full_recovery() {
        let mut monitor = SpaceMonitor::new();
        let recordings = two_rows();
        run(&mut monitor, &recordings, None);
        // Back above the threshold but below 1.1x: the
        // "unavailable" report is forgotten anyway.
        run(&mut monitor, &recordings, Some(1100 * MIB));
        let (messages, _) = run(&mut monitor, &recordings, Some(512 * MIB));
        assert_eq!(
            messages,
            vec![(SpaceState::Warning, 512, 2)],
            "warns again after recovering from unavailable"
        );
    }

    #[cfg(windows)]
    #[test]
    fn different_drives_warn_separately() {
        let mut monitor = SpaceMonitor::new();
        let recordings = vec![
            SpaceRecording {
                id: "c".to_string(),
                path: PathBuf::from(r"C:\rec\a.ts"),
                stopping: false,
            },
            SpaceRecording {
                id: "d".to_string(),
                path: PathBuf::from(r"D:\rec\b.ts"),
                stopping: false,
            },
        ];
        let mut messages = Vec::new();
        monitor.check(
            &recordings,
            &prefs(),
            |_| Some(512 * MIB),
            &mut |state, _dir, _free, count| messages.push((state, count)),
            &mut |_| {},
        );
        assert_eq!(
            messages,
            vec![(SpaceState::Warning, 1), (SpaceState::Warning, 1)],
            "one warning per drive"
        );
    }
}
