//! Persistent configuration, porting the Python `options.py` contract.
//!
//! The on-disk format is the Python one: a JSON object in `iptvclient.conf`.
//! Loading never fails the caller: an unreadable or wrong-shaped file is
//! copied to `<name>.corrupt`, the next candidate is tried, and defaults are
//! the last resort. Normalization clamps hand-edited values instead of
//! crashing startup, exactly as the reference does. Unknown keys survive a
//! load/save round trip untouched.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

pub const CONFIG_FILE_NAME: &str = "iptvclient.conf";
pub const APP_DATA_DIR_NAME: &str = "AccessibleIPTVClient";
pub const DEFAULT_BUFFER_SECONDS: f64 = 25.0;
pub const DEFAULT_MAX_BUFFER_SECONDS: f64 = 30.0;
pub const DEFAULT_RECORDING_FORMAT: &str = "provider_mkv";

const RECORDING_FORMATS: [&str; 10] = [
    "provider_mkv",
    "provider_mp4",
    "x264_mkv",
    "x264_mp4",
    "provider_mka",
    "audio_mp3_v0",
    "audio_flac",
    "audio_wav",
    "audio_aac_m4a",
    "audio_opus",
];

/// The Python default key set (`options.load_config`).
fn defaults() -> Map<String, Value> {
    let mut map = Map::new();
    let mut put = |key: &str, value: Value| {
        map.insert(key.to_string(), value);
    };
    put("playlists", json!([]));
    put("epgs", json!([]));
    put("media_player", json!("Built-in Player"));
    put("custom_player_path", json!(""));
    put(
        "internal_player_buffer_seconds",
        json!(DEFAULT_BUFFER_SECONDS),
    );
    put(
        "internal_player_max_buffer_seconds",
        json!(DEFAULT_MAX_BUFFER_SECONDS),
    );
    put("internal_player_variant_max_mbps", json!(0.0));
    put("minimize_to_tray", json!(false));
    put("auto_check_updates", json!(true));
    put("epg_enabled", json!(true));
    put("use_playlist_epg", json!(true));
    put("epg_auto_import_interval_hours", json!(6.0));
    put("show_player_on_enter", json!(true));
    put("show_channel_url", json!(true));
    put("language", json!("auto"));
    put("recordings_dir", json!(""));
    put("recording_format", json!(DEFAULT_RECORDING_FORMAT));
    put("recording_format_audio", json!("audio_mp3_v0"));
    put("recording_format_video", json!(DEFAULT_RECORDING_FORMAT));
    put("recording_pre_padding_minutes", json!(0));
    put("recording_post_padding_minutes", json!(2));
    put("shutdown_after_recordings", json!(false));
    put("favorites", json!([]));
    put("recent_channels", json!([]));
    put("live_buffer_enabled", json!(false));
    put("live_buffer_seconds", json!(600));
    put("live_buffer_mib", json!(512));
    put("sleep_fade_seconds", json!(5));
    put("announcement_level", json!(2));
    put("shortcuts", json!({}));
    put("preferred_audio_tracks", json!([]));
    put("prefer_audio_description", json!(false));
    put("last_audio_track", json!(""));
    put("channel_audio_tracks", json!({}));
    put("channel_audio_track_indices", json!({}));
    map
}

/// Python `coerce_bool`: booleans pass through, numbers are truthy-tested,
/// and the strings 1/true/yes/on and 0/false/no/off/"" are recognized.
#[must_use]
pub fn coerce_bool(value: Option<&Value>, default: bool) -> bool {
    match value {
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_f64().is_some_and(|n| n != 0.0),
        Some(Value::String(text)) => match text.trim().to_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" | "" => false,
            _ => default,
        },
        _ => default,
    }
}

/// Python `coerce_string_list`: a bare string splits on commas; containers
/// and nulls drop out; remaining items stringify, trim, and dedupe in order.
#[must_use]
pub fn coerce_string_list(value: Option<&Value>) -> Vec<String> {
    let items: Vec<&Value> = match value {
        Some(Value::String(text)) => {
            return normalize_texts(text.split(',').map(str::to_string));
        }
        Some(Value::Array(items)) => items.iter().collect(),
        _ => return Vec::new(),
    };
    let texts = items.into_iter().filter_map(|item| match item {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    });
    normalize_texts(texts)
}

fn normalize_texts(texts: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for text in texts {
        let text = text.trim().to_string();
        if !text.is_empty() && !out.contains(&text) {
            out.push(text);
        }
    }
    out
}

fn coerce_f64(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Number(number)) => number.as_f64(),
        Some(Value::String(text)) => text.trim().parse().ok(),
        _ => None,
    }
}

// Python coerces with int(float(value)), which truncates; the clamp right
// after bounds the result, and Rust float-to-int casts saturate safely.
#[allow(clippy::cast_possible_truncation)]
fn coerce_minutes(value: Option<&Value>, default: i64) -> i64 {
    coerce_f64(value)
        .map_or(default, |n| n as i64)
        .clamp(0, 180)
}

fn normalize_recording_format(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) if RECORDING_FORMATS.contains(&text.as_str()) => text.clone(),
        _ => DEFAULT_RECORDING_FORMAT.to_string(),
    }
}

/// The clamped internal-player buffering triple.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InternalPlayerSettings {
    pub buffer_seconds: f64,
    pub max_buffer_seconds: f64,
    pub variant_max_mbps: f64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Config {
    values: Map<String, Value>,
}

impl Config {
    /// Defaults, normalized.
    #[must_use]
    pub fn new() -> Self {
        let mut config = Self { values: defaults() };
        config.normalize();
        config
    }

    /// A loaded JSON object, or `None` when it is not an object at all
    /// (Python treats that as unreadable, same as a parse error).
    #[must_use]
    pub fn from_json(value: Value) -> Option<Self> {
        let Value::Object(values) = value else {
            return None;
        };
        let mut config = Self { values };
        config.normalize();
        Some(config)
    }

    /// Fill missing defaults and clamp every normalized key, in place.
    pub fn normalize(&mut self) {
        for (key, value) in defaults() {
            if key == "recording_format_audio" || key == "recording_format_video" {
                continue; // seeded by the migration below, as in Python
            }
            self.values.entry(key).or_insert(value);
        }
        let format = normalize_recording_format(self.values.get("recording_format"));
        self.values
            .insert("recording_format".to_string(), json!(format));
        // migrate_recording_format_prefs: per-media keys seed from the
        // legacy single format when absent; audio keeps its own default.
        self.values
            .entry("recording_format_video".to_string())
            .or_insert_with(|| json!(format));
        self.values
            .entry("recording_format_audio".to_string())
            .or_insert_with(|| json!("audio_mp3_v0"));
        for key in ["recording_format_audio", "recording_format_video"] {
            let normalized = normalize_recording_format(self.values.get(key));
            self.values.insert(key.to_string(), json!(normalized));
        }
        let pre = coerce_minutes(self.values.get("recording_pre_padding_minutes"), 0);
        let post = coerce_minutes(self.values.get("recording_post_padding_minutes"), 2);
        self.values
            .insert("recording_pre_padding_minutes".to_string(), json!(pre));
        self.values
            .insert("recording_post_padding_minutes".to_string(), json!(post));
        for key in ["favorites", "preferred_audio_tracks"] {
            let list = coerce_string_list(self.values.get(key));
            self.values.insert(key.to_string(), json!(list));
        }
        for key in ["prefer_audio_description", "shutdown_after_recordings"] {
            let flag = coerce_bool(self.values.get(key), false);
            self.values.insert(key.to_string(), json!(flag));
        }
        // Truncation is the Python int() behavior; clamped to 0..=3 below.
        #[allow(clippy::cast_possible_truncation)]
        let level = coerce_f64(self.values.get("announcement_level")).map_or(2, |n| n as i64);
        self.values
            .insert("announcement_level".to_string(), json!(level.clamp(0, 3)));
        let settings = self.internal_player_settings();
        self.values.insert(
            "internal_player_buffer_seconds".to_string(),
            json!(settings.buffer_seconds),
        );
        self.values.insert(
            "internal_player_max_buffer_seconds".to_string(),
            json!(settings.max_buffer_seconds),
        );
        self.values.insert(
            "internal_player_variant_max_mbps".to_string(),
            json!(settings.variant_max_mbps),
        );
    }

    /// Python `_apply_internal_player_bounds`, including the legacy
    /// max == 18 default migrating to 30 so the live cushion has room.
    #[must_use]
    pub fn internal_player_settings(&self) -> InternalPlayerSettings {
        let mut max = coerce_f64(self.values.get("internal_player_max_buffer_seconds"))
            .unwrap_or(DEFAULT_MAX_BUFFER_SECONDS);
        if max <= 0.0 || (max - 18.0).abs() < f64::EPSILON {
            max = DEFAULT_MAX_BUFFER_SECONDS;
        }
        max = max.min(300.0);
        let mut base = coerce_f64(self.values.get("internal_player_buffer_seconds"))
            .unwrap_or(DEFAULT_BUFFER_SECONDS);
        if base <= 0.0 {
            base = DEFAULT_BUFFER_SECONDS;
        }
        if base > max {
            base = max;
        }
        let mut variant =
            coerce_f64(self.values.get("internal_player_variant_max_mbps")).unwrap_or(0.0);
        variant = if variant <= 0.0 {
            0.0
        } else {
            variant.clamp(0.25, 500.0)
        };
        InternalPlayerSettings {
            buffer_seconds: base,
            max_buffer_seconds: max,
            variant_max_mbps: variant,
        }
    }

    #[must_use]
    pub fn favorites(&self) -> Vec<String> {
        coerce_string_list(self.values.get("favorites"))
    }

    pub fn set_favorites(&mut self, keys: Vec<String>) {
        let list = normalize_texts(keys);
        self.values.insert("favorites".to_string(), json!(list));
    }

    #[must_use]
    pub fn string_list(&self, key: &str) -> Vec<String> {
        coerce_string_list(self.values.get(key))
    }

    #[must_use]
    pub fn bool_or(&self, key: &str, default: bool) -> bool {
        coerce_bool(self.values.get(key), default)
    }

    #[must_use]
    pub fn string_or(&self, key: &str, default: &str) -> String {
        match self.values.get(key) {
            Some(Value::String(text)) => text.clone(),
            _ => default.to_string(),
        }
    }

    #[must_use]
    pub fn raw(&self) -> &Map<String, Value> {
        &self.values
    }

    /// Set an arbitrary key (used by features that own a config section,
    /// such as resume positions). Normalization never removes unknown keys.
    pub fn set_value(&mut self, key: &str, value: Value) {
        self.values.insert(key.to_string(), value);
    }

    #[must_use]
    pub fn value(&self, key: &str) -> Option<&Value> {
        self.values.get(key)
    }

    /// Pretty JSON exactly as a save would write it.
    #[must_use]
    pub fn to_json_string(&self) -> String {
        serde_json::to_string_pretty(&Value::Object(self.values.clone()))
            .unwrap_or_else(|_| "{}".to_string())
    }

    /// Atomic save: unique temp file in the same directory, fsync, rename.
    /// Normalizes first, as Python's `save_config` does.
    ///
    /// # Errors
    ///
    /// Returns the I/O error when the directory cannot be created, the temp
    /// file cannot be written or synced, or the rename fails.
    pub fn save(&mut self, path: &Path) -> io::Result<()> {
        self.normalize();
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                fs::create_dir_all(dir)?;
            }
        }
        let dir = path
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        let name = path.file_name().map_or_else(
            || CONFIG_FILE_NAME.to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let temp = dir.join(format!(".{name}.{stamp}.tmp"));
        let result = (|| {
            fs::write(&temp, self.to_json_string())?;
            fs::File::open(&temp)?.sync_all()?;
            fs::rename(&temp, path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }
}

/// The outcome of a candidate scan: where saves go, and which unreadable
/// files were preserved as `.corrupt` copies along the way.
#[derive(Debug)]
pub struct Loaded {
    pub config: Config,
    pub path: PathBuf,
    pub recovered: Vec<PathBuf>,
}

/// Try each candidate in order; fall back to defaults aimed at
/// `write_target`. Corrupt candidates are preserved, never deleted.
#[must_use]
pub fn load(candidates: &[PathBuf], write_target: &Path) -> Loaded {
    let mut recovered = Vec::new();
    for candidate in candidates {
        let Ok(text) = fs::read_to_string(candidate) else {
            continue;
        };
        let parsed = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(Config::from_json);
        if let Some(config) = parsed {
            return Loaded {
                config,
                path: candidate.clone(),
                recovered,
            };
        }
        let kept = PathBuf::from(format!("{}.corrupt", candidate.display()));
        if fs::copy(candidate, &kept).is_ok() {
            recovered.push(kept);
        }
    }
    Loaded {
        config: Config::new(),
        path: write_target.to_path_buf(),
        recovered,
    }
}

/// The per-user config directory: `%APPDATA%` on Windows, `$XDG_CONFIG_HOME`
/// or `~/.config` elsewhere.
#[must_use]
pub fn user_config_dir() -> PathBuf {
    if cfg!(windows) {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return PathBuf::from(appdata).join(APP_DATA_DIR_NAME);
        }
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join(APP_DATA_DIR_NAME);
        }
    }
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join(".config").join(APP_DATA_DIR_NAME)
}

/// Read candidates in Python's spirit: the per-user location first, then a
/// portable copy beside the working directory.
#[must_use]
pub fn config_candidates() -> Vec<PathBuf> {
    vec![
        user_config_dir().join(CONFIG_FILE_NAME),
        PathBuf::from(CONFIG_FILE_NAME),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_from(text: &str) -> Config {
        Config::from_json(serde_json::from_str(text).expect("json")).expect("object")
    }

    #[test]
    fn defaults_carry_the_python_key_set() {
        let config = Config::new();
        assert_eq!(config.string_or("media_player", "?"), "Built-in Player");
        assert_eq!(config.string_or("language", "?"), "auto");
        assert!(config.favorites().is_empty());
        assert_eq!(
            config.internal_player_settings(),
            InternalPlayerSettings {
                buffer_seconds: 25.0,
                max_buffer_seconds: 30.0,
                variant_max_mbps: 0.0,
            }
        );
    }

    #[test]
    fn buffer_bounds_match_python_including_the_18_migration() {
        let config = config_from(
            r#"{"internal_player_buffer_seconds": 99, "internal_player_max_buffer_seconds": 18}"#,
        );
        let settings = config.internal_player_settings();
        assert!((settings.max_buffer_seconds - 30.0).abs() < f64::EPSILON);
        assert!((settings.buffer_seconds - 30.0).abs() < f64::EPSILON); // clamped to max
        let zeroed = config_from(
            r#"{"internal_player_buffer_seconds": "junk", "internal_player_variant_max_mbps": 0.1}"#,
        );
        assert!((zeroed.internal_player_settings().buffer_seconds - 25.0).abs() < f64::EPSILON);
        assert!((zeroed.internal_player_settings().variant_max_mbps - 0.25).abs() < f64::EPSILON);
    }

    #[test]
    fn hand_edited_lists_and_bools_are_coerced_not_fatal() {
        let config = config_from(
            r#"{"favorites": "a|b, a|b, , c", "shutdown_after_recordings": "yes", "announcement_level": 9}"#,
        );
        assert_eq!(config.favorites(), vec!["a|b", "c"]);
        assert!(config.bool_or("shutdown_after_recordings", false));
        assert_eq!(config.raw()["announcement_level"], json!(3));
    }

    #[test]
    fn unknown_keys_survive_and_wrong_shape_is_rejected() {
        let config = config_from(r#"{"my_custom_key": {"x": 1}}"#);
        assert_eq!(config.raw()["my_custom_key"], json!({"x": 1}));
        assert!(Config::from_json(json!([1, 2, 3])).is_none());
    }

    #[test]
    fn corrupt_candidate_is_preserved_and_defaults_load() {
        let dir = std::env::temp_dir().join(format!("iptv-config-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("dir");
        let bad = dir.join("iptvclient.conf");
        fs::write(&bad, "{not json").expect("write");
        let target = dir.join("fresh").join("iptvclient.conf");
        let loaded = load(std::slice::from_ref(&bad), &target);
        assert_eq!(loaded.path, target);
        assert_eq!(loaded.recovered.len(), 1);
        assert!(loaded.recovered[0].exists());
        assert!(bad.exists(), "original kept");
        let mut config = loaded.config;
        config.set_favorites(vec!["k1".to_string(), "k1".to_string(), " k2 ".to_string()]);
        config.save(&target).expect("save");
        let reloaded = load(std::slice::from_ref(&target), &target);
        assert_eq!(reloaded.config.favorites(), vec!["k1", "k2"]);
        let _ = fs::remove_dir_all(&dir);
    }
}
