//! Embedded subtitle tracks and recording chapter labels for
//! local Matroska recordings, ported from Python's
//! `recording_media.py` (with `clock_text` from
//! `media_navigation.py`, which the chapter labels use): the
//! ffmpeg stream-summary listing, text-track cue extraction,
//! the file-signature freshness guards, and the list labels.
//! Chapters themselves come from libVLC (`recording_media.chapters`
//! in Python); only their labels live here, next to the rows.

use std::path::Path;

use crate::subtitles::{parse_cues, Cue, MAX_FILE_BYTES};

/// Chrome of one embedded subtitle track, as ffmpeg's input
/// summary declares it (Python's `subtitle_tracks` rows).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmbeddedTrack {
    pub index: i32,
    pub language: String,
    pub codec: String,
    pub title: String,
    pub readable: bool,
    /// The listing's file signature, carried so cue extraction
    /// can refuse a file that changed since the list (Python
    /// stores it on every row).
    pub signature: FileSignature,
}

/// `(size, mtime_ns)` — the pair a library row's
/// `local-recording:{size}:{mtime_ns}` id is built from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileSignature {
    pub size: u64,
    pub mtime_ns: u128,
}

/// Codecs ffmpeg can render to text for speech (Python's `_TEXT`).
/// Anything else (image subtitles such as `hdvd_subtitle`) is
/// identified in the list and refused for speech.
const TEXT_CODECS: &[&str] = &["subrip", "srt", "ass", "ssa", "webvtt", "text", "mov_text"];

/// The file signature the freshness guards compare.
fn file_signature(path: &Path) -> Result<FileSignature, String> {
    let metadata = std::fs::metadata(path).map_err(|e| e.to_string())?;
    let mtime_ns = metadata
        .modified()
        .ok()
        .and_then(|when| when.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |duration: std::time::Duration| duration.as_nanos());
    Ok(FileSignature {
        size: metadata.len(),
        mtime_ns,
    })
}

/// The library id of a file with this signature
/// (`local-recording:{size}:{mtime_ns}`).
#[must_use]
pub fn recording_id(signature: FileSignature) -> String {
    format!("local-recording:{}:{}", signature.size, signature.mtime_ns)
}

// ---------------------------------------------------------------- ffmpeg input

/// The ffmpeg input arguments both operations share (Python's
/// `_input`): no stdin, a fixed Matroska demuxer, the file as a
/// local path, so a weird URL in a title cannot reach ffmpeg.
fn input_args(ffmpeg: &str, path: &Path) -> Result<Vec<String>, String> {
    let suffix = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_lowercase();
    if (suffix != "mkv" && suffix != "mka") || !path.is_file() {
        return Err("Open a local Matroska recording to read its embedded subtitles.".to_string());
    }
    let absolute = std::path::absolute(path).map_or_else(
        |_| path.to_string_lossy().into_owned(),
        |resolved| resolved.to_string_lossy().into_owned(),
    );
    Ok(vec![
        ffmpeg.to_string(),
        "-nostdin".to_string(),
        "-hide_banner".to_string(),
        "-protocol_whitelist".to_string(),
        "file".to_string(),
        "-f".to_string(),
        "matroska".to_string(),
        "-i".to_string(),
        absolute,
    ])
}

// ---------------------------------------------------------------- stream listing

/// The fields of one `Stream #0:N` summary line.
struct StreamLine {
    index: i32,
    language: String,
    kind: String,
    codec: String,
}

/// Match `^\s*Stream #0:(\d+)(?:\[[^]]*])?(?:\(([^)]*)\))?:\s*(\w+):\s*([\w]+)`
/// (Python's `_STREAM`) without a regex crate.
fn parse_stream_line(line: &str) -> Option<StreamLine> {
    let rest = line.trim_start();
    let rest = rest.strip_prefix("Stream #0:")?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    let index: i32 = digits.parse().ok()?;
    let mut cursor = &rest[digits.len()..];
    // An optional `[und]`-style handler tag.
    if let Some(bracketed) = cursor.strip_prefix('[') {
        let close = bracketed.find(']')?;
        cursor = &bracketed[close + 1..];
    }
    // The optional `(language)` group.
    let mut language = String::new();
    if let Some(parened) = cursor.strip_prefix('(') {
        let close = parened.find(')')?;
        language = parened[..close].to_string();
        cursor = &parened[close + 1..];
    }
    let cursor = cursor.strip_prefix(':')?.trim_start();
    // Kind is a word; the codec follows the first colon after it.
    let kind: String = cursor
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if kind.is_empty() {
        return None;
    }
    let after_kind = &cursor[kind.len()..];
    let after_colon = after_kind.strip_prefix(':')?.trim_start();
    let codec: String = after_colon
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    if codec.is_empty() {
        return None;
    }
    Some(StreamLine {
        index,
        language,
        kind,
        codec: codec.to_lowercase(),
    })
}

/// A `title: ...` metadata line under a stream (Python's title
/// regex, case-insensitive): the value, whitespace-collapsed
/// and capped at 240 characters.
fn parse_title_line(line: &str) -> Option<String> {
    // Python requires leading whitespace (`^\s+title\s*:\s*`).
    line.chars().next().filter(|c| c.is_whitespace())?;
    let trimmed = line.trim_start();
    if !trimmed
        .chars()
        .take(5)
        .collect::<String>()
        .eq_ignore_ascii_case("title")
    {
        return None;
    }
    let value = trimmed[5..].trim_start().strip_prefix(':')?.trim();
    if value.is_empty() {
        return None;
    }
    let collapsed: String = value.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(collapsed.chars().take(240).collect())
}

/// The subtitle tracks of an ffmpeg input summary (Python's
/// `subtitle_tracks` over its listing): index, language, codec,
/// title and speech readability, in first-seen order.
///
/// # Errors
/// `Could not read the recording's track information.` when
/// the summary has no input header.
pub fn parse_listing(
    listing: &str,
    signature: FileSignature,
) -> Result<Vec<EmbeddedTrack>, String> {
    if !listing.contains("Input #0,") {
        return Err("Could not read the recording's track information.".to_string());
    }
    let mut tracks: Vec<EmbeddedTrack> = Vec::new();
    let mut current: Option<usize> = None;
    for line in listing.lines() {
        if let Some(stream) = parse_stream_line(line) {
            current = None;
            if stream.kind == "Subtitle" {
                let track = EmbeddedTrack {
                    index: stream.index,
                    language: stream.language,
                    codec: stream.codec.clone(),
                    title: String::new(),
                    readable: TEXT_CODECS.contains(&stream.codec.as_str()),
                    signature,
                };
                // Python assigns into a dict keyed by index: a
                // repeat replaces the row but keeps its position.
                if let Some(existing) = tracks.iter().position(|t| t.index == track.index) {
                    tracks[existing] = track;
                    current = Some(existing);
                } else {
                    tracks.push(track);
                    current = Some(tracks.len() - 1);
                }
            }
        } else if let Some(at) = current {
            if let Some(title) = parse_title_line(line) {
                tracks[at].title = title;
            }
        }
    }
    Ok(tracks)
}

// ---------------------------------------------------------------- process runner

/// Drain one pipe in 64 KiB chunks until the child closes it
/// or the cap passes (Python's `collect` thread).
fn spawn_reader(
    pipe: impl std::io::Read + Send + 'static,
    limit: usize,
) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut pipe = pipe;
        let mut output = Vec::new();
        let mut chunk = vec![0_u8; 65536];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(count) => {
                    output.extend_from_slice(&chunk[..count]);
                    if output.len() > limit {
                        break;
                    }
                }
            }
        }
        output
    })
}

/// Read a child process's output with Python's bounds
/// (`_read_process`): a size cap (1 MiB for the listing, the cue
/// file cap for extraction), a deadline (20 s / 180 s), and
/// cancellation that kills the child and yields an empty read.
/// The listing merges stderr into stdout; extraction's goes
/// nowhere.
fn read_process(
    args: &[String],
    canceled: &impl Fn() -> bool,
    listing: bool,
) -> Result<Vec<u8>, String> {
    if canceled() {
        return Ok(Vec::new());
    }
    let limit = if listing { 1024 * 1024 } else { MAX_FILE_BYTES };
    let mut command = crate::hidden_command(&args[0]);
    command
        .args(&args[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(if listing {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        });
    // Python merges stderr into the listing's pipe
    // (`stderr=subprocess.STDOUT`); both pipes are read fully
    // here and concatenated, so the summary is detectable the
    // same way.
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    let mut stderr_merge = None;
    if listing {
        // ffmpeg `-i` prints the summary to stderr. Read both
        // pipes fully: stderr separately, then appended, which
        // keeps "Input #0," detectable as in the merged stream.
        stderr_merge = child.stderr.take();
    }
    let stdout = child.stdout.take().map(|pipe| spawn_reader(pipe, limit));
    let stderr_merge_reader = stderr_merge.map(|pipe| spawn_reader(pipe, limit));
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(if listing { 20 } else { 180 });
    let mut timed_out = false;
    loop {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => break,
            Ok(None) => {
                if canceled() {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok(Vec::new());
                }
                if std::time::Instant::now() >= deadline {
                    timed_out = true;
                    let _ = child.kill();
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
    let status = child.wait().ok();
    let mut output = stdout
        .and_then(|reader| reader.join().ok())
        .unwrap_or_default();
    if let Some(stderr_output) = stderr_merge_reader.and_then(|reader| reader.join().ok()) {
        output.extend_from_slice(&stderr_output);
    }
    if timed_out {
        return Err("Reading the embedded subtitles timed out.".to_string());
    }
    if output.len() > limit {
        return Err("The subtitle data is too large to read.".to_string());
    }
    if !listing && !status.is_some_and(|status| status.success()) {
        return Err("Could not read this subtitle track.".to_string());
    }
    Ok(output)
}

// ---------------------------------------------------------------- public operations

/// The embedded subtitle tracks of a local Matroska recording
/// (Python's `subtitle_tracks`): lists with ffmpeg, guarding
/// the file's signature and expiry of the library id.
///
/// # Errors
/// Python's exact sentences for every refusal: a non-Matroska
/// path, an expired library id, a changed file, an unreadable
/// summary, a timeout, or an over-limit read.
pub fn subtitle_tracks(
    ffmpeg: &str,
    path: &Path,
    canceled: &impl Fn() -> bool,
    recording_id: &str,
) -> Result<Vec<EmbeddedTrack>, String> {
    if canceled() {
        return Ok(Vec::new());
    }
    let args = input_args(ffmpeg, path)?;
    let signature = file_signature(path)?;
    if recording_id.starts_with("local-recording:") && recording_id != recording_id_from(&signature)
    {
        return Err("The recording file changed. Refresh the library.".to_string());
    }
    let data = read_process(&args, canceled, true)?;
    if canceled() {
        return Ok(Vec::new());
    }
    if file_signature(path)? != signature {
        return Err("The recording file changed. Refresh the library.".to_string());
    }
    let listing = String::from_utf8_lossy(&data).into_owned();
    parse_listing(&listing, signature)
}

fn recording_id_from(signature: &FileSignature) -> String {
    recording_id(*signature)
}

/// The speech cues of one embedded track (Python's
/// `embedded_cues`): image tracks and out-of-range indexes are
/// refused, the file's signature is re-checked, and the track
/// is extracted to ASS (copied for ASS, converted otherwise)
/// and parsed.
///
/// # Errors
/// Python's exact sentences: an unreadable track, a changed
/// file, a timeout, an over-limit read, or a failed
/// extraction.
pub fn embedded_cues(
    ffmpeg: &str,
    path: &Path,
    track: &EmbeddedTrack,
    canceled: &impl Fn() -> bool,
) -> Result<Vec<Cue>, String> {
    if canceled() {
        return Ok(Vec::new());
    }
    if !track.readable || track.index < 0 {
        return Err("This subtitle track cannot be read aloud.".to_string());
    }
    if file_signature(path)? != track.signature {
        return Err("The recording file changed. Refresh the library.".to_string());
    }
    let ass = matches!(track.codec.as_str(), "ass" | "ssa");
    let format = if ass { "ass" } else { "srt" };
    let mut args = input_args(ffmpeg, path)?;
    args.extend([
        "-loglevel".to_string(),
        "error".to_string(),
        "-map".to_string(),
        format!("0:{}", track.index),
        "-c:s".to_string(),
        if ass {
            "copy".to_string()
        } else {
            "srt".to_string()
        },
        "-f".to_string(),
        format.to_string(),
        "pipe:1".to_string(),
    ]);
    let data = read_process(&args, canceled, false)?;
    if canceled() {
        return Ok(Vec::new());
    }
    if file_signature(path)? != track.signature {
        return Err("The recording file changed. Refresh the library.".to_string());
    }
    let text = String::from_utf8_lossy(&data)
        .trim_start_matches('\u{feff}')
        .to_string();
    Ok(parse_cues(&text, format))
}

// ---------------------------------------------------------------- chapters

/// A recording chapter as libVLC reports it (Python's
/// `chapters` rows): index, start and name. The engine reads
/// them; this module owns the label the choice dialog shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChapterRow {
    pub index: i32,
    pub start_ms: i64,
    pub name: String,
}

/// The chapter the labels need: a blank name falls back to the
/// row's number, never to nothing (Python's `Chapter {number}`
/// format, translated by the caller).
#[must_use]
pub fn chapter_display_name(row: &ChapterRow) -> Option<String> {
    let trimmed = row.name.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// `HH:MM:SS`, clamped at zero (Python's `clock_text`).
#[must_use]
pub fn clock_text(milliseconds: i64) -> String {
    let seconds = milliseconds.max(0) / 1000;
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let secs = seconds % 60;
    format!("{hours:02}:{minutes:02}:{secs:02}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn listing_sample() -> &'static str {
        "Input #0, matroska,webm from '/x.mkv':\n  Duration: 00:00:05.00, start: 0.000000, bitrate: 100 kb/s\n  Stream #0:0: Audio: pcm_s16le, 44100 Hz, stereo, s16, 1411 kb/s\n  Stream #0:1(und): Subtitle: subrip\n    title: Magyar\n  Stream #0:2(eng): Subtitle: subrip\n    title: English\n  Stream #0:3: Subtitle: hdvd_subtitle\n"
    }
    fn test_signature() -> FileSignature {
        FileSignature {
            size: 42,
            mtime_ns: 7,
        }
    }

    #[test]
    fn listing_reads_language_title_and_readability() {
        let tracks = parse_listing(listing_sample(), test_signature()).unwrap();
        assert_eq!(tracks.len(), 3);
        assert_eq!(tracks[0].index, 1);
        assert_eq!(tracks[0].language, "und");
        assert_eq!(tracks[0].title, "Magyar");
        assert!(tracks[0].readable);
        assert_eq!(tracks[1].language, "eng");
        assert_eq!(tracks[1].title, "English");
        // Image subtitles are identified but not speakable.
        assert!(!tracks[2].readable);
        assert_eq!(tracks[2].language, "");
    }

    #[test]
    fn listing_title_does_not_cross_streams() {
        let listing = "Input #0, matroska from 'x':\n  Stream #0:0: Audio: pcm_s16le\n    title: Audio Title\n  Stream #0:1: Subtitle: subrip\n";
        let tracks = parse_listing(listing, test_signature()).unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].title, "", "an audio title must not leak");
    }

    #[test]
    fn listing_needs_an_input_header() {
        assert_eq!(
            parse_listing("ffmpeg version 6.1\n", test_signature()),
            Err("Could not read the recording's track information.".to_string())
        );
    }

    #[test]
    fn listing_without_subtitles_is_empty_not_an_error() {
        let listing = "Input #0, matroska from 'x':\n  Stream #0:0: Video: h264\n  Stream #0:1: Audio: pcm_s16le\n";
        assert_eq!(
            parse_listing(listing, test_signature()).unwrap(),
            Vec::new()
        );
    }

    #[test]
    fn stream_line_without_language_or_bracket() {
        // `Stream #0:4: Subtitle: srt` and `[und]` handlers parse too.
        let plain = parse_stream_line("  Stream #0:4: Subtitle: srt");
        assert!(plain.is_some());
        let plain = plain.unwrap();
        assert_eq!(plain.language, "");
        assert_eq!(plain.kind, "Subtitle");
        let handler = parse_stream_line("  Stream #0:5[und]: Subtitle: ass");
        assert!(handler.is_some());
        let handler = handler.unwrap();
        assert_eq!(handler.index, 5);
        assert_eq!(handler.codec, "ass");
        assert!(parse_stream_line("  Stream #1:0: Subtitle: srt").is_none());
        assert!(parse_stream_line("  Duration: 00:01:00").is_none());
    }

    #[test]
    fn title_is_collapsed_and_capped_like_python() {
        let long = "A ".repeat(200);
        let listing = format!(
            "Input #0, matroska from 'x':\n  Stream #0:1: Subtitle: subrip\n    TITLE:   {long}  tail\n"
        );
        let tracks = parse_listing(&listing, test_signature()).unwrap();
        assert_eq!(tracks[0].title.chars().count(), 240);
    }

    #[test]
    fn recording_id_matches_the_library_id_shape() {
        assert_eq!(
            recording_id(test_signature()),
            "local-recording:42:7".to_string()
        );
    }

    #[test]
    fn read_embedded_guards_need_a_matroska_file() {
        let missing = Path::new("/nonexistent/file.mkv");
        assert_eq!(
            subtitle_tracks("ffmpeg", missing, &|| false, ""),
            Err("Open a local Matroska recording to read its embedded subtitles.".to_string())
        );
        assert_eq!(
            subtitle_tracks("ffmpeg", Path::new("clip.mp4"), &|| false, ""),
            Err("Open a local Matroska recording to read its embedded subtitles.".to_string())
        );
        // Cancellation wins over the file checks, as in Python.
        assert_eq!(
            subtitle_tracks("ffmpeg", Path::new("clip.mp4"), &|| true, ""),
            Ok(Vec::new())
        );
    }

    #[test]
    fn clock_text_matches_the_reference() {
        assert_eq!(clock_text(0), "00:00:00");
        assert_eq!(clock_text(61_000), "00:01:01");
        assert_eq!(clock_text(3_661_000), "01:01:01");
        assert_eq!(clock_text(86_399_000), "23:59:59");
        assert_eq!(clock_text(-12_345), "00:00:00");
    }

    #[test]
    fn chapter_name_falls_back_to_the_number() {
        let row = ChapterRow {
            index: 2,
            start_ms: 2_500,
            name: String::new(),
        };
        assert_eq!(chapter_display_name(&row), None);
        let named = ChapterRow {
            name: "Finálé".to_string(),
            ..row
        };
        assert_eq!(chapter_display_name(&named), Some("Finálé".to_string()));
    }
}
