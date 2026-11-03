//! The application log file and the diagnostics copy (port of the
//! Python app's file logging plus `main.py`'s diagnostic copy and
//! `diagnostic_privacy.py`).
//!
//! Python routes every module logger into one rotating file,
//! `iptvclient_epg_debug.log`, in a `logs` folder; Open Logs opens
//! that folder and Copy Diagnostic Information copies operational
//! details plus a redacted, bounded 128 KiB tail of the log. This
//! port keeps the same file name and layout with a deliberately
//! small sink: one mutex-guarded writer, one size-based rollover,
//! no logging framework.
//!
//! Stream URLs and credentials are deliberately kept in the local
//! log so failures can be diagnosed from it; redaction belongs only
//! to the export path (`sharing_excerpt`), never to the file.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The log file name, shared with the Python app.
pub const LOG_FILE_NAME: &str = "iptvclient_epg_debug.log";
/// The subfolder holding the log (Python's `LOG_DIR_NAME`).
pub const LOG_DIR_NAME: &str = "logs";
/// Copy diagnostics include at most this much log tail (128 KiB).
pub const DIAGNOSTIC_TAIL_BYTES: u64 = 128 * 1024;
/// The log rolls over once past this size (5 MiB, as Python's
/// rotating handler), keeping one backup.
pub const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

/// The folder Open Logs opens and the log lives in. Python's
/// `get_logs_dir` law (audit S05): the config directory on Windows,
/// the system temp directory everywhere else — a portable or
/// read-only install location must never be able to silence the
/// log, and tech-support instructions name one place per platform.
#[must_use]
pub fn logs_dir(config_dir: &Path) -> PathBuf {
    if cfg!(windows) {
        config_dir.join(LOG_DIR_NAME)
    } else {
        std::env::temp_dir().join(LOG_DIR_NAME)
    }
}

/// The log file path inside `config_dir`'s logs folder.
#[must_use]
pub fn log_path(config_dir: &Path) -> PathBuf {
    logs_dir(config_dir).join(LOG_FILE_NAME)
}

/// A minimal append-only log sink with one size-based rollover.
pub struct LogSink {
    path: PathBuf,
    max_bytes: u64,
    writer: Mutex<Option<std::io::BufWriter<File>>>,
    size: Mutex<u64>,
}

impl LogSink {
    /// Open (creating folders) the sink at `path`.
    #[must_use]
    pub fn open(path: &Path) -> Self {
        Self::open_with_limit(path, MAX_LOG_BYTES)
    }

    /// Open with an explicit rollover size (tests use a small one).
    #[must_use]
    pub fn open_with_limit(path: &Path, max_bytes: u64) -> Self {
        let sink = Self {
            path: path.to_path_buf(),
            max_bytes,
            writer: Mutex::new(None),
            size: Mutex::new(0),
        };
        sink.roll_if_needed();
        if let Ok(metadata) = std::fs::metadata(path) {
            if let Ok(mut size) = sink.size.lock() {
                *size = metadata.len();
            }
        }
        sink
    }

    /// The file this sink writes.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn roll_if_needed(&self) {
        let Ok(metadata) = std::fs::metadata(&self.path) else {
            return;
        };
        if metadata.len() > self.max_bytes {
            let backup = PathBuf::from(format!("{}.1", self.path.display()));
            let _ = std::fs::rename(&self.path, &backup);
        }
    }

    /// Append one line: `{timestamp} [{level}] {target}: {message}`.
    /// Failures are swallowed: logging must never break the app.
    pub fn write(&self, level: &str, target: &str, message: &str) {
        let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        let line = format!("{stamp} [{level}] {target}: {message}\n");
        let Ok(mut slot) = self.writer.lock() else {
            return;
        };
        if let Ok(mut size) = self.size.lock() {
            *size += line.len() as u64;
            if *size > self.max_bytes && slot.is_some() {
                // Roll before this line so the file stays bounded:
                // close, rename to the single backup, reopen fresh.
                *slot = None;
                self.roll_over();
                *size = 0;
            }
        }
        if slot.is_none() {
            if let Some(parent) = self.path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let Ok(file) = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
            else {
                return;
            };
            *slot = Some(std::io::BufWriter::new(file));
        }
        if let Some(writer) = slot.as_mut() {
            let _ = writer.write_all(line.as_bytes());
            let _ = writer.flush();
        }
    }

    fn roll_over(&self) {
        let backup = PathBuf::from(format!("{}.1", self.path.display()));
        let _ = std::fs::rename(&self.path, &backup);
    }
}

static SINK: std::sync::OnceLock<LogSink> = std::sync::OnceLock::new();

/// Install the process-wide sink (first call wins) and return it.
pub fn init(path: &Path) -> &'static LogSink {
    SINK.get_or_init(|| LogSink::open(path))
}

/// The installed sink, when logging was initialized.
#[must_use]
pub fn sink() -> Option<&'static LogSink> {
    SINK.get()
}

/// Log an error through the installed sink, when one exists.
pub fn error(target: &str, message: &str) {
    if let Some(sink) = sink() {
        sink.write("ERROR", target, message);
    }
}

/// Log a warning through the installed sink, when one exists.
pub fn warn(target: &str, message: &str) {
    if let Some(sink) = sink() {
        sink.write("WARNING", target, message);
    }
}

/// Log an informational line through the installed sink.
pub fn info(target: &str, message: &str) {
    if let Some(sink) = sink() {
        sink.write("INFO", target, message);
    }
}

/// The bounded tail of a log file (port of
/// `_read_diagnostic_log_tail`): at most `max_bytes` from the end,
/// decoded lossily; empty when the file cannot be read.
#[must_use]
pub fn read_tail(path: &Path, max_bytes: u64) -> String {
    let Ok(mut file) = File::open(path) else {
        return String::new();
    };
    let Ok(size) = file.seek(SeekFrom::End(0)) else {
        return String::new();
    };
    let start = size.saturating_sub(max_bytes);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).is_err() {
        return String::new();
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

const SECRET_WORDS: [&str; 15] = [
    "password",
    "passwd",
    "username",
    "user",
    "token",
    "access_token",
    "refresh_token",
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
    "secret",
    "api-key",
    "api_key",
    "mac",
];

fn has_scheme_address(line: &str) -> bool {
    // Python: `[a-z][a-z0-9+.-]*://`, case-insensitive.
    let bytes = line.as_bytes();
    for start in 0..bytes.len() {
        if !bytes[start].is_ascii_alphabetic() {
            continue;
        }
        let mut end = start + 1;
        while end < bytes.len()
            && (bytes[end].is_ascii_alphanumeric() || b"+.-".contains(&bytes[end]))
        {
            end += 1;
        }
        if line[end..].starts_with("://") {
            return true;
        }
    }
    false
}

fn has_secret_assignment(line: &str) -> bool {
    // Python: a secret word, an optional quote, then `:` or `=`;
    // or `bearer <token>`. Word boundaries on both sides of the key.
    let lower = line.to_lowercase();
    let bytes = lower.as_bytes();
    let word_at = |index: usize| {
        index == 0 || !(bytes[index - 1].is_ascii_alphanumeric() || bytes[index - 1] == b'_')
    };
    for (index, _) in lower.char_indices() {
        if !word_at(index) {
            continue;
        }
        let rest = &lower[index..];
        if let Some(stripped) = rest.strip_prefix("bearer") {
            let after = stripped.trim_start();
            if after.len() < rest[6..].len() && !after.is_empty() {
                return true;
            }
        }
        for word in SECRET_WORDS {
            if let Some(after) = rest.strip_prefix(word) {
                let boundary = after
                    .chars()
                    .next()
                    .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_'));
                if !boundary {
                    continue;
                }
                let after = after.trim_start_matches(['"', '\'']).trim_start();
                if after.starts_with(':') || after.starts_with('=') {
                    return true;
                }
            }
        }
    }
    false
}

/// Redact lines that carry an address or an obvious secret (port of
/// `diagnostic_privacy.sharing_excerpt`): the whole line becomes
/// `[***]`, because quotes, spaces and encoded parameters may
/// otherwise leave credential tails. Best-effort sharing filter,
/// not a guarantee; the original log is never altered.
#[must_use]
pub fn sharing_excerpt(text: &str) -> String {
    text.lines()
        .map(|line| {
            if has_scheme_address(line) || has_secret_assignment(line) {
                "[***]"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The copied diagnostics report (port of
/// `_copy_diagnostic_information`): identity lines, then the
/// redacted bounded log tail.
#[must_use]
pub fn diagnostics_report(
    version: &str,
    platform: &str,
    config_path: &Path,
    epg_enabled: bool,
    playlist_count: usize,
    active_recordings: usize,
    log_tail: &str,
) -> String {
    let mut lines = vec![
        format!("Accessible IPTV Client {version}"),
        format!("Platform: {platform}"),
        format!("Config: {}", config_path.display()),
        format!("EPG enabled: {epg_enabled}"),
        format!("Configured playlists: {playlist_count}"),
        format!("Active recordings: {active_recordings}"),
        String::new(),
        "Log (latest 128 KiB):".to_string(),
    ];
    if log_tail.is_empty() {
        lines.push("(No log has been written yet.)".to_string());
    } else {
        lines.push(sharing_excerpt(log_tail));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("iptv-applog-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        dir
    }

    #[test]
    fn logs_dir_follows_pythons_platform_law() {
        // S05: Windows keeps logs under the config directory; every
        // other platform uses the system temp directory.
        let config = Path::new("/some/config");
        let dir = logs_dir(config);
        if cfg!(windows) {
            assert_eq!(dir, config.join(LOG_DIR_NAME));
        } else {
            assert_eq!(dir, std::env::temp_dir().join(LOG_DIR_NAME));
        }
        assert_eq!(log_path(config), dir.join(LOG_FILE_NAME));
    }

    #[test]
    fn sink_writes_lines_and_rolls_over() {
        let dir = temp_dir("sink");
        let path = dir.join("logs").join(LOG_FILE_NAME);
        let sink = LogSink::open_with_limit(&path, 120);
        for index in 0..12 {
            sink.write("INFO", "test", &format!("line number {index} here"));
        }
        let text = std::fs::read_to_string(&path).expect("log written");
        assert!(text.contains("[INFO] test: line number 11 here"), "{text}");
        assert!(
            text.len() <= 240,
            "rollover bounds the file: {}",
            text.len()
        );
        assert!(
            PathBuf::from(format!("{}.1", path.display())).exists(),
            "the rolled-over backup exists"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tail_read_is_bounded_to_the_end() {
        let dir = temp_dir("tail");
        let path = dir.join("log.txt");
        std::fs::write(&path, "a".repeat(1000) + &"b".repeat(1000)).expect("write");
        let tail = read_tail(&path, 300);
        assert_eq!(tail.len(), 300);
        assert!(tail.bytes().all(|b| b == b'b'));
        assert_eq!(read_tail(&dir.join("missing"), 10), "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn excerpt_redacts_addresses_and_secrets_only() {
        let text = "Guide loaded 42 channels\nFetching http://provider.example/list.m3u8?token=abc\npassword: hunter2\nAuthorization = Bearer xyz\nuser agent string here\nBearer abc123\nMAC=00:11:22:33:44:55\nPlain status line";
        let out = sharing_excerpt(text);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "Guide loaded 42 channels");
        assert_eq!(lines[1], "[***]");
        assert_eq!(lines[2], "[***]");
        assert_eq!(lines[3], "[***]");
        assert_eq!(lines[4], "user agent string here");
        assert_eq!(lines[5], "[***]");
        assert_eq!(lines[6], "[***]");
        assert_eq!(lines[7], "Plain status line");
    }

    #[test]
    fn report_carries_facts_and_the_redacted_tail() {
        let report = diagnostics_report(
            "0.4.4",
            "linux",
            Path::new("/home/u/iptvclient.conf"),
            true,
            3,
            1,
            "ok line\nhttp://secret.example/x",
        );
        assert!(report.contains("Accessible IPTV Client 0.4.4"));
        assert!(report.contains("Configured playlists: 3"));
        assert!(report.contains("Active recordings: 1"));
        assert!(report.contains("ok line"));
        assert!(report.contains("[***]"));
        assert!(!report.contains("secret.example"));
        let empty = diagnostics_report("0.4.4", "linux", Path::new("/x"), false, 0, 0, "");
        assert!(empty.contains("(No log has been written yet.)"));
    }
}
