//! Optional Windows recording power requests, owned by one
//! dedicated thread (Python `recording_power.py`).
//!
//! No display/away-mode requests and no changes to the user's
//! power plan. Wake timers need the app to remain open and a
//! system that permits waking. On non-Windows platforms this
//! is a no-op and [`supported`] is false, as in Python.

use std::sync::{Arc, Condvar, Mutex};

use crate::config::Config;
use crate::dvr::{DvrJob, STATUS_SCHEDULED};

/// Whether this platform can request power states while
/// recording (Windows only, Python `supported()`).
#[must_use]
pub fn supported() -> bool {
    cfg!(windows)
}

/// Python `config.get(key, default) is True`: only a real JSON
/// boolean true counts, never "yes" or 1.
fn strict_true(config: &Config, key: &str, default: bool) -> bool {
    match config.value(key) {
        None => default,
        Some(value) => *value == serde_json::Value::Bool(true),
    }
}

/// Python `desired_state`: stay awake while recording (when
/// enabled) and up to 90 seconds before the next scheduled
/// start (when wake is enabled); arm a wake timer for 60
/// seconds before that start. Returns `(keep_awake, alarm)`
/// where `alarm` is the wake-timer instant in unix seconds.
#[must_use]
pub fn desired_state(
    config: &Config,
    active: bool,
    jobs: &[DvrJob],
    now: f64,
) -> (bool, Option<f64>) {
    let wake = strict_true(config, "wake_for_recordings", false);
    // PR #44: a paused occurrence does not wake the machine. A
    // paused time repeat wakes for its first occurrence after
    // the pause instead (the wake timer only schedules the
    // coming year, so a pause ending beyond it is ignored).
    let mut starts: Vec<f64> = Vec::new();
    for job in jobs.iter().filter(|job| job.status == STATUS_SCHEDULED) {
        if wake && crate::dvr::paused_occurrence(job) {
            if !crate::dvr::TIME_REPEATS.contains(&job.repeat.as_str()) {
                continue;
            }
            let Some(last) = job
                .pause_dates
                .as_ref()
                .and_then(|period| chrono::NaiveDate::parse_from_str(&period[1], "%Y-%m-%d").ok())
            else {
                continue;
            };
            let horizon =
                crate::dvr::programme_local_start(now + 366.0 * 86_400.0, 0).map(|dt| dt.date());
            if horizon.is_some_and(|date| last > date) {
                continue;
            }
            if let Ok(next) = crate::dvr::next_time_occurrence_from(
                job,
                now,
                &chrono::Local,
                false,
                Some(last + chrono::TimeDelta::days(1)),
            ) {
                starts.push(next.start_ts);
            }
            continue;
        }
        starts.push(job.start_ts);
    }
    let starts: Vec<f64> = starts
        .into_iter()
        .filter(|value| now - 15.0 <= *value && *value <= now + 366.0 * 86_400.0)
        .collect();
    let next_start = if wake {
        starts.into_iter().fold(None, |next: Option<f64>, value| {
            Some(next.map_or(value, |current| current.min(value)))
        })
    } else {
        None
    };
    // Stay awake between the early wake and the scheduler's next tick.
    let imminent = next_start.is_some_and(|start| start - now <= 90.0);
    let keep_awake =
        (strict_true(config, "prevent_sleep_while_recording", true) && active) || imminent;
    let alarm = next_start.and_then(|start| {
        if start - now > 60.0 {
            Some((now + 1.0).max(start - 60.0))
        } else {
            None
        }
    });
    (keep_awake, alarm)
}

/// One snapshot the manager thread reads (Python's
/// `snapshot()`): the config, whether the recorder has an
/// active recording, and the scheduler's jobs without done
/// ones.
#[derive(Clone, Default)]
pub struct PowerSnapshot {
    pub config: Config,
    pub active: bool,
    pub jobs: Vec<DvrJob>,
}

/// The backend the manager thread drives: `update` applies
/// Python `WindowsBackend.update`, `close` releases everything.
pub trait PowerBackend: Send {
    /// Applies the desired state.
    ///
    /// # Errors
    ///
    /// Returns the failure to report to the user.
    fn update(&mut self, awake: bool, alarm: Option<f64>) -> Result<(), String>;
    fn close(&mut self);
}

/// The Windows backend (`SetThreadExecutionState` plus a
/// waitable wake timer). On other platforms it is never
/// constructed: the app gates creation on [`supported`].
#[derive(Default)]
pub struct WindowsBackend {
    #[cfg(windows)]
    awake: bool,
    // The timer handle is owned by this backend and is only
    // ever touched on the thread that calls `update`/`close`.
    #[cfg(windows)]
    timer: Option<*mut std::ffi::c_void>,
    #[cfg(windows)]
    alarm: Option<f64>,
}

#[cfg(windows)]
#[allow(unsafe_code, non_snake_case)] // audited exception: see Cargo.toml lints note
extern "system" {
    fn SetThreadExecutionState(flags: u32) -> u32;
    fn CreateWaitableTimerW(
        security: *mut std::ffi::c_void,
        manual_reset: i32,
        name: *const u16,
    ) -> *mut std::ffi::c_void;
    fn SetWaitableTimer(
        timer: *mut std::ffi::c_void,
        due_time: *const i64,
        period: i32,
        completion_routine: *mut std::ffi::c_void,
        argument: *mut std::ffi::c_void,
        resume: i32,
    ) -> i32;
    fn CancelWaitableTimer(timer: *mut std::ffi::c_void) -> i32;
    fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
}

// SAFETY: the timer handle is owned by this backend and is
// only ever used on the thread the manager passes it to;
// sending the backend itself is what creates that thread.
#[allow(unsafe_code)] // audited exception: see Cargo.toml lints note
unsafe impl Send for WindowsBackend {}

#[cfg(windows)]
impl WindowsBackend {
    const ES_CONTINUOUS: u32 = 0x8000_0000;
    const ES_SYSTEM_REQUIRED: u32 = 1;
    const ERROR_NOT_SUPPORTED: i32 = 50;
    /// Seconds between 1601-01-01 (FILETIME epoch) and
    /// 1970-01-01 (unix epoch).
    const FILETIME_OFFSET_SECONDS: f64 = 11_644_473_600.0;

    fn set_execution_state(flags: u32) -> Result<(), String> {
        // SAFETY: SetThreadExecutionState only changes this
        // process's execution-state request; a zero return
        // means failure and GetLastError reports why.
        #[allow(unsafe_code)] // audited exception: see Cargo.toml lints note
        let result = unsafe { SetThreadExecutionState(flags) };
        if result == 0 {
            Err(std::io::Error::last_os_error().to_string())
        } else {
            Ok(())
        }
    }
}

#[cfg(windows)]
impl PowerBackend for WindowsBackend {
    fn update(&mut self, awake: bool, alarm: Option<f64>) -> Result<(), String> {
        if awake != self.awake {
            // ES_CONTINUOUS | ES_SYSTEM_REQUIRED while
            // awake (0x80000000 | 1), ES_CONTINUOUS alone
            // otherwise (0x80000000), as Python does.
            let flags = if awake {
                Self::ES_CONTINUOUS | Self::ES_SYSTEM_REQUIRED
            } else {
                Self::ES_CONTINUOUS
            };
            Self::set_execution_state(flags)?;
            self.awake = awake;
        }
        if alarm == self.alarm {
            return Ok(());
        }
        if let Some(timer) = self.timer {
            // SAFETY: see the handle rule on the field.
            #[allow(unsafe_code)] // audited exception: see Cargo.toml lints note
            let cancelled = unsafe { CancelWaitableTimer(timer) };
            if cancelled == 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            self.alarm = None;
        }
        let Some(alarm) = alarm else {
            return Ok(());
        };
        let needs_create = match self.timer {
            Some(timer) => timer.is_null(),
            None => true,
        };
        if needs_create {
            // SAFETY: see the handle rule on the field.
            #[allow(unsafe_code)] // audited exception: see Cargo.toml lints note
            let created =
                unsafe { CreateWaitableTimerW(std::ptr::null_mut(), 1, std::ptr::null()) };
            if created.is_null() {
                self.timer = None;
                return Err(std::io::Error::last_os_error().to_string());
            }
            self.timer = Some(created);
        }
        let timer = self.timer.unwrap_or(std::ptr::null_mut());
        let due = ((alarm + Self::FILETIME_OFFSET_SECONDS) * 10_000_000.0).round();
        #[allow(clippy::cast_possible_truncation)]
        let due = due as i64;
        // SAFETY: the pointer to `due` is valid for the call,
        // the handle is owned as above, and expiry resumes
        // normal processing just as `_CRT` defaults do.
        #[allow(unsafe_code)] // audited exception: see Cargo.toml lints note
        let accepted = unsafe {
            SetWaitableTimer(
                timer,
                std::ptr::addr_of!(due),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                1,
            )
        };
        let error = std::io::Error::last_os_error();
        if accepted == 0 || error.raw_os_error() == Some(Self::ERROR_NOT_SUPPORTED) {
            // ERROR_NOT_SUPPORTED can accompany success.
            // SAFETY: see the handle rule on the field.
            #[allow(unsafe_code)] // audited exception: see Cargo.toml lints note
            unsafe {
                let _ = CancelWaitableTimer(timer);
            }
            self.alarm = None;
            return Err("Windows could not enable the recording wake timer.".to_string());
        }
        self.alarm = Some(alarm);
        Ok(())
    }

    fn close(&mut self) {
        // SAFETY: see the handle rule on the field.
        #[allow(unsafe_code)] // audited exception: see Cargo.toml lints note
        unsafe {
            if self.awake {
                let _ = SetThreadExecutionState(Self::ES_CONTINUOUS);
                self.awake = false;
            }
            if let Some(timer) = self.timer.take() {
                let _ = CancelWaitableTimer(timer);
                let _ = CloseHandle(timer);
            }
            self.alarm = None;
        }
    }
}

#[cfg(not(windows))]
impl PowerBackend for WindowsBackend {
    fn update(&mut self, awake: bool, alarm: Option<f64>) -> Result<(), String> {
        let _ = (awake, alarm);
        Err("Recording power management is only supported on Windows.".to_string())
    }

    fn close(&mut self) {}
}

struct PowerState {
    stopping: bool,
}

/// Python `RecordingPower`: one dedicated thread applies the
/// desired state, polling the snapshot every 5 seconds and
/// waking early when [`RecordingPower::refresh`] is called.
///
/// `start` consumes the handle pattern Python relies on:
/// `close` is called exactly once (at app exit) and joins
/// the thread after releasing the backend on that thread.
pub struct RecordingPower {
    snapshot: Arc<Mutex<PowerSnapshot>>,
    state: Arc<(Mutex<PowerState>, Condvar)>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl RecordingPower {
    /// Start the manager thread with the production Windows
    /// backend. `on_error` runs on that thread; the app
    /// marshals the message to the UI itself.
    #[must_use]
    pub fn start(
        snapshot: Arc<Mutex<PowerSnapshot>>,
        on_error: Box<dyn Fn(String) + Send + Sync>,
    ) -> Self {
        Self::start_with(snapshot, on_error, Box::new(WindowsBackend::default()))
    }

    fn start_with(
        snapshot: Arc<Mutex<PowerSnapshot>>,
        on_error: Box<dyn Fn(String) + Send + Sync>,
        backend: Box<dyn PowerBackend>,
    ) -> Self {
        let state = Arc::new((Mutex::new(PowerState { stopping: false }), Condvar::new()));
        let thread_snapshot = Arc::clone(&snapshot);
        let thread_state = Arc::clone(&state);
        let thread = std::thread::Builder::new()
            .name("RecordingPower".to_string())
            .spawn(move || run_power_loop(&thread_snapshot, &thread_state, backend, &on_error))
            .expect("RecordingPower thread starts");
        Self {
            snapshot,
            state,
            thread: Some(thread),
        }
    }

    /// Python `refresh()`: recompute now instead of at the
    /// next 5-second poll.
    pub fn refresh(&self) {
        self.state.1.notify_all();
    }

    /// Python `close()`: stop the thread and release the
    /// backend (execution state and wake timer) on it.
    pub fn close(mut self) {
        {
            let (lock, cvar) = &*self.state;
            if let Ok(mut state) = lock.lock() {
                state.stopping = true;
            }
            cvar.notify_all();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }

    /// Python `manager.stopping.is_set()`: the app suppresses
    /// failure boxes once the manager is closing.
    #[must_use]
    pub fn is_stopping(&self) -> bool {
        self.state.0.lock().is_ok_and(|state| state.stopping)
    }

    /// The shared snapshot the app rewrites before `refresh`.
    #[must_use]
    pub fn snapshot(&self) -> &Arc<Mutex<PowerSnapshot>> {
        &self.snapshot
    }
}

fn now_seconds() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |duration| duration.as_secs_f64())
}

fn run_power_loop(
    snapshot: &Arc<Mutex<PowerSnapshot>>,
    state: &Arc<(Mutex<PowerState>, Condvar)>,
    mut backend: Box<dyn PowerBackend>,
    on_error: &(dyn Fn(String) + Send + Sync),
) {
    let mut reported = false;
    loop {
        let stopping = state.0.lock().map_or(true, |state| state.stopping);
        if stopping {
            break;
        }
        let current = snapshot
            .lock()
            .map_or_else(|_| PowerSnapshot::default(), |guard| guard.clone());
        let (awake, alarm) = desired_state(
            &current.config,
            current.active,
            &current.jobs,
            now_seconds(),
        );
        match backend.update(awake, alarm) {
            Ok(()) => reported = false,
            Err(error) => {
                if !reported {
                    on_error(error);
                    reported = true;
                }
            }
        }
        if let Ok(guard) = state.0.lock() {
            let _ = state
                .1
                .wait_timeout(guard, std::time::Duration::from_secs(5));
        }
    }
    backend.close();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dvr::STATUS_SCHEDULED;
    use serde_json::json;

    fn job(status: &str, start_ts: f64) -> DvrJob {
        DvrJob {
            status: status.to_string(),
            start_ts,
            ..DvrJob::default()
        }
    }

    fn config(value: serde_json::Value) -> Config {
        Config::from_json(value).expect("object")
    }

    #[test]
    fn desired_state_follows_only_pending_jobs_and_guards_the_wake_interval() {
        // Python test_power_timer_follows_only_pending_jobs_and_guards_the_wake_interval.
        let pending = job(STATUS_SCHEDULED, 1000.0);
        let wake = config(json!({"wake_for_recordings": true}));
        assert_eq!(
            desired_state(&wake, false, std::slice::from_ref(&pending), 100.0),
            (false, Some(940.0))
        );
        assert_eq!(
            desired_state(&wake, false, std::slice::from_ref(&pending), 950.0),
            (true, None)
        );
        assert_eq!(desired_state(&wake, true, &[], 100.0), (true, None));
        assert_eq!(
            desired_state(
                &config(json!({})),
                false,
                std::slice::from_ref(&pending),
                100.0
            ),
            (false, None)
        );
        assert_eq!(
            desired_state(&wake, false, &[job("canceled", 1000.0)], 100.0),
            (false, None)
        );
        // prevent_sleep off blocks the active-recording leg.
        let no_prevent = config(json!({"prevent_sleep_while_recording": false}));
        assert_eq!(desired_state(&no_prevent, true, &[], 100.0), (false, None));
        // A stale or distant start is ignored.
        assert_eq!(
            desired_state(&wake, false, &[job(STATUS_SCHEDULED, 80.0)], 100.0),
            (false, None)
        );
        assert_eq!(
            desired_state(
                &wake,
                false,
                &[job(STATUS_SCHEDULED, 100.0 + 400.0 * 86_400.0)],
                100.0,
            ),
            (false, None)
        );
    }

    #[test]
    fn desired_state_skips_paused_occurrences() {
        let wake = config(json!({"wake_for_recordings": true}));
        // A paused series rule never wakes for the paused start.
        let mut series = job(STATUS_SCHEDULED, 1_850_000_000.0);
        series.repeat = "series".to_string();
        let date = crate::dvr::programme_local_date(&series)
            .expect("date")
            .format("%Y-%m-%d")
            .to_string();
        series.pause_dates = Some([date.clone(), date.clone()]);
        assert_eq!(
            desired_state(&wake, false, std::slice::from_ref(&series), 1_849_000_000.0),
            (false, None)
        );
        // A paused daily rule wakes for the day after the pause:
        // its alarm is a minute before that start.
        let mut daily = job(STATUS_SCHEDULED, 1_850_000_000.0);
        daily.stop_ts = 1_850_003_600.0;
        daily.repeat = "daily".to_string();
        daily.pause_dates = Some([date.clone(), date]);
        let (_keep, alarm) =
            desired_state(&wake, false, std::slice::from_ref(&daily), 1_849_000_000.0);
        let alarm = alarm.expect("alarm for the resumed occurrence");
        assert!(
            alarm > 1_850_000_000.0,
            "alarm {alarm} must be after the paused start"
        );
    }

    #[test]
    fn desired_state_reads_only_json_true() {
        // Python `is True`: "yes" and 1 do not enable either option.
        // `Config::default` skips the load-time normalization
        // (Python `normalize_config` coerces these keys to
        // real booleans before `desired_state` ever sees them).
        let mut yes = Config::default();
        yes.set_value("wake_for_recordings", json!("yes"));
        yes.set_value("prevent_sleep_while_recording", json!(1));
        assert_eq!(
            desired_state(&yes, true, &[job(STATUS_SCHEDULED, 1000.0)], 100.0),
            (false, None)
        );
    }

    #[test]
    fn backend_is_released_on_the_same_thread_after_failure() {
        // Python test_power_backend_is_released_on_the_same_thread_after_failure.
        use std::sync::mpsc;
        struct Failing {
            calls: Arc<Mutex<Vec<String>>>,
        }
        impl PowerBackend for Failing {
            fn update(&mut self, _awake: bool, _alarm: Option<f64>) -> Result<(), String> {
                self.calls.lock().expect("lock").push("update".to_string());
                Err("fixture failure".to_string())
            }
            fn close(&mut self) {
                self.calls.lock().expect("lock").push("close".to_string());
            }
        }
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let (error_tx, error_rx) = mpsc::channel();
        let backend: Box<dyn PowerBackend> = Box::new(Failing {
            calls: Arc::clone(&calls),
        });
        let manager = RecordingPower::start_with(
            Arc::new(Mutex::new(PowerSnapshot::default())),
            Box::new(move |error: String| {
                let _ = error_tx.send(error);
            }),
            backend,
        );
        assert_eq!(
            error_rx.recv_timeout(std::time::Duration::from_secs(2)),
            Ok("fixture failure".to_string())
        );
        // No second report while the failure persists.
        assert!(error_rx
            .recv_timeout(std::time::Duration::from_millis(200))
            .is_err());
        manager.close();
        assert_eq!(
            *calls.lock().expect("lock"),
            vec!["update".to_string(), "close".to_string()]
        );
    }

    #[test]
    fn platforms_agree_on_support() {
        assert_eq!(supported(), cfg!(windows));
    }
}
