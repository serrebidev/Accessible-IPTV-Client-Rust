//! Wx-free recording plan. No process or filesystem mutation occurs here.
//!
//! Format and `FFmpeg` behavior follows Python `recorder.py` at
//! `9aa89e053225ef2c83c4f3659775a00723012ff7`. Paths are candidates:
//! callers must reserve the chosen name before starting `FFmpeg`.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::playback::{split_stream_modifiers, StreamHeaders};
use crate::Channel;

const DEFAULT_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/123.0.0.0 Safari/537.36";

/// Stable format metadata used by both recording callers and format pickers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecordingFormat {
    pub key: &'static str,
    pub label: &'static str,
    pub extension: &'static str,
    pub kind: &'static str,
    pub muxer: &'static str,
    pub faststart: bool,
    /// Output mapping, dispositions, and codec options before selected-track policy.
    pub ffmpeg_options: &'static [&'static str],
}

const PROVIDER_MKV: &[&str] = &["-map", "0", "-c", "copy"];
const PROVIDER_MP4: &[&str] = &[
    "-map", "0:v?", "-map", "0:a?", "-dn", "-sn", "-c:v", "copy", "-c:a", "aac", "-b:a", "192k",
];
const X264: &[&str] = &[
    "-map", "0:v?", "-map", "0:a?", "-c:v", "libx264", "-preset", "veryfast", "-crf", "21",
    "-pix_fmt", "yuv420p", "-c:a", "aac", "-b:a", "192k",
];
const PROVIDER_MKA: &[&str] = &["-map", "0:a?", "-vn", "-sn", "-dn", "-c:a", "copy"];
const MP3: &[&str] = &["-vn", "-c:a", "libmp3lame", "-q:a", "0"];
const FLAC: &[&str] = &["-vn", "-c:a", "flac"];
const WAV: &[&str] = &["-vn", "-c:a", "pcm_s16le"];
const AAC: &[&str] = &["-vn", "-c:a", "aac", "-b:a", "256k"];
const OPUS: &[&str] = &["-vn", "-c:a", "libopus", "-b:a", "160k"];

pub const FORMATS: [RecordingFormat; 10] = [
    RecordingFormat {
        key: "provider_mkv",
        label: "Provider quality (copy, MKV)",
        extension: "mkv",
        kind: "video",
        muxer: "matroska",
        faststart: false,
        ffmpeg_options: PROVIDER_MKV,
    },
    RecordingFormat {
        key: "provider_mp4",
        label: "Provider quality (copy, MP4)",
        extension: "mp4",
        kind: "video",
        muxer: "mp4",
        faststart: true,
        ffmpeg_options: PROVIDER_MP4,
    },
    RecordingFormat {
        key: "x264_mkv",
        label: "x264 re-encode (MKV)",
        extension: "mkv",
        kind: "video",
        muxer: "matroska",
        faststart: false,
        ffmpeg_options: X264,
    },
    RecordingFormat {
        key: "x264_mp4",
        label: "x264 re-encode (MP4)",
        extension: "mp4",
        kind: "video",
        muxer: "mp4",
        faststart: true,
        ffmpeg_options: X264,
    },
    RecordingFormat {
        key: "provider_mka",
        label: "Provider quality (stream copy, MKA)",
        extension: "mka",
        kind: "audio",
        muxer: "matroska",
        faststart: false,
        ffmpeg_options: PROVIDER_MKA,
    },
    RecordingFormat {
        key: "audio_mp3_v0",
        label: "Audio only (MP3 V0)",
        extension: "mp3",
        kind: "audio",
        muxer: "mp3",
        faststart: false,
        ffmpeg_options: MP3,
    },
    RecordingFormat {
        key: "audio_flac",
        label: "Audio only (FLAC)",
        extension: "flac",
        kind: "audio",
        muxer: "flac",
        faststart: false,
        ffmpeg_options: FLAC,
    },
    RecordingFormat {
        key: "audio_wav",
        label: "Audio only (WAV)",
        extension: "wav",
        kind: "audio",
        muxer: "wav",
        faststart: false,
        ffmpeg_options: WAV,
    },
    RecordingFormat {
        key: "audio_aac_m4a",
        label: "Audio only (AAC, M4A)",
        extension: "m4a",
        kind: "audio",
        muxer: "ipod",
        faststart: false,
        ffmpeg_options: AAC,
    },
    RecordingFormat {
        key: "audio_opus",
        label: "Audio only (Opus)",
        extension: "opus",
        kind: "audio",
        muxer: "oga",
        faststart: false,
        ffmpeg_options: OPUS,
    },
];

#[must_use]
pub fn recording_format(key: &str) -> &'static RecordingFormat {
    FORMATS
        .iter()
        .find(|format| format.key == key)
        .unwrap_or(&FORMATS[0])
}

/// `FFmpeg` options chosen by the caller; no UI or process state is stored.
#[derive(Clone, Copy, Debug, Default)]
pub struct RecordingOptions<'a> {
    pub format: &'a str,
    pub duration_seconds: Option<f64>,
    pub show_stats: bool,
    pub log_datetime: bool,
    pub audio_track: Option<usize>,
    pub audio_track_count: usize,
}

/// The raw argv contains stream credentials. Never log or announce it.
pub struct RecordingPlan {
    pub format: &'static RecordingFormat,
    pub output_path: PathBuf,
    pub part_path: PathBuf,
    pub argv: Vec<String>,
    pub display_label: String,
}

impl fmt::Debug for RecordingPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecordingPlan")
            .field("format", &self.format.key)
            .field("display_label", &self.display_label)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlanError {
    MissingStream,
    InvalidTimestamp,
}

/// Python's invalid-character, whitespace, and 120-character rules, plus a
/// Windows device-name guard so the resulting path remains usable on Windows.
#[must_use]
pub fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
                ' '
            } else {
                c
            }
        })
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim_matches([' ', '.']);
    let base = if trimmed.is_empty() {
        "Recording"
    } else {
        trimmed
    };
    let mut result: String = base
        .chars()
        .take(120)
        .collect::<String>()
        .trim_end_matches([' ', '.'])
        .to_string();
    if result.is_empty() {
        result = "Recording".to_string();
    }
    if windows_reserved(&result) {
        let stem_end = result.find('.').unwrap_or(result.len());
        if result.chars().count() == 120 {
            result.pop();
        }
        result.insert(stem_end.min(result.len()), '_');
    }
    result
}

fn windows_reserved(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    matches!(
        stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

/// Build one candidate plan. `exists` observes completed and `.part` paths;
/// `reserved` names covers concurrent starts whose files do not exist yet.
/// The caller must serialize reservation with the same observations.
///
/// # Errors
/// Returns `MissingStream` for an empty stream URL or `InvalidTimestamp` for
/// a timestamp outside the live/catch-up filename forms.
pub fn plan_recording(
    channel: &Channel,
    output_dir: &Path,
    timestamp: &str,
    options: RecordingOptions<'_>,
    reserved: &[PathBuf],
    exists: impl Fn(&Path) -> bool,
) -> Result<RecordingPlan, PlanError> {
    let (url, _) = split_stream_modifiers(&channel.url);
    if url.is_empty() {
        return Err(PlanError::MissingStream);
    }
    if !valid_timestamp(timestamp) {
        return Err(PlanError::InvalidTimestamp);
    }
    let format = recording_format(options.format);
    let headers = StreamHeaders::for_channel(channel);
    let name = safe_display_name(channel, &headers);
    let base = sanitize_filename(&name);
    let mut suffix = 1;
    let output_path = loop {
        let suffix_text = if suffix == 1 {
            String::new()
        } else {
            format!(" ({suffix})")
        };
        let candidate = output_dir.join(format!(
            "{base} - {timestamp}{suffix_text}.{}",
            format.extension
        ));
        let part = part_path(&candidate);
        if !exists(&candidate)
            && !exists(&part)
            && !reserved
                .iter()
                .any(|path| path_eq(path, &candidate) || path_eq(path, &part))
        {
            break candidate;
        }
        suffix += 1;
    };
    let part_path = part_path(&output_path);
    let argv = ffmpeg_argv(&url, &headers, &part_path, format, options);
    Ok(RecordingPlan {
        format,
        output_path,
        part_path,
        argv,
        display_label: base,
    })
}

fn valid_timestamp(timestamp: &str) -> bool {
    let bytes = timestamp.as_bytes();
    (bytes.len() == 16 || bytes.len() == 19)
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            4 | 7 | 13 | 16 => *byte == b'-',
            10 => *byte == b' ',
            _ => byte.is_ascii_digit(),
        })
}

fn safe_display_name(channel: &Channel, headers: &StreamHeaders) -> String {
    let name = crate::favorites::display_name(channel);
    let contains_header_value = headers
        .entries
        .iter()
        .any(|(_, value)| !value.is_empty() && name.contains(value))
        || headers
            .extra
            .iter()
            .filter_map(|line| line.split_once(':'))
            .any(|(_, value)| {
                let value = value.trim();
                !value.is_empty() && name.contains(value)
            });
    if name.contains("://") || name.contains('|') || contains_header_value {
        "Recording".to_string()
    } else {
        name.to_string()
    }
}

fn part_path(output: &Path) -> PathBuf {
    let mut part = output.as_os_str().to_os_string();
    part.push(".part");
    PathBuf::from(part)
}

fn path_eq(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a.to_string_lossy()
            .eq_ignore_ascii_case(&b.to_string_lossy())
    } else {
        a == b
    }
}

fn ffmpeg_argv(
    url: &str,
    headers: &StreamHeaders,
    part: &Path,
    format: &RecordingFormat,
    options: RecordingOptions<'_>,
) -> Vec<String> {
    let mut args = vec![
        "ffmpeg".into(),
        "-hide_banner".into(),
        "-loglevel".into(),
        if options.log_datetime {
            "level+datetime+info"
        } else {
            "level+info"
        }
        .into(),
        "-n".into(),
    ];
    if options.show_stats {
        args.extend(["-stats_period".into(), "1".into()]);
    } else {
        args.push("-nostats".into());
    }
    args.extend(["-rw_timeout".into(), "15000000".into()]);
    if url.to_ascii_lowercase().starts_with("http://")
        || url.to_ascii_lowercase().starts_with("https://")
    {
        args.extend([
            "-reconnect".into(),
            "1".into(),
            "-reconnect_streamed".into(),
            "1".into(),
            "-reconnect_delay_max".into(),
            "5".into(),
        ]);
        let user_agent = headers.get("User-Agent").unwrap_or(DEFAULT_USER_AGENT);
        args.extend(["-user_agent".into(), user_agent.into()]);
        if let Some(referer) = headers.get("Referer") {
            args.extend(["-referer".into(), referer.into()]);
        }
        let mut lines = Vec::new();
        let mut seen = BTreeSet::from(["user-agent".to_string(), "referer".to_string()]);
        for (name, value) in &headers.entries {
            if !value.is_empty() && seen.insert(name.to_ascii_lowercase()) {
                lines.push(format!("{name}: {value}\r\n"));
            }
        }
        for line in &headers.extra {
            if let Some((name, value)) = line.split_once(':') {
                if !name.trim().is_empty()
                    && !value.trim().is_empty()
                    && seen.insert(name.trim().to_ascii_lowercase())
                {
                    lines.push(format!("{}: {}\r\n", name.trim(), value.trim()));
                }
            }
        }
        if !lines.is_empty() {
            args.extend(["-headers".into(), lines.concat()]);
        }
    } else if url.to_ascii_lowercase().starts_with("rtsp://") {
        args.extend(["-rtsp_transport".into(), "tcp".into()]);
    }
    args.extend(["-i".into(), url.into()]);
    if let Some(seconds) = options
        .duration_seconds
        .filter(|s| s.is_finite() && *s > 0.0)
    {
        args.extend(["-t".into(), format!("{seconds:?}")]);
    }
    if format.key.starts_with("audio_") {
        if let Some(track) = options.audio_track {
            extend_args(&mut args, &["-map", &format!("0:a:{track}")]);
        }
    }
    extend_args(&mut args, format.ffmpeg_options);
    if format.kind == "video" {
        if let Some(track) = options.audio_track {
            for index in 0..options.audio_track_count.max(track + 1) {
                args.extend([
                    format!("-disposition:a:{index}"),
                    if index == track {
                        "+default"
                    } else {
                        "-default"
                    }
                    .into(),
                ]);
            }
        }
    }
    if format.faststart {
        extend_args(&mut args, &["-movflags", "+faststart"]);
    }
    args.extend([
        "-f".into(),
        format.muxer.into(),
        part.to_string_lossy().into_owned(),
    ]);
    args
}

fn extend_args(args: &mut Vec<String>, parts: &[&str]) {
    args.extend(parts.iter().map(|part| (*part).to_string()));
}
