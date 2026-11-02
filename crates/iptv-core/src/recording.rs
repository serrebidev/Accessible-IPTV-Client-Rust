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
fn header_input_args(headers: &StreamHeaders) -> Vec<String> {
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

/// Free bytes on the filesystem holding `path`, when the
/// platform can report it (Unix `df`; `None` elsewhere, which
/// skips the gate).
#[must_use]
pub fn free_space_bytes(path: &std::path::Path) -> Option<u64> {
    if !cfg!(unix) {
        return None;
    }
    let output = std::process::Command::new("df")
        .args(["-B1", "--output=avail"])
        .arg(path)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines().nth(1)?.trim().parse().ok()
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
}
