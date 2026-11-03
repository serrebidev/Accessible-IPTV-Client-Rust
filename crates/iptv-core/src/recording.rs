//! Recording engine pieces ported from Python's `recorder.py` and
//! `power.py`: the format presets, the ffmpeg command builder, output
//! naming, a process manager (stop is `q` on stdin, like the reference),
//! and the shutdown-after-recordings decision.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use crate::playback::StreamHeaders;

/// (key, English label, file extension, kind) — `RECORDING_FORMATS`.
pub const RECORDING_FORMATS: &[(&str, &str, &str, &str)] = &[
    (
        "provider_mkv",
        "Provider quality (copy, MKV)",
        "mkv",
        "video",
    ),
    (
        "provider_mp4",
        "Provider quality (copy, MP4)",
        "mp4",
        "video",
    ),
    ("x264_mkv", "x264 re-encode (MKV)", "mkv", "video"),
    ("x264_mp4", "x264 re-encode (MP4)", "mp4", "video"),
    (
        "provider_mka",
        "Provider quality (stream copy, MKA)",
        "mka",
        "audio",
    ),
    ("audio_mp3_v0", "Audio only (MP3 V0)", "mp3", "audio"),
    ("audio_flac", "Audio only (FLAC)", "flac", "audio"),
    ("audio_wav", "Audio only (WAV)", "wav", "audio"),
    ("audio_aac_m4a", "Audio only (AAC, M4A)", "m4a", "audio"),
    ("audio_opus", "Audio only (Opus)", "opus", "audio"),
];

pub const DEFAULT_RECORDING_FORMAT: &str = "provider_mkv";

fn format_entry(key: &str) -> &'static (&'static str, &'static str, &'static str, &'static str) {
    RECORDING_FORMATS
        .iter()
        .find(|entry| entry.0 == key)
        .unwrap_or(&RECORDING_FORMATS[0])
}

/// The file extension for a format key (unknown keys fall back to the
/// default format, like the reference).
#[must_use]
pub fn format_extension(key: &str) -> &'static str {
    format_entry(key).2
}

/// "video" or "audio".
#[must_use]
pub fn format_kind(key: &str) -> &'static str {
    format_entry(key).3
}

/// Whether the format's muxer gets `+faststart`.
#[must_use]
pub fn format_uses_faststart(key: &str) -> bool {
    matches!(key, "provider_mp4" | "x264_mp4")
}

/// The ffmpeg muxer name for an extension, for `.part` outputs.
#[must_use]
pub fn format_muxer(extension: &str) -> Option<&'static str> {
    Some(match extension {
        "mkv" | "mka" => "matroska",
        "mp4" => "mp4",
        "m4a" => "ipod",
        "mp3" => "mp3",
        "flac" => "flac",
        "wav" => "wav",
        "opus" => "oga",
        _ => return None,
    })
}

/// Port of `sanitize_filename`: illegal and control characters become
/// spaces, whitespace collapses, edge spaces/dots trim, 120-char cap.
#[must_use]
pub fn sanitize_filename(name: &str) -> String {
    let trimmed = name.trim();
    let source = if trimmed.is_empty() {
        "Recording"
    } else {
        trimmed
    };
    let mut out = String::new();
    let mut last_was_space = false;
    for ch in source.chars() {
        let illegal = matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
            || (ch as u32) < 0x20;
        if illegal || ch.is_whitespace() {
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
        } else {
            out.push(ch);
            last_was_space = false;
        }
    }
    let out: String = out
        .trim_matches(|c| c == ' ' || c == '.')
        .chars()
        .take(120)
        .collect();
    if out.is_empty() {
        "Recording".to_string()
    } else {
        out
    }
}

/// The upstream default User-Agent the proxy/recorder normalizer adds
/// when a channel carries none.
pub const DEFAULT_UPSTREAM_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
    AppleWebKit/537.36 (KHTML, like Gecko) Chrome/123.0.0.0 Safari/537.36";

/// ffmpeg input options from the channel headers (port of
/// `_header_input_args`): dedicated `-user_agent`/`-referer`, everything
/// else folded into one CRLF `-headers` blob.
#[must_use]
pub fn header_input_args(headers: &StreamHeaders) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    let mut extra_lines: Vec<String> = Vec::new();
    let mut entries: Vec<(String, String)> = headers.entries.clone();
    if headers.get("User-Agent").is_none() {
        entries.push((
            "User-Agent".to_string(),
            DEFAULT_UPSTREAM_USER_AGENT.to_string(),
        ));
    }
    for (name, value) in &entries {
        if value.is_empty() {
            continue;
        }
        if name.eq_ignore_ascii_case("user-agent") {
            args.push("-user_agent".to_string());
            args.push(value.clone());
        } else if name.eq_ignore_ascii_case("referer") {
            args.push("-referer".to_string());
            args.push(value.clone());
        } else {
            extra_lines.push(format!("{name}: {value}"));
        }
    }
    extra_lines.extend(headers.extra.iter().cloned());
    if !extra_lines.is_empty() {
        let mut blob = String::new();
        for line in &extra_lines {
            blob.push_str(line);
            blob.push_str("\r\n");
        }
        args.push("-headers".to_string());
        args.push(blob);
    }
    args
}

/// Optional knobs for [`ffmpeg_command`].
#[derive(Clone, Debug, Default)]
pub struct FfmpegOptions {
    pub duration_seconds: Option<f64>,
    pub show_stats: bool,
    pub force_format: Option<String>,
    pub audio_track: Option<i64>,
    pub audio_track_count: i64,
    pub copy_to_stdout: bool,
    pub log_datetime: bool,
}

/// Construct the full ffmpeg argument list for one recording (port of
/// `build_ffmpeg_command`). Header input args ride only on http(s)
/// inputs; rtsp gets its transport flag; everything else gets neither.
#[must_use]
pub fn ffmpeg_command(
    ffmpeg_path: &str,
    url: &str,
    out_path: &str,
    format: &str,
    headers: &StreamHeaders,
    options: &FfmpegOptions,
) -> Vec<String> {
    let format = if RECORDING_FORMATS.iter().any(|e| e.0 == format) {
        format
    } else {
        DEFAULT_RECORDING_FORMAT
    };
    let mut cmd: Vec<String> = vec![
        ffmpeg_path.to_string(),
        "-hide_banner".to_string(),
        "-loglevel".to_string(),
        if options.log_datetime {
            "level+datetime+info".to_string()
        } else {
            "level+info".to_string()
        },
        "-y".to_string(),
    ];
    if options.show_stats {
        cmd.push("-stats_period".to_string());
        cmd.push("1".to_string());
    } else {
        cmd.push("-nostats".to_string());
    }
    cmd.push("-rw_timeout".to_string());
    cmd.push("15000000".to_string());
    let lower = url.to_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        for flag in [
            "-reconnect",
            "1",
            "-reconnect_streamed",
            "1",
            "-reconnect_delay_max",
            "5",
        ] {
            cmd.push(flag.to_string());
        }
        cmd.extend(header_input_args(headers));
    } else if lower.starts_with("rtsp://") {
        cmd.push("-rtsp_transport".to_string());
        cmd.push("tcp".to_string());
    }
    cmd.push("-i".to_string());
    cmd.push(url.to_string());
    if let Some(duration) = options.duration_seconds {
        if duration > 0.0 {
            cmd.push("-t".to_string());
            cmd.push(duration.to_string());
        }
    }
    cmd.extend(format_args(format, options.audio_track));
    if let Some(track) = options.audio_track {
        if format_kind(format) == "video" {
            let count = options.audio_track_count.max(track + 1);
            for index in 0..count {
                cmd.push(format!("-disposition:a:{index}"));
                cmd.push(
                    if index == track {
                        "+default"
                    } else {
                        "-default"
                    }
                    .to_string(),
                );
            }
        }
    }
    if format_uses_faststart(format) {
        cmd.push("-movflags".to_string());
        cmd.push("+faststart".to_string());
    }
    if let Some(force) = &options.force_format {
        cmd.push("-f".to_string());
        cmd.push(force.clone());
    }
    cmd.push(out_path.to_string());
    if options.copy_to_stdout {
        cmd.extend(
            [
                "-map", "0:v?", "-map", "0:a?", "-c", "copy", "-f", "mpegts", "pipe:1",
            ]
            .iter()
            .map(ToString::to_string),
        );
    }
    cmd
}

/// `<dir>/<Name> - <stamp>.<ext>`, with ` (2)`, ` (3)`… suffixes while
/// the name (or its `.part`) is taken (port of `_unique_output_path`).
/// `when_minute` formats the stamp to the minute (downloads of aired
/// programmes); otherwise to the second (live captures).
#[must_use]
pub fn unique_output_path(
    dir: &Path,
    display_name: &str,
    extension: &str,
    stamp: &str,
    in_use: &[PathBuf],
) -> PathBuf {
    let base = sanitize_filename(display_name);
    let part_of = |path: &Path| PathBuf::from(format!("{}.part", path.display()));
    let taken = |path: &Path| -> bool {
        in_use.iter().any(|p| p == path) || path.exists() || part_of(path).exists()
    };
    let candidate = dir.join(format!("{base} - {stamp}.{extension}"));
    if !taken(&candidate) {
        return candidate;
    }
    let mut counter = 2;
    loop {
        let candidate = dir.join(format!("{base} - {stamp} ({counter}).{extension}"));
        if !taken(&candidate) {
            return candidate;
        }
        counter += 1;
    }
}

/// The per-format map/codec argument block.
fn format_args(format: &str, audio_track: Option<i64>) -> Vec<String> {
    let parts: &[&str] = match format {
        "provider_mp4" => &[
            "-map", "0:v?", "-map", "0:a?", "-dn", "-sn", "-c:v", "copy", "-c:a", "aac", "-b:a",
            "192k",
        ],
        "provider_mka" => &["-map", "0:a?", "-vn", "-sn", "-dn", "-c:a", "copy"],
        "x264_mp4" | "x264_mkv" => &[
            "-map", "0:v?", "-map", "0:a?", "-c:v", "libx264", "-preset", "veryfast", "-crf", "21",
            "-pix_fmt", "yuv420p", "-c:a", "aac", "-b:a", "192k",
        ],
        "audio_wav" => &["-vn", "-c:a", "pcm_s16le"],
        "audio_flac" => &["-vn", "-c:a", "flac"],
        "audio_mp3_v0" => &["-vn", "-c:a", "libmp3lame", "-q:a", "0"],
        "audio_aac_m4a" => &["-vn", "-c:a", "aac", "-b:a", "256k"],
        "audio_opus" => &["-vn", "-c:a", "libopus", "-b:a", "160k"],
        // provider_mkv and anything unrecognized: copy everything.
        _ => &["-map", "0", "-c", "copy"],
    };
    let mut out: Vec<String> = Vec::new();
    if format.starts_with("audio_") {
        if let Some(track) = audio_track {
            out.push("-map".to_string());
            out.push(format!("0:a:{track}"));
        }
    }
    out.extend(parts.iter().map(ToString::to_string));
    out
}

/// One running recording.
struct Active {
    child: Child,
    out_path: PathBuf,
}

/// A finished recording report.
#[derive(Clone, Debug)]
pub struct RecordingFinished {
    pub key: String,
    pub out_path: PathBuf,
    pub success: bool,
}

/// The recording process manager: spawn ffmpeg, stop it gracefully,
/// collect finishes. Keys are caller-chosen (channel keys for manual
/// recordings, `dvr:<job id>` for scheduled ones).
#[derive(Default)]
pub struct RecordingEngine {
    active: BTreeMap<String, Active>,
}

impl RecordingEngine {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn is_recording(&self, key: &str) -> bool {
        self.active.contains_key(key)
    }

    #[must_use]
    pub fn active_count(&self) -> usize {
        self.active.len()
    }

    /// The keys of all active recordings.
    #[must_use]
    pub fn active_keys(&self) -> Vec<String> {
        self.active.keys().cloned().collect()
    }

    /// Paths held by running recordings (for collision checks).
    #[must_use]
    pub fn in_use_paths(&self) -> Vec<PathBuf> {
        self.active.values().map(|a| a.out_path.clone()).collect()
    }

    /// Spawn ffmpeg for one recording.
    ///
    /// # Errors
    ///
    /// Returns the spawn failure text, or the duplicate-key refusal.
    pub fn start(
        &mut self,
        key: &str,
        command: &[String],
        out_path: PathBuf,
    ) -> Result<(), String> {
        if self.active.contains_key(key) {
            return Err("That recording is already running.".to_string());
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("Recording failed: {e}"))?;
        }
        let mut cmd = Command::new(&command[0]);
        cmd.args(&command[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = cmd.spawn().map_err(|e| format!("Recording failed: {e}"))?;
        self.active
            .insert(key.to_string(), Active { child, out_path });
        Ok(())
    }

    /// Ask ffmpeg to finish: `q` on stdin, like the reference; the
    /// process is collected by [`RecordingEngine::poll_finished`].
    pub fn stop(&mut self, key: &str) {
        if let Some(active) = self.active.get_mut(key) {
            if let Some(stdin) = active.child.stdin.as_mut() {
                let _ = stdin.write_all(b"q\n");
                let _ = stdin.flush();
            }
        }
    }

    /// Stop one recording with escalation (R07): write `q` on stdin,
    /// wait up to `grace` for a graceful exit, then kill the child and
    /// reap it. Removes the recording from the active set and returns
    /// its finish report; `success` is the exit status success, so a
    /// killed process reports `false`. Returns `None` when `key` is
    /// not active.
    pub fn stop_escalating(
        &mut self,
        key: &str,
        grace: std::time::Duration,
    ) -> Option<RecordingFinished> {
        if !self.active.contains_key(key) {
            return None;
        }
        self.stop(key);
        let deadline = std::time::Instant::now() + grace;
        let mut success: Option<bool> = None;
        while let Some(active) = self.active.get_mut(key) {
            match active.child.try_wait() {
                Ok(Some(status)) => {
                    success = Some(status.success());
                    break;
                }
                Ok(None) => {}
                Err(_) => {
                    success = Some(false);
                    break;
                }
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let success = match success {
            Some(success) => success,
            None => {
                // Still running past the grace deadline: kill and reap.
                match self.active.get_mut(key) {
                    Some(active) => {
                        let _ = active.child.kill();
                        match active.child.wait() {
                            Ok(status) => status.success(),
                            Err(_) => false,
                        }
                    }
                    None => false,
                }
            }
        };
        let active = self.active.remove(key)?;
        Some(RecordingFinished {
            key: key.to_string(),
            out_path: active.out_path,
            success,
        })
    }

    /// Stop every active recording with escalation (R07), returning a
    /// finish report per recording.
    pub fn shutdown_all(&mut self, grace: std::time::Duration) -> Vec<RecordingFinished> {
        let keys: Vec<String> = self.active.keys().cloned().collect();
        let mut finished = Vec::new();
        for key in keys {
            if let Some(report) = self.stop_escalating(&key, grace) {
                finished.push(report);
            }
        }
        finished
    }

    /// Collect recordings whose ffmpeg has exited.
    pub fn poll_finished(&mut self) -> Vec<RecordingFinished> {
        let mut finished = Vec::new();
        let keys: Vec<String> = self.active.keys().cloned().collect();
        for key in keys {
            let done = match self.active.get_mut(&key) {
                Some(active) => match active.child.try_wait() {
                    Ok(Some(status)) => Some(status.success()),
                    _ => None,
                },
                None => None,
            };
            if let Some(success) = done {
                if let Some(active) = self.active.remove(&key) {
                    finished.push(RecordingFinished {
                        key,
                        out_path: active.out_path,
                        success,
                    });
                }
            }
        }
        finished
    }
}

/// Whether the computer may shut down now (port of
/// Minimum free space required before a recording may start.
pub const MIN_FREE_BYTES: u64 = 500 * 1024 * 1024;

/// Default minimum-free-space reserve in MiB (Python
/// `recording_storage.DEFAULT_RESERVE_MIB`).
pub const DEFAULT_RESERVE_MIB: i64 = 256;

/// Largest reserve the setting accepts (Python `MAX_RESERVE_MIB`).
pub const MAX_RESERVE_MIB: i64 = 65536;

/// Normalize a configured reserve: an integer inside
/// 0..=`MAX_RESERVE_MIB` stands, anything else falls back to the
/// default (Python `normalize_reserve`).
#[must_use]
pub fn normalize_reserve_mib(value: Option<i64>) -> u64 {
    match value {
        Some(v) if (0..=MAX_RESERVE_MIB).contains(&v) => u64::try_from(v).unwrap_or(0),
        _ => u64::try_from(DEFAULT_RESERVE_MIB).unwrap_or(0),
    }
}

/// Parse portable `df -kP` output: the second line's fourth
/// whitespace-separated column is the available space in KiB.
#[must_use]
pub fn parse_df_kp_avail(text: &str) -> Option<u64> {
    let line = text.lines().nth(1)?;
    let kib: u64 = line.split_whitespace().nth(3)?.parse().ok()?;
    kib.checked_mul(1024)
}

/// Parse Windows `dir` output: the free space is the integer token
/// immediately before `bytes free` (thousands separators stripped).
#[must_use]
pub fn parse_dir_free_bytes(text: &str) -> Option<u64> {
    let at = text.rfind("bytes free")?;
    let token = text[..at].split_whitespace().next_back()?;
    token.replace(',', "").parse().ok()
}

/// Free bytes on the filesystem holding `path`, when the platform
/// can report it: Unix `df` (GNU form first, portable `df -kP`
/// fallback for macOS and other non-GNU systems), Windows `dir`;
/// `None` elsewhere.
#[must_use]
pub fn free_space_bytes(path: &std::path::Path) -> Option<u64> {
    #[cfg(unix)]
    {
        if let Some(output) = std::process::Command::new("df")
            .args(["-B1", "--output=avail"])
            .arg(path)
            .output()
            .ok()
            .filter(|o| o.status.success())
        {
            let text = String::from_utf8_lossy(&output.stdout);
            if let Some(free) = text
                .lines()
                .nth(1)
                .and_then(|line| line.trim().parse::<u64>().ok())
            {
                return Some(free);
            }
        }
        let output = std::process::Command::new("df")
            .arg("-kP")
            .arg(path)
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        parse_df_kp_avail(&text)
    }
    #[cfg(windows)]
    {
        let output = std::process::Command::new("cmd")
            .args(["/C", "dir", "/-C"])
            .arg(path)
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&output.stdout);
        return parse_dir_free_bytes(&text);
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        None
    }
}

/// A successful pre-recording destination check (R08): the free
/// space found and the normalized reserve it was checked against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StorageCheck {
    pub free_bytes: u64,
    pub reserve_mib: u64,
}

/// Port of Python `recording_storage.check_destination`: create the
/// destination folder, prove it is writable with a flushed one-byte
/// probe file, then require at least the normalized reserve of free
/// space before any stream is opened.
///
/// # Errors
///
/// Returns a user-facing message when the folder is unavailable or
/// not writable, when free space cannot be determined, or when free
/// space is below the reserve.
pub fn check_destination(dir: &Path, reserve_mib: Option<i64>) -> Result<StorageCheck, String> {
    let reserve = normalize_reserve_mib(reserve_mib);
    let probe_result = (|| -> Result<(), std::io::Error> {
        std::fs::create_dir_all(dir)?;
        if !dir.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotADirectory,
                "not a directory",
            ));
        }
        let unique = format!(
            ".iptv-storage-check-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        );
        let probe_path = dir.join(unique);
        let mut probe = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe_path)?;
        let write_result = probe.write_all(b"\0").and_then(|()| probe.sync_all());
        drop(probe);
        let _ = std::fs::remove_file(&probe_path);
        write_result
    })();
    if let Err(e) = probe_result {
        return Err(format!(
            "The recording folder is unavailable or not writable: {e}"
        ));
    }
    let free = free_space_bytes(dir).ok_or_else(|| {
        "Free space could not be determined: no free-space reading for this destination".to_string()
    })?;
    let reserve_bytes = reserve * 1024 * 1024;
    if free < reserve_bytes {
        return Err(format!(
            "Not enough free space to start recording: {} MiB available; at least {} MiB required.",
            free / (1024 * 1024),
            reserve
        ));
    }
    Ok(StorageCheck {
        free_bytes: free,
        reserve_mib: reserve,
    })
}

/// True only when `path` is an existing file with bytes in it (R09):
/// an exit-zero capture that wrote nothing did not record anything.
#[must_use]
pub fn output_has_media(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.len() > 0)
}

/// `power.should_shutdown`): the option must be armed, at least one
/// recording must actually have run, and nothing may be active or
/// pending.
#[must_use]
pub fn should_shutdown(
    armed: bool,
    recorded_something: bool,
    active_recordings: usize,
    pending_jobs: usize,
) -> bool {
    armed && recorded_something && active_recordings == 0 && pending_jobs == 0
}

/// Candidate shutdown commands for a platform, best first (port of
/// `power.shutdown_commands`; `system` is "windows", "darwin", or other).
#[must_use]
pub fn shutdown_commands(system: &str, windows_root: &str) -> Vec<Vec<String>> {
    match system {
        "windows" => vec![vec![
            format!("{windows_root}\\System32\\shutdown.exe"),
            "/s".to_string(),
            "/t".to_string(),
            "0".to_string(),
        ]],
        "darwin" => vec![
            vec![
                "osascript".to_string(),
                "-e".to_string(),
                "tell application \"System Events\" to shut down".to_string(),
            ],
            vec!["shutdown".to_string(), "-h".to_string(), "now".to_string()],
        ],
        _ => vec![
            vec!["shutdown".to_string(), "-h".to_string(), "now".to_string()],
            vec!["poweroff".to_string()],
        ],
    }
}

/// Run the first shutdown command that works.
///
/// # Errors
///
/// Returns the joined failure texts when every candidate fails.
pub fn shutdown_computer(system: &str, windows_root: &str) -> Result<Vec<String>, String> {
    let mut failures: Vec<String> = Vec::new();
    for cmd in shutdown_commands(system, windows_root) {
        match Command::new(&cmd[0]).args(&cmd[1..]).status() {
            Ok(status) if status.success() => return Ok(cmd),
            Ok(status) => failures.push(format!(
                "{}: exit code {}",
                cmd[0],
                status.code().unwrap_or(-1)
            )),
            Err(e) => failures.push(format!("{}: {e}", cmd[0])),
        }
    }
    Err(if failures.is_empty() {
        "no shutdown command available".to_string()
    } else {
        failures.join("; ")
    })
}

#[cfg(test)]
mod tests {
    // R03: same-title downloads never truncate an earlier file; the
    // reservation walks (2), (3)… past existing files, their .part
    // staging files, and paths the engine already holds.
    #[test]
    fn unique_output_path_suffixes_past_taken_names() {
        let dir = std::env::temp_dir().join(format!("iptv-unique-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let name = "News - BBC";
        let first = unique_output_path(&dir, name, "ts", "2026-11-03 19-00", &[]);
        assert_eq!(
            first.file_name().unwrap(),
            "News - BBC - 2026-11-03 19-00.ts"
        );
        std::fs::write(&first, b"x").unwrap();
        let second = unique_output_path(&dir, name, "ts", "2026-11-03 19-00", &[]);
        assert_eq!(
            second.file_name().unwrap(),
            "News - BBC - 2026-11-03 19-00 (2).ts"
        );
        // An in-use path is skipped even when nothing is on disk yet.
        let third = unique_output_path(
            &dir,
            name,
            "ts",
            "2026-11-03 19-00",
            std::slice::from_ref(&second),
        );
        assert_eq!(
            third.file_name().unwrap(),
            "News - BBC - 2026-11-03 19-00 (3).ts"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reserve_normalization_matches_python() {
        assert_eq!(normalize_reserve_mib(Some(0)), 0);
        assert_eq!(normalize_reserve_mib(Some(1024)), 1024);
        assert_eq!(normalize_reserve_mib(Some(65536)), 65536);
        assert_eq!(normalize_reserve_mib(Some(65537)), 256);
        assert_eq!(normalize_reserve_mib(Some(-5)), 256);
        assert_eq!(normalize_reserve_mib(None), 256);
    }

    use super::*;

    #[test]
    fn sanitize_matches_reference_rules() {
        assert_eq!(sanitize_filename("  A/B: C*D  "), "A B C D");
        assert_eq!(sanitize_filename(""), "Recording");
        assert_eq!(sanitize_filename("..."), "Recording");
        assert_eq!(sanitize_filename(&"x".repeat(200)).chars().count(), 120);
    }

    #[test]
    fn provider_mkv_command_copies_everything() {
        let headers = StreamHeaders::default();
        let cmd = ffmpeg_command(
            "ffmpeg",
            "http://example.com/s",
            "out.mkv",
            "provider_mkv",
            &headers,
            &FfmpegOptions::default(),
        );
        assert!(cmd.contains(&"-reconnect".to_string()));
        assert!(cmd.contains(&"-user_agent".to_string()));
        let map_at = cmd.iter().position(|a| a == "-map").expect("map");
        assert_eq!(cmd[map_at + 1], "0");
        assert_eq!(cmd.last().map(String::as_str), Some("out.mkv"));
    }

    #[test]
    fn mp3_command_picks_the_named_audio_track() {
        let headers = StreamHeaders::default();
        let cmd = ffmpeg_command(
            "ffmpeg",
            "rtsp://example.com/s",
            "out.mp3",
            "audio_mp3_v0",
            &headers,
            &FfmpegOptions {
                audio_track: Some(1),
                ..FfmpegOptions::default()
            },
        );
        assert!(cmd.contains(&"-rtsp_transport".to_string()));
        assert!(!cmd.contains(&"-reconnect".to_string()));
        let map_at = cmd.iter().position(|a| a == "-map").expect("map");
        assert_eq!(cmd[map_at + 1], "0:a:1");
        assert!(cmd.contains(&"libmp3lame".to_string()));
    }

    #[test]
    fn shutdown_gate_needs_all_conditions() {
        assert!(should_shutdown(true, true, 0, 0));
        assert!(!should_shutdown(false, true, 0, 0));
        assert!(!should_shutdown(true, false, 0, 0));
        assert!(!should_shutdown(true, true, 1, 0));
        assert!(!should_shutdown(true, true, 0, 2));
    }

    #[test]
    fn recording_engine_reports_successful_process_completion() {
        let dir =
            std::env::temp_dir().join(format!("iptv-recording-success-{}", std::process::id()));
        let output = dir.join("success.ts");
        let command = test_process_command(0);
        let mut engine = RecordingEngine::new();
        engine
            .start("channel", &command, output.clone())
            .expect("start");

        let finished = wait_for_finished(&mut engine);
        assert_eq!(finished.len(), 1);
        assert_eq!(finished[0].key, "channel");
        assert_eq!(finished[0].out_path, output);
        assert!(finished[0].success);
        assert_eq!(engine.active_count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recording_engine_reports_failed_process_completion() {
        let dir =
            std::env::temp_dir().join(format!("iptv-recording-failure-{}", std::process::id()));
        let output = dir.join("failure.ts");
        let command = test_process_command(7);
        let mut engine = RecordingEngine::new();
        engine
            .start("channel", &command, output.clone())
            .expect("start");

        let finished = wait_for_finished(&mut engine);
        assert_eq!(finished.len(), 1);
        assert_eq!(finished[0].key, "channel");
        assert_eq!(finished[0].out_path, output);
        assert!(!finished[0].success);
        assert_eq!(engine.active_count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ----- R08: storage check and free-space parsing -----

    #[test]
    fn parse_df_kp_avail_reads_fourth_column_of_second_line() {
        let sample = "Filesystem   1024-blocks      Used Available Capacity Mounted on\n\
                      /dev/disk3s1   976490576 400000000 12345678    98% /\n";
        assert_eq!(parse_df_kp_avail(sample), Some(12_345_678 * 1024));
        assert_eq!(parse_df_kp_avail("only one line\n"), None);
        assert_eq!(parse_df_kp_avail("a b\n1 2 3 nope\n"), None);
        assert_eq!(parse_df_kp_avail(""), None);
    }

    #[test]
    fn parse_dir_free_bytes_reads_token_before_bytes_free() {
        let sample = " Volume in drive C is Windows\n\
                      \n Directory of C:\\Recordings\n\
                      \n2026/11/03  12:00    <DIR>          .\n\
                      \n               0 File(s)              0 bytes\n\
                      \n               2 Dir(s)  53,687,091,200 bytes free\n";
        assert_eq!(parse_dir_free_bytes(sample), Some(53_687_091_200));
        let plain = "               1 Dir(s)  987654321 bytes free";
        assert_eq!(parse_dir_free_bytes(plain), Some(987_654_321));
        assert_eq!(parse_dir_free_bytes("no free space line"), None);
        assert_eq!(parse_dir_free_bytes(""), None);
    }

    #[test]
    fn check_destination_succeeds_with_zero_reserve() {
        let dir = std::env::temp_dir().join(format!(
            "iptv-storage-ok-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let check = check_destination(&dir, Some(0)).expect("check passes");
        assert_eq!(check.reserve_mib, 0);
        assert!(check.free_bytes > 0);
        assert!(dir.is_dir(), "destination folder was created");
        // The write probe must clean up after itself.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .expect("read dir")
            .filter_map(Result::ok)
            .collect();
        assert!(leftovers.is_empty(), "probe left files: {leftovers:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_destination_fails_when_reserve_exceeds_free_space() {
        let dir = std::env::temp_dir().join(format!(
            "iptv-storage-low-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let ok = check_destination(&dir, Some(0)).expect("baseline check");
        // A reserve guaranteed above the measured free space, capped
        // to the accepted range so normalization keeps it as-is.
        let too_much = (ok.free_bytes / (1024 * 1024) + 1).min(65536);
        if too_much * 1024 * 1024 > ok.free_bytes {
            let err = check_destination(&dir, Some(i64::try_from(too_much).expect("fits")))
                .expect_err("oversized reserve must fail");
            assert!(
                err.contains("Not enough free space to start recording"),
                "unexpected error: {err}"
            );
        } else {
            // This disk holds more than the largest accepted reserve
            // (CI runners do), so no permitted reserve can exceed the
            // free space: the maximum reserve must pass instead.
            check_destination(&dir, Some(65536)).expect("max reserve fits this disk");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_destination_fails_for_unwritable_path() {
        // A path whose parent is a regular file cannot be created.
        let base = std::env::temp_dir().join(format!(
            "iptv-storage-blocked-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::write(&base, b"file").expect("seed file");
        let err =
            check_destination(&base.join("child"), Some(0)).expect_err("child of a file must fail");
        assert!(
            err.contains("The recording folder is unavailable or not writable"),
            "unexpected error: {err}"
        );
        let _ = std::fs::remove_file(&base);
    }

    // ----- R09: output has media -----

    #[test]
    fn output_has_media_cases() {
        let dir = std::env::temp_dir().join(format!(
            "iptv-media-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        let missing = dir.join("missing.ts");
        let empty = dir.join("empty.ts");
        let nonempty = dir.join("nonempty.ts");
        std::fs::write(&empty, b"").expect("empty");
        std::fs::write(&nonempty, b"x").expect("nonempty");
        assert!(!output_has_media(&missing));
        assert!(!output_has_media(&empty));
        assert!(output_has_media(&nonempty));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ----- R07: stop escalation -----

    #[cfg(not(windows))]
    fn stdin_cooperative_command() -> Vec<String> {
        vec![
            "sh".to_string(),
            "-c".to_string(),
            "read x; exit 0".to_string(),
        ]
    }

    #[cfg(not(windows))]
    fn sleeper_command() -> Vec<String> {
        vec!["sh".to_string(), "-c".to_string(), "sleep 30".to_string()]
    }

    #[cfg(not(windows))]
    #[test]
    fn stop_escalating_cooperative_process_succeeds_quickly() {
        let dir = std::env::temp_dir().join(format!("iptv-rec-esc-ok-{}", std::process::id()));
        let output = dir.join("coop.ts");
        let mut engine = RecordingEngine::new();
        engine
            .start("coop", &stdin_cooperative_command(), output.clone())
            .expect("start");
        let started = std::time::Instant::now();
        let finished = engine
            .stop_escalating("coop", std::time::Duration::from_secs(2))
            .expect("finished report");
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        assert_eq!(finished.key, "coop");
        assert_eq!(finished.out_path, output);
        assert!(finished.success, "graceful q exit must succeed");
        assert_eq!(engine.active_count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(not(windows))]
    #[test]
    fn stop_escalating_kills_stdin_ignoring_sleeper_after_grace() {
        let dir = std::env::temp_dir().join(format!("iptv-rec-esc-kill-{}", std::process::id()));
        let output = dir.join("sleeper.ts");
        let mut engine = RecordingEngine::new();
        engine
            .start("sleeper", &sleeper_command(), output)
            .expect("start");
        let started = std::time::Instant::now();
        let finished = engine
            .stop_escalating("sleeper", std::time::Duration::from_millis(200))
            .expect("finished report");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "kill path took {:?}",
            started.elapsed()
        );
        assert!(!finished.success, "a killed process reports false");
        assert_eq!(engine.active_count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(not(windows))]
    #[test]
    fn stop_escalating_unknown_key_returns_none() {
        let mut engine = RecordingEngine::new();
        assert!(engine
            .stop_escalating("nope", std::time::Duration::from_millis(10))
            .is_none());
    }

    #[cfg(not(windows))]
    #[test]
    fn shutdown_all_drains_two_sleepers() {
        let dir = std::env::temp_dir().join(format!("iptv-rec-shutdown-{}", std::process::id()));
        let mut engine = RecordingEngine::new();
        engine
            .start("one", &sleeper_command(), dir.join("one.ts"))
            .expect("start one");
        engine
            .start("two", &sleeper_command(), dir.join("two.ts"))
            .expect("start two");
        let started = std::time::Instant::now();
        let finished = engine.shutdown_all(std::time::Duration::from_millis(200));
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        assert_eq!(finished.len(), 2);
        assert!(finished.iter().all(|f| !f.success));
        assert_eq!(engine.active_count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn wait_for_finished(engine: &mut RecordingEngine) -> Vec<RecordingFinished> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let finished = engine.poll_finished();
            if !finished.is_empty() {
                return finished;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "recording process timed out"
            );
            std::thread::yield_now();
        }
    }

    #[cfg(windows)]
    fn test_process_command(exit_code: u8) -> Vec<String> {
        vec![
            "cmd.exe".to_string(),
            "/C".to_string(),
            format!("exit {exit_code}"),
        ]
    }

    #[cfg(not(windows))]
    fn test_process_command(exit_code: u8) -> Vec<String> {
        vec![
            "sh".to_string(),
            "-c".to_string(),
            format!("exit {exit_code}"),
        ]
    }
}
