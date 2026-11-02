//! Playback seam: stream headers, external player commands, launch
//! debounce, the session state machine, and resume checkpoints.
//!
//! Ports the Python rules that decide *what* plays and *how it is asked*:
//! `http_headers.py` (pipe modifiers and attribute headers),
//! `external_player.py` (argv shapes, 0.75 s same-stream debounce,
//! generation supersede), the agents.md buffer law (half-second start
//! cache growing to the saved buffer; playback rate is never touched),
//! and `playback_resume.py` checkpoint rules. Engines sit behind this
//! seam; the core never spawns processes or opens sockets.

use std::collections::BTreeMap;

use crate::config::InternalPlayerSettings;
use crate::Channel;

/// Canonical headers for a channel: pipe modifiers on the URL fill the
/// gaps left by the attribute headers (Python `merge_headers`: base wins).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StreamHeaders {
    pub entries: Vec<(String, String)>,
    /// Non-canonical modifiers, kept as ready `Name: value` lines.
    pub extra: Vec<String>,
}

fn canonical_alias(key: &str) -> Option<&'static str> {
    match key {
        "user-agent" | "ua" | "http-user-agent" => Some("User-Agent"),
        "referer" | "referrer" | "http-referrer" | "http-referer" => Some("Referer"),
        "origin" | "http-origin" => Some("Origin"),
        "cookie" | "http-cookie" => Some("Cookie"),
        "authorization" | "auth" | "http-authorization" => Some("Authorization"),
        "x-forwarded-for" | "xff" => Some("X-Forwarded-For"),
        "accept" | "http-accept" => Some("Accept"),
        "range" | "http-range" => Some("Range"),
        _ => None,
    }
}

fn title_case_header(key: &str) -> String {
    key.split('-')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            chars.next().map_or(String::new(), |first| {
                first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase()
            })
        })
        .collect::<Vec<_>>()
        .join("-")
}

fn apply_modifier(headers: &mut StreamHeaders, key: &str, value: &str, fill_only: bool) {
    if value.is_empty() {
        return;
    }
    if let Some(name) = canonical_alias(key) {
        let present = headers.entries.iter().any(|(n, _)| n == name);
        if !fill_only || !present {
            headers.entries.retain(|(n, _)| n != name);
            headers.entries.push((name.to_string(), value.to_string()));
        }
    } else if key == "bearer" || key == "token" {
        apply_modifier(
            headers,
            "authorization",
            &format!("Bearer {value}"),
            fill_only,
        );
    } else if key == "host" || key == "http-host" {
        headers.extra.push(format!("Host: {value}"));
    } else {
        headers
            .extra
            .push(format!("{}: {value}", title_case_header(key)));
    }
}

/// Split `url|Key=Value|...` into the clean URL and its modifiers
/// (Python `split_stream_modifiers`).
#[must_use]
pub fn split_stream_modifiers(url: &str) -> (String, StreamHeaders) {
    let mut headers = StreamHeaders::default();
    let Some((base, tail)) = url.split_once('|') else {
        return (url.trim().to_string(), headers);
    };
    for part in tail.split('|') {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        let value = decode_modifier_value(value.trim());
        apply_modifier(
            &mut headers,
            key.trim().to_lowercase().as_str(),
            value.trim(),
            false,
        );
    }
    (base.trim().to_string(), headers)
}

fn decode_modifier_value(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_digit(bytes[index + 1]), hex_digit(bytes[index + 2]))
            {
                decoded.push((hi << 4) | lo);
                index += 3;
                continue;
            }
        }
        decoded.push(if bytes[index] == b'+' {
            b' '
        } else {
            bytes[index]
        });
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

impl StreamHeaders {
    /// Headers for a channel: `http-user-agent` / `http-referrer` style
    /// attributes form the base; the URL's pipe modifiers fill gaps.
    #[must_use]
    pub fn for_channel(channel: &Channel) -> Self {
        let mut headers = Self::default();
        for (attr, alias) in [
            ("http-user-agent", "user-agent"),
            ("http-referrer", "referer"),
            ("http-referer", "referer"),
            ("http-origin", "origin"),
            ("http-cookie", "cookie"),
            ("http-authorization", "authorization"),
            ("http-accept", "accept"),
        ] {
            if let Some(value) = channel.unknown_attributes.get(attr) {
                apply_modifier(&mut headers, alias, value.trim(), false);
            }
        }
        let (_, modifiers) = split_stream_modifiers(&channel.url);
        for (name, value) in &modifiers.entries {
            let present = headers.entries.iter().any(|(n, _)| n == name);
            if !present {
                headers.entries.push((name.clone(), value.clone()));
            }
        }
        headers.extra.extend(modifiers.extra);
        if let Some(extra) = channel.unknown_attributes.get("http-headers") {
            if let Some((name, value)) = extra.split_once(':') {
                let name = name.trim();
                let value = value.trim();
                if !name.is_empty() && !value.is_empty() {
                    headers.extra.push(format!("{name}: {value}"));
                }
            }
        }
        headers
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// What an engine is asked to play.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlaybackRequest {
    pub url: String,
    pub title: String,
    pub headers: StreamHeaders,
    pub resume_ms: Option<i64>,
    pub live: bool,
}

impl PlaybackRequest {
    #[must_use]
    pub fn for_channel(channel: &Channel, resume_ms: Option<i64>) -> Self {
        let (url, _) = split_stream_modifiers(&channel.url);
        let live = !is_vod_url(&url);
        Self {
            url,
            title: crate::favorites::display_name(channel).to_string(),
            headers: StreamHeaders::for_channel(channel),
            resume_ms,
            live,
        }
    }
}

/// VOD heuristic: file-like stream URLs are seekable content, everything
/// else is treated as a live stream (reconnectable, not resumable).
fn is_vod_url(url: &str) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or(url).to_lowercase();
    [".mp4", ".mkv", ".avi", ".mov", ".webm"]
        .iter()
        .any(|ext| path.ends_with(ext))
        || path.contains("/movie/")
        || path.contains("/series/")
}

/// The buffer law: start at half a second so playback begins fast, then
/// grow to the user's saved buffer. Engines that cannot set a cache use
/// the target only; the rate is never part of this contract.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StartCache {
    pub initial_ms: u32,
    pub target_ms: u32,
    pub max_ms: u32,
}

// Config normalization bounds buffers to 0..=300 s, so the millisecond
// products fit u32 comfortably; the casts cannot lose range in practice.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
#[must_use]
pub fn start_cache(settings: &InternalPlayerSettings) -> StartCache {
    StartCache {
        initial_ms: 500,
        target_ms: (settings.buffer_seconds * 1000.0) as u32,
        max_ms: (settings.max_buffer_seconds * 1000.0) as u32,
    }
}

/// The configured player choice (`media_player` config values).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlayerChoice {
    BuiltIn,
    Named(String),
    Custom(String),
}

#[must_use]
pub fn player_choice(media_player: &str, custom_path: &str) -> PlayerChoice {
    match media_player {
        "Built-in Player" => PlayerChoice::BuiltIn,
        "Custom" => PlayerChoice::Custom(custom_path.to_string()),
        other => PlayerChoice::Named(other.to_string()),
    }
}

/// Detect a known player from a custom executable path (Python heuristic).
#[must_use]
pub fn detect_from_path(path: &str) -> String {
    let base = path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .to_lowercase();
    if base.contains("mpv") {
        "MPV".to_string()
    } else if base.contains("vlc") {
        "VLC".to_string()
    } else if base.contains("mpc-be") || base.contains("mpcbe") {
        "MPC-BE".to_string()
    } else if base.starts_with("mpc-hc") || base.starts_with("mpc") {
        "MPC".to_string()
    } else {
        "Custom".to_string()
    }
}

/// Command line for an external player (Python `_argv_for`): VLC replaces
/// through one-instance flags, MPV gets its IPC/idle flags, MPC and
/// everything else take the bare URL.
#[must_use]
pub fn external_argv(player_name: &str, exe: &str, url: &str, mpv_ipc_path: &str) -> Vec<String> {
    match player_name {
        "VLC" => vec![
            exe.to_string(),
            "--one-instance".to_string(),
            "--one-instance-when-started-from-file".to_string(),
            url.to_string(),
        ],
        "MPV" => vec![
            exe.to_string(),
            format!("--input-ipc-server={mpv_ipc_path}"),
            "--force-window=yes".to_string(),
            "--idle=yes".to_string(),
            "--no-terminal".to_string(),
            url.to_string(),
        ],
        _ => vec![exe.to_string(), url.to_string()],
    }
}

/// Well-known install locations / commands for the headline players.
/// The Python tables cover a long tail of niche players; those fall back
/// to a same-named command on POSIX and "not located" on Windows, as the
/// reference reports them.
#[must_use]
pub fn candidate_executables(player_name: &str, windows: bool, macos: bool) -> Vec<String> {
    // Python external_player.py tables, verbatim: a player with no
    // entry for the platform yields no candidates (the adapter then
    // reports it could not be located / is not configured there).
    if windows {
        return windows_candidates(player_name);
    }
    if macos {
        return macos_candidates(player_name);
    }
    linux_candidates(player_name)
}

fn windows_candidates(player_name: &str) -> Vec<String> {
    {
        let paths: &[&str] = match player_name {
            "VLC" => &[
                r"C:\Program Files\VideoLAN\VLC\vlc.exe",
                r"C:\Program Files (x86)\VideoLAN\VLC\vlc.exe",
            ],
            "MPC" => &[
                r"C:\Program Files\MPC-HC\mpc-hc64.exe",
                r"C:\Program Files (x86)\K-Lite Codec Pack\MPC-HC64\mpc-hc64.exe",
                r"C:\Program Files (x86)\MPC-HC\mpc-hc.exe",
            ],
            "MPC-BE" => &[
                r"C:\Program Files\MPC-BE\mpc-be64.exe",
                r"C:\Program Files (x86)\MPC-BE\mpc-be.exe",
                r"C:\Program Files\MPC-BE x64\mpc-be64.exe",
                r"C:\Program Files\MPC-BE\mpc-be.exe",
            ],
            "Kodi" => &[r"C:\Program Files\Kodi\kodi.exe"],
            "Winamp" => &[r"C:\Program Files\Winamp\winamp.exe"],
            "Foobar2000" => &[r"C:\Program Files\foobar2000\foobar2000.exe"],
            "MPV" => &[
                r"C:\Program Files\mpv\mpv.exe",
                r"C:\Program Files (x86)\mpv\mpv.exe",
            ],
            "SMPlayer" => &[
                r"C:\Program Files\SMPlayer\smplayer.exe",
                r"C:\Program Files (x86)\SMPlayer\smplayer.exe",
            ],
            "QuickTime" => &[
                r"C:\Program Files\QuickTime\QuickTimePlayer.exe",
                r"C:\Program Files (x86)\QuickTime\QuickTimePlayer.exe",
            ],
            "iTunes/Apple Music" => &[
                r"C:\Program Files\iTunes\iTunes.exe",
                r"C:\Program Files (x86)\iTunes\iTunes.exe",
                r"C:\Program Files\Apple\Music\AppleMusic.exe",
            ],
            "PotPlayer" => &[
                r"C:\Program Files\DAUM\PotPlayer\PotPlayerMini64.exe",
                r"C:\Program Files\DAUM\PotPlayer\PotPlayerMini.exe",
            ],
            "KMPlayer" => &[
                r"C:\Program Files\KMP Media\KMPlayer\KMPlayer64.exe",
                r"C:\Program Files (x86)\KMP Media\KMPlayer\KMPlayer.exe",
            ],
            "AIMP" => &[
                r"C:\Program Files\AIMP\AIMP.exe",
                r"C:\Program Files (x86)\AIMP\AIMP.exe",
            ],
            "QMPlay2" => &[
                r"C:\Program Files\QMPlay2\QMPlay2.exe",
                r"C:\Program Files (x86)\QMPlay2\QMPlay2.exe",
            ],
            "GOM Player" => &[
                r"C:\Program Files\GRETECH\GomPlayer\GOM.exe",
                r"C:\Program Files (x86)\GRETECH\GomPlayer\GOM.exe",
            ],
            "Clementine" => &[r"C:\Program Files\Clementine\clementine.exe"],
            "Strawberry" => &[r"C:\Program Files\Strawberry\strawberry.exe"],
            _ => &[],
        };
        paths.iter().map(ToString::to_string).collect()
    }
}

fn macos_candidates(player_name: &str) -> Vec<String> {
    {
        let paths: &[&str] = match player_name {
            "VLC" => &["/Applications/VLC.app/Contents/MacOS/VLC"],
            "QuickTime" => &["/Applications/QuickTime Player.app/Contents/MacOS/QuickTime Player"],
            "iTunes/Apple Music" => &[
                "/Applications/Music.app/Contents/MacOS/Music",
                "/Applications/iTunes.app/Contents/MacOS/iTunes",
            ],
            "QMPlay2" => &["/Applications/QMPlay2.app/Contents/MacOS/QMPlay2"],
            "Audacious" => &["/Applications/Audacious.app/Contents/MacOS/Audacious"],
            "Fauxdacious" => &["/Applications/Fauxdacious.app/Contents/MacOS/Fauxdacious"],
            _ => &[],
        };
        paths.iter().map(ToString::to_string).collect()
    }
}

fn linux_candidates(player_name: &str) -> Vec<String> {
    let command = match player_name {
        "VLC" => "vlc",
        "MPV" => "mpv",
        "Kodi" => "kodi",
        "SMPlayer" => "smplayer",
        "Totem" => "totem",
        "PotPlayer" => "potplayer",
        "KMPlayer" => "kmplayer",
        "AIMP" => "aimp",
        "QMPlay2" => "qmplay2",
        "GOM Player" => "gomplayer",
        "Audacious" => "audacious",
        "Fauxdacious" => "fauxdacious",
        "MPC-BE" => "mpc-be",
        "Clementine" => "clementine",
        "Strawberry" => "strawberry",
        "Amarok" => "amarok",
        "Rhythmbox" => "rhythmbox",
        "Pragha" => "pragha",
        "Lollypop" => "lollypop",
        "Exaile" => "exaile",
        "Quod Libet" => "quodlibet",
        "Gmusicbrowser" => "gmusicbrowser",
        "Xmms" => "xmms",
        "Vocal" => "vocal",
        "Haruna" => "haruna",
        "Celluloid" => "celluloid",
        _ => return Vec::new(),
    };
    vec![command.to_string()]
}

/// The launch debounce (Python): the same stream in the same player
/// within 0.75 s is a duplicate; every accepted launch supersedes the
/// previous generation.
#[derive(Debug, Default)]
pub struct LaunchGate {
    last_ms: i64,
    last_key: (String, String, String),
    generation: u64,
}

impl LaunchGate {
    /// Returns the new generation when the launch is accepted.
    pub fn accept(&mut self, player: &str, path: &str, url: &str, now_ms: i64) -> Option<u64> {
        let key = (player.to_string(), path.to_string(), url.to_string());
        if now_ms - self.last_ms < 750 && self.last_key == key {
            return None;
        }
        self.last_ms = now_ms;
        self.last_key = key;
        self.generation += 1;
        Some(self.generation)
    }

    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }
}

/// Session state an engine reports and the UI announces.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum SessionState {
    #[default]
    Idle,
    Opening,
    Playing,
    Paused,
    Ended,
    Failed(String),
}

/// Events the engine side feeds the session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EngineEvent {
    Started,
    Stalled,
    Ended,
    Error(String),
}

/// What the session asks the engine to do next.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EngineAction {
    None,
    Load(PlaybackRequest),
    Reconnect(PlaybackRequest),
    Stop,
}

const MAX_RECONNECTS: u32 = 3;

/// The playback session: owns the current request, reconnect policy for
/// live streams, and volume (2% fine / 5% coarse steps, as in Python).
#[derive(Debug)]
pub struct PlaybackSession {
    state: SessionState,
    current: Option<PlaybackRequest>,
    reconnects: u32,
    volume: i32,
}

impl Default for PlaybackSession {
    fn default() -> Self {
        Self::new()
    }
}

impl PlaybackSession {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: SessionState::Idle,
            current: None,
            reconnects: 0,
            volume: 100,
        }
    }

    pub fn play(&mut self, request: PlaybackRequest) -> EngineAction {
        self.current = Some(request.clone());
        self.reconnects = 0;
        self.state = SessionState::Opening;
        EngineAction::Load(request)
    }

    pub fn stop(&mut self) -> EngineAction {
        self.current = None;
        self.state = SessionState::Idle;
        EngineAction::Stop
    }

    pub fn toggle_pause(&mut self) {
        match self.state {
            SessionState::Playing => self.state = SessionState::Paused,
            SessionState::Paused => self.state = SessionState::Playing,
            _ => {}
        }
    }

    pub fn on_event(&mut self, event: EngineEvent) -> EngineAction {
        match event {
            EngineEvent::Started => {
                self.state = SessionState::Playing;
                self.reconnects = 0;
                EngineAction::None
            }
            EngineEvent::Stalled => {
                if let (true, Some(request)) = (
                    self.current.as_ref().is_some_and(|r| r.live),
                    self.current.clone(),
                ) {
                    if self.reconnects < MAX_RECONNECTS {
                        self.reconnects += 1;
                        self.state = SessionState::Opening;
                        return EngineAction::Reconnect(request);
                    }
                }
                self.state = SessionState::Failed("Playback stalled.".to_string());
                EngineAction::Stop
            }
            EngineEvent::Ended => {
                self.state = SessionState::Ended;
                EngineAction::None
            }
            EngineEvent::Error(message) => {
                self.state = SessionState::Failed(message);
                EngineAction::Stop
            }
        }
    }

    #[must_use]
    pub fn state(&self) -> &SessionState {
        &self.state
    }

    #[must_use]
    pub fn current(&self) -> Option<&PlaybackRequest> {
        self.current.as_ref()
    }

    /// Adjust volume by `delta` (Python: 2% arrow steps, 5% Ctrl steps),
    /// clamped 0..=100. Returns the new volume.
    pub fn adjust_volume(&mut self, delta: i32) -> i32 {
        self.volume = (self.volume + delta).clamp(0, 100);
        self.volume
    }

    #[must_use]
    pub const fn volume(&self) -> i32 {
        self.volume
    }

    /// Status text for announcements.
    #[must_use]
    pub fn announcement(&self) -> String {
        match &self.state {
            SessionState::Idle => "Stopped.".to_string(),
            SessionState::Opening => format!(
                "Opening {}.",
                self.current.as_ref().map_or("", |r| r.title.as_str())
            ),
            SessionState::Playing => format!(
                "Playing {}.",
                self.current.as_ref().map_or("", |r| r.title.as_str())
            ),
            SessionState::Paused => "Paused.".to_string(),
            SessionState::Ended => "Playback ended.".to_string(),
            SessionState::Failed(message) => format!("Playback failed: {message}"),
        }
    }
}

/// One resume checkpoint (Python `playback_resume` record shape).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResumeEntry {
    pub position_ms: i64,
    pub length_ms: i64,
    pub updated: i64,
}

const RESUME_MIN_MS: i64 = 30_000;
const RESUME_MAX_ENTRIES: usize = 200;

/// Apply the Python checkpoint rules: a position under 30 s is not worth
/// keeping; a position inside the end window (30 s or the last 5%)
/// removes the entry because the item counts as finished.
pub fn resume_checkpoint(
    entries: &mut BTreeMap<String, ResumeEntry>,
    key: &str,
    position_ms: i64,
    length_ms: i64,
    now: i64,
) {
    if key.is_empty() || length_ms <= 0 || !(0..=length_ms).contains(&position_ms) {
        return;
    }
    let end_window = RESUME_MIN_MS.max(length_ms / 20);
    if position_ms < RESUME_MIN_MS || length_ms - position_ms <= end_window {
        entries.remove(key);
    } else {
        entries.insert(
            key.to_string(),
            ResumeEntry {
                position_ms,
                length_ms,
                updated: now,
            },
        );
    }
    // Keep the newest 200, like Python's normalize cap.
    if entries.len() > RESUME_MAX_ENTRIES {
        let mut by_age: Vec<(String, i64)> = entries
            .iter()
            .map(|(k, v)| (k.clone(), v.updated))
            .collect();
        by_age.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        for (stale, _) in by_age.into_iter().skip(RESUME_MAX_ENTRIES) {
            entries.remove(&stale);
        }
    }
}

/// The stored resume position for a key, if one survives the rules.
#[must_use]
pub fn resume_position(entries: &BTreeMap<String, ResumeEntry>, key: &str) -> Option<i64> {
    entries.get(key).map(|entry| entry.position_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap as Map;

    fn channel(url: &str) -> Channel {
        Channel {
            name: "Chan".to_string(),
            group: String::new(),
            url: url.to_string(),
            tvg_id: String::new(),
            tvg_name: String::new(),
            tvg_logo: String::new(),
            unknown_attributes: Map::new(),
        }
    }

    #[test]
    fn pipe_modifiers_split_and_canonicalize() {
        let (url, headers) = split_stream_modifiers(
            "http://x/y.m3u8|User-Agent=My+Agent|http-referrer=http://ref/|X-Custom=v",
        );
        assert_eq!(url, "http://x/y.m3u8");
        assert_eq!(headers.get("User-Agent"), Some("My Agent"));
        assert_eq!(headers.get("Referer"), Some("http://ref/"));
        assert_eq!(headers.extra, vec!["X-Custom: v"]);
    }

    #[test]
    fn attribute_headers_win_over_pipe_modifiers() {
        let mut chan = channel("http://x/y|User-Agent=PipeAgent");
        chan.unknown_attributes
            .insert("http-user-agent".to_string(), "AttrAgent".to_string());
        let headers = StreamHeaders::for_channel(&chan);
        assert_eq!(headers.get("User-Agent"), Some("AttrAgent"));
        let request = PlaybackRequest::for_channel(&chan, None);
        assert_eq!(
            request.url, "http://x/y",
            "pipe tail never reaches the engine"
        );
    }

    #[test]
    fn candidate_tables_match_python() {
        // Windows: the full 32-player menu's located paths.
        assert_eq!(
            candidate_executables("PotPlayer", true, false)[0],
            r"C:\Program Files\DAUM\PotPlayer\PotPlayerMini64.exe"
        );
        assert_eq!(candidate_executables("MPC", true, false).len(), 3);
        assert!(candidate_executables("Totem", true, false).is_empty());
        assert!(candidate_executables("Winamp", true, false)
            .iter()
            .any(|p| p.ends_with("winamp.exe")));
        // macOS: Python locates only six players there.
        assert_eq!(
            candidate_executables("QuickTime", false, true)[0],
            "/Applications/QuickTime Player.app/Contents/MacOS/QuickTime Player"
        );
        assert!(candidate_executables("MPV", false, true).is_empty());
        // Linux: bare commands; unconfigured players yield none.
        assert_eq!(
            candidate_executables("Celluloid", false, false),
            ["celluloid"]
        );
        assert_eq!(candidate_executables("MPC-BE", false, false), ["mpc-be"]);
        assert!(candidate_executables("MPC", false, false).is_empty());
        assert!(candidate_executables("Winamp", false, false).is_empty());
    }

    #[test]
    fn argv_shapes_match_python() {
        assert_eq!(
            external_argv("VLC", "vlc", "http://s", "ipc"),
            vec![
                "vlc",
                "--one-instance",
                "--one-instance-when-started-from-file",
                "http://s"
            ]
        );
        let mpv = external_argv("MPV", "mpv", "http://s", "/tmp/ipc");
        assert!(mpv[1].starts_with("--input-ipc-server=/tmp/ipc"));
        assert_eq!(
            external_argv("MPC", "mpc", "http://s", "ipc"),
            vec!["mpc", "http://s"]
        );
        assert_eq!(detect_from_path(r"C:\players\MpvPortable\mpv.exe"), "MPV");
        assert_eq!(detect_from_path("/usr/bin/mpc-hc"), "MPC");
    }

    #[test]
    fn launch_gate_debounces_same_stream_only() {
        let mut gate = LaunchGate::default();
        assert_eq!(gate.accept("VLC", "vlc", "http://s", 1_000), Some(1));
        assert_eq!(gate.accept("VLC", "vlc", "http://s", 1_500), None);
        assert_eq!(gate.accept("VLC", "vlc", "http://other", 1_600), Some(2));
        assert_eq!(gate.accept("VLC", "vlc", "http://s", 2_000), Some(3));
    }

    #[test]
    fn session_reconnects_live_then_fails_and_never_reconnects_vod() {
        let mut session = PlaybackSession::new();
        let request = PlaybackRequest::for_channel(&channel("http://live"), None);
        assert!(request.live);
        assert!(matches!(session.play(request), EngineAction::Load(_)));
        session.on_event(EngineEvent::Started);
        for _ in 0..MAX_RECONNECTS {
            assert!(matches!(
                session.on_event(EngineEvent::Stalled),
                EngineAction::Reconnect(_)
            ));
        }
        assert!(matches!(
            session.on_event(EngineEvent::Stalled),
            EngineAction::Stop
        ));
        assert!(matches!(session.state(), SessionState::Failed(_)));
        assert_eq!(session.adjust_volume(5), 100);
        assert_eq!(session.adjust_volume(-250), 0);

        let mut vod = PlaybackSession::new();
        let movie = PlaybackRequest::for_channel(&channel("http://x/movie/1.mp4"), None);
        assert!(!movie.live);
        vod.play(movie);
        vod.on_event(EngineEvent::Started);
        assert!(matches!(
            vod.on_event(EngineEvent::Stalled),
            EngineAction::Stop
        ));
    }

    #[test]
    fn resume_checkpoint_rules_match_python() {
        let mut entries = BTreeMap::new();
        resume_checkpoint(&mut entries, "k", 10_000, 3_600_000, 100);
        assert!(entries.is_empty(), "under 30s not kept");
        resume_checkpoint(&mut entries, "k", 900_000, 3_600_000, 100);
        assert_eq!(resume_position(&entries, "k"), Some(900_000));
        resume_checkpoint(&mut entries, "k", 3_590_000, 3_600_000, 200);
        assert!(entries.is_empty(), "inside end window removes");
    }

    #[test]
    fn start_cache_is_half_second_to_saved_buffer() {
        let cache = start_cache(&InternalPlayerSettings {
            buffer_seconds: 25.0,
            max_buffer_seconds: 30.0,
            variant_max_mbps: 0.0,
        });
        assert_eq!(cache.initial_ms, 500);
        assert_eq!(cache.target_ms, 25_000);
    }
}
