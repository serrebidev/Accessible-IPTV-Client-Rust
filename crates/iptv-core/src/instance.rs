//! Single-instance handover (port of the Python `single_instance.py`).
//!
//! Two copies share one config directory, one EPG database, one log
//! and one recording schedule, and nothing coordinates them: both
//! schedulers can start the same recording, settings saves collide,
//! and provider connections fight. A second launch therefore hands
//! over instead of running: it asks the running copy to come to the
//! front and exits once that copy has taken the request. The running
//! copy polls for the request with a timer.
//!
//! The lock is a file in the config directory, not a named mutex: a
//! session-scoped mutex lets the same account in the console and in
//! an RDP session each start a copy against the same config
//! directory. The OS releases the file lock if the process dies.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// File a later launch writes to ask the running copy to show itself.
pub const SHOW_REQUEST_FILE: &str = "show_request";
/// Exit code of a launch that handed over to the running copy; the
/// update helper treats it as a successful restart, not a crash.
pub const HANDED_OVER_EXIT_CODE: i32 = 12;

const LOCK_FILE_NAME: &str = "instance.lock";

/// The held lock; dropping it (or process death) releases the lock.
#[derive(Debug)]
pub struct InstanceGuard {
    // Kept only for its lock; never read or written after locking.
    _file: File,
}

/// The guard for this process, or `None` when another copy already
/// holds the lock in `config_dir` (port of `acquire`).
#[must_use]
pub fn acquire(config_dir: &Path) -> Option<InstanceGuard> {
    let _ = std::fs::create_dir_all(config_dir);
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(config_dir.join(LOCK_FILE_NAME))
        .ok()?;
    // `File::try_lock` is the std flock/LockFile wrapper: it fails
    // while any other open handle holds the lock, in this process or
    // another, and the OS releases it when the holder dies.
    file.try_lock().ok()?;
    Some(InstanceGuard { _file: file })
}

fn request_path(config_dir: &Path) -> PathBuf {
    config_dir.join(SHOW_REQUEST_FILE)
}

/// Ask the running copy to come to the front (port of
/// `request_show`).
pub fn request_show(config_dir: &Path) {
    let _ = std::fs::create_dir_all(config_dir);
    if let Ok(mut file) = File::create(request_path(config_dir)) {
        let _ = write!(file, "{}", std::process::id());
    }
}

/// Running copy: whether a later launch asked for the window; the
/// request is consumed either way (port of `take_show_request`).
#[must_use]
pub fn take_show_request(config_dir: &Path) -> bool {
    std::fs::remove_file(request_path(config_dir)).is_ok()
}

/// Drop any pending show request (port of `clear_show_request`).
pub fn clear_show_request(config_dir: &Path) {
    let _ = take_show_request(config_dir);
}

/// The outcome of a second launch's handover attempt.
#[derive(Debug)]
pub enum HandOver {
    /// The running copy took the request and showed itself.
    Shown,
    /// The running copy exited meanwhile; this launch holds the
    /// lock and runs after all.
    Acquired(InstanceGuard),
    /// The deadline passed with the request unanswered.
    Unresponsive,
}

/// Second launch: pass the request on and wait for the answer (port
/// of `hand_over`).
#[must_use]
pub fn hand_over(config_dir: &Path, timeout: Duration) -> HandOver {
    request_show(config_dir);
    let deadline = Instant::now() + timeout;
    loop {
        if !request_path(config_dir).exists() {
            return HandOver::Shown;
        }
        if let Some(guard) = acquire(config_dir) {
            clear_show_request(config_dir);
            return HandOver::Acquired(guard);
        }
        if Instant::now() >= deadline {
            clear_show_request(config_dir);
            return HandOver::Unresponsive;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("iptv-instance-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        dir
    }

    #[test]
    fn second_acquire_fails_while_the_first_guard_lives() {
        let dir = temp_dir("lock");
        let first = acquire(&dir).expect("first launch holds the lock");
        assert!(acquire(&dir).is_none(), "second launch must hand over");
        drop(first);
        assert!(acquire(&dir).is_some(), "a dead holder frees the lock");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn show_request_is_written_and_consumed_once() {
        let dir = temp_dir("show");
        assert!(!take_show_request(&dir));
        request_show(&dir);
        assert!(request_path(&dir).exists());
        assert!(take_show_request(&dir), "the poll takes the request");
        assert!(!take_show_request(&dir), "and it does not fire twice");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hand_over_reports_shown_when_the_running_copy_takes_the_request() {
        let dir = temp_dir("shown");
        let _holder = acquire(&dir).expect("running copy");
        let dir_clone = dir.clone();
        let consumer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(60));
            assert!(take_show_request(&dir_clone));
        });
        match hand_over(&dir, Duration::from_secs(5)) {
            HandOver::Shown => {}
            _ => panic!("expected Shown"),
        }
        consumer.join().expect("consumer");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hand_over_runs_after_all_when_the_lock_is_free() {
        let dir = temp_dir("free");
        // Nobody holds the lock: the "running copy" was closing, so
        // this launch acquires and proceeds.
        match hand_over(&dir, Duration::from_secs(5)) {
            HandOver::Acquired(_guard) => {}
            _ => panic!("expected Acquired"),
        }
        assert!(
            !request_path(&dir).exists(),
            "its own request is cleared, not left for a later poll"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hand_over_reports_an_unresponsive_holder() {
        let dir = temp_dir("stuck");
        let _holder = acquire(&dir).expect("running copy");
        match hand_over(&dir, Duration::from_millis(200)) {
            HandOver::Unresponsive => {}
            _ => panic!("expected Unresponsive"),
        }
        assert!(!request_path(&dir).exists(), "the stale request is cleared");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
