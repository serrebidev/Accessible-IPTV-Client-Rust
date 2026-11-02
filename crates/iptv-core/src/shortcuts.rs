//! Validated keyboard shortcuts (port of the Python app's
//! `shortcuts.py`), shared by the main window and the player.
//!
//! wx accelerator text ("Ctrl+Shift+P") is parsed into modifier flags
//! plus a key code, canonicalized back to the same text form, checked
//! against the app's validity law, and merged over the defaults from
//! the config's `shortcuts` map — a hand-edited invalid override is
//! ignored rather than installed, exactly as the reference does.

use std::collections::BTreeMap;

/// Modifier flag: Ctrl (matches wx `ACCEL_CTRL`).
pub const ACCEL_CTRL: u32 = 0x0001;
/// Modifier flag: Alt (matches wx `ACCEL_ALT`).
pub const ACCEL_ALT: u32 = 0x0002;
/// Modifier flag: Shift (matches wx `ACCEL_SHIFT`).
pub const ACCEL_SHIFT: u32 = 0x0004;

/// The F1 key code (wx `WXK_F1`), reserved for Help.
pub const KEY_F1: u32 = 340;

/// Main-window defaults: action → (accelerator text, command id)
/// (port of `MAIN`).
#[must_use]
pub fn main_defaults() -> Vec<(&'static str, &'static str, i32)> {
    vec![
        ("playlist_manager", "Ctrl+M", 4001),
        ("epg_manager", "Ctrl+E", 4002),
        ("import_epg", "Ctrl+I", 4003),
        ("exit", "Ctrl+Q", 4004),
        ("play_pause", "Ctrl+Shift+P", 4010),
        ("stop", "Ctrl+Shift+S", 4011),
        ("cast", "Ctrl+Shift+C", 4012),
        ("volume_up", "Ctrl+Up", 4015),
        ("volume_down", "Ctrl+Down", 4016),
        ("record", "Ctrl+Shift+R", 4017),
        ("account", "Ctrl+Shift+A", 4018),
        ("favorite", "Ctrl+D", 4019),
        ("downloads", "Ctrl+Shift+D", 4020),
        ("show_player", "Ctrl+Shift+J", 4021),
        ("previous_channel", "Ctrl+0", 4022),
        ("channel_number", "Ctrl+G", 4023),
        ("recent_channels", "Ctrl+H", 4024),
        ("whats_on_now", "Ctrl+W", 4025),
        ("what_is_playing", "Ctrl+Shift+I", 4026),
    ]
}

/// Player defaults: action → accelerator text (port of `PLAYER`).
#[must_use]
pub fn player_defaults() -> Vec<(&'static str, &'static str)> {
    vec![
        ("play_pause", "Ctrl+P"),
        ("stop", "Ctrl+S"),
        ("record", "Ctrl+R"),
        ("cast", "Ctrl+C"),
        ("volume_up", "Up"),
        ("volume_down", "Down"),
        ("audio_track", "A"),
        ("subtitles", "S"),
        ("speak_subtitles", "Shift+S"),
        ("read_subtitle", "C"),
        ("previous_subtitle", "PageUp"),
        ("next_subtitle", "PageDown"),
        ("fullscreen", "F11"),
        ("hide", "Ctrl+W"),
        ("exit", "Ctrl+Q"),
        ("what_is_playing", "I"),
    ]
}

/// Menu labels per context (port of `LABELS`): so a conflict names
/// the command the user knows, not the internal key.
#[must_use]
pub fn label(context: &str, action: &str) -> Option<&'static str> {
    let table: &[(&str, &str)] = match context {
        "main" => &[
            ("playlist_manager", "Playlist Manager"),
            ("epg_manager", "EPG Manager"),
            ("import_epg", "Import EPG to DB"),
            ("exit", "Exit"),
            ("play_pause", "Play/Pause"),
            ("stop", "Stop"),
            ("cast", "Cast / Connect..."),
            ("volume_up", "Volume Up"),
            ("volume_down", "Volume Down"),
            ("record", "Start Recording"),
            ("account", "Account Info"),
            ("favorite", "Add to Favorites"),
            ("downloads", "Show Downloads"),
            ("show_player", "Show Built-in Player"),
            ("previous_channel", "Previous Channel"),
            ("channel_number", "Go to Channel Number..."),
            ("recent_channels", "Recently Watched..."),
            ("whats_on_now", "What's on Now"),
            ("what_is_playing", "What Is Playing"),
        ],
        _ => &[
            ("play_pause", "Play/Pause"),
            ("stop", "Stop"),
            ("record", "Record"),
            ("cast", "Cast..."),
            ("volume_up", "Volume Up"),
            ("volume_down", "Volume Down"),
            ("audio_track", "Audio Track"),
            ("subtitles", "Subtitles"),
            ("speak_subtitles", "Speak Subtitles"),
            ("read_subtitle", "Read Current Subtitle"),
            ("previous_subtitle", "Previous Subtitle"),
            ("next_subtitle", "Next Subtitle"),
            ("fullscreen", "Toggle Full Screen"),
            ("hide", "Hide Window"),
            ("exit", "Exit Player"),
            ("what_is_playing", "What Is Playing"),
        ],
    };
    table
        .iter()
        .find(|(name, _)| *name == action)
        .map(|(_, text)| *text)
}

/// A named (non-character) key: canonical wx name → key code using
/// the wx numbering for the special keys the defaults use.
fn named_key(name: &str) -> Option<(u32, String)> {
    let upper = name.to_uppercase();
    let (code, canonical) = match upper.as_str() {
        "UP" => (315, "Up"),
        "DOWN" => (317, "Down"),
        "LEFT" => (314, "Left"),
        "RIGHT" => (316, "Right"),
        "PAGEUP" => (366, "PageUp"),
        "PAGEDOWN" => (367, "PageDown"),
        "HOME" => (313, "Home"),
        "END" => (312, "End"),
        "SPACE" => (32, "Space"),
        "ENTER" | "RETURN" => (13, "Enter"),
        "ESC" | "ESCAPE" => (27, "Esc"),
        "TAB" => (9, "Tab"),
        "DELETE" | "DEL" => (127, "Delete"),
        "INSERT" | "INS" => (322, "Insert"),
        "BACK" | "BACKSPACE" => (8, "Back"),
        _ => {
            if let Some(rest) = upper.strip_prefix('F') {
                if let Ok(number) = rest.parse::<u32>() {
                    if (1..=24).contains(&number) {
                        return Some((339 + number, format!("F{number}")));
                    }
                }
            }
            return None;
        }
    };
    Some((code, canonical.to_string()))
}

/// Parse accelerator text into (flags, key code, canonical text)
/// (port of `parse`, over the vocabulary wx produces for these
/// bindings).
///
/// # Errors
/// Returns the reference's "Invalid keyboard shortcut." for anything
/// unparseable.
pub fn parse(value: &str) -> Result<(u32, u32, String), String> {
    let text = value.trim();
    if text.is_empty() {
        return Err("Invalid keyboard shortcut.".to_string());
    }
    let parts: Vec<&str> = text.split('+').map(str::trim).collect();
    let (modifiers, key_part) = parts.split_at(parts.len() - 1);
    let mut flags = 0_u32;
    let mut names: Vec<&str> = Vec::new();
    for modifier in modifiers {
        match modifier.to_lowercase().as_str() {
            "ctrl" | "control" => {
                flags |= ACCEL_CTRL;
                names.push("Ctrl");
            }
            "alt" => {
                flags |= ACCEL_ALT;
                names.push("Alt");
            }
            "shift" => {
                flags |= ACCEL_SHIFT;
                names.push("Shift");
            }
            _ => return Err("Invalid keyboard shortcut.".to_string()),
        }
    }
    let key_text = key_part[0];
    let (key, canonical_key) = if key_text.chars().count() == 1 {
        let ch = key_text.chars().next().unwrap_or(' ');
        if ch.is_ascii_graphic() {
            (
                u32::from(ch.to_ascii_uppercase()),
                ch.to_ascii_uppercase().to_string(),
            )
        } else {
            return Err("Invalid keyboard shortcut.".to_string());
        }
    } else {
        named_key(key_text).ok_or_else(|| "Invalid keyboard shortcut.".to_string())?
    };
    let mut canonical = names.join("+");
    if !canonical.is_empty() {
        canonical.push('+');
    }
    canonical.push_str(&canonical_key);
    Ok((flags, key, canonical))
}

/// True when the app can live with this shortcut (port of `_allowed`):
/// F1 is the help key, and a main-window shortcut without Ctrl or Alt
/// would swallow ordinary typing in the search box.
#[must_use]
pub fn allowed(context: &str, flags: u32, key: u32) -> bool {
    if key == KEY_F1 {
        return false;
    }
    if context == "main" && flags & (ACCEL_CTRL | ACCEL_ALT) == 0 {
        return false;
    }
    true
}

fn defaults_for(context: &str) -> Vec<(String, String)> {
    if context == "main" {
        main_defaults()
            .into_iter()
            .map(|(action, text, _)| (action.to_string(), text.to_string()))
            .collect()
    } else {
        player_defaults()
            .into_iter()
            .map(|(action, text)| (action.to_string(), text.to_string()))
            .collect()
    }
}

/// The shortcuts in force for a context (port of `effective`): the
/// defaults with the valid config overrides applied.
#[must_use]
pub fn effective(config: &serde_json::Value, context: &str) -> BTreeMap<String, String> {
    let saved = config
        .get("shortcuts")
        .and_then(serde_json::Value::as_object);
    let mut result = BTreeMap::new();
    for (action, default) in defaults_for(context) {
        let mut value = default;
        if let Some(overridden) = saved
            .and_then(|map| map.get(&format!("{context}.{action}")))
            .and_then(serde_json::Value::as_str)
        {
            if !overridden.is_empty() {
                if let Ok((flags, key, label)) = parse(overridden) {
                    if allowed(context, flags, key) {
                        value = label;
                    }
                }
            }
        }
        result.insert(action, value);
    }
    result
}

/// Set (or reset, with an empty value) one shortcut in a config map
/// (port of `set_shortcut`), with the reference's error strings.
///
/// # Errors
/// Returns the reference's messages for unknown commands, invalid
/// text, the F1 reservation, the main-window modifier law, and
/// conflicts (naming the other command's label).
pub fn set_shortcut(
    config: &mut serde_json::Map<String, serde_json::Value>,
    context: &str,
    action: &str,
    value: &str,
) -> Result<(), String> {
    let defaults = defaults_for(context);
    let Some((_, default)) = defaults.iter().find(|(name, _)| name == action) else {
        return Err("Unknown command.".to_string());
    };
    let default = default.clone();
    let value = if value.trim().is_empty() {
        default.clone()
    } else {
        parse(value)?.2
    };
    let (flags, key, _) = parse(&value)?;
    if !allowed(context, flags, key) {
        if key == KEY_F1 {
            return Err("F1 is reserved for Help.".to_string());
        }
        return Err("Main-window shortcuts need Ctrl or Alt.".to_string());
    }
    let current = effective(&serde_json::Value::Object(config.clone()), context);
    for (other, existing) in &current {
        if other != action {
            if let Ok((other_flags, other_key, _)) = parse(existing) {
                if (other_flags, other_key) == (flags, key) {
                    let name = label(context, other).unwrap_or(other);
                    return Err(format!("Shortcut conflicts with {name}."));
                }
            }
        }
    }
    let mut saved = config
        .get("shortcuts")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    let key_name = format!("{context}.{action}");
    if value == default {
        saved.remove(&key_name);
    } else {
        saved.insert(key_name, serde_json::Value::String(value));
    }
    config.insert("shortcuts".to_string(), serde_json::Value::Object(saved));
    Ok(())
}

/// Accelerator entries for the main window's menus (port of
/// `main_entries`): (flags, key code, command id).
#[must_use]
pub fn main_entries(config: &serde_json::Value) -> Vec<(u32, u32, i32)> {
    let keys = effective(config, "main");
    main_defaults()
        .into_iter()
        .filter_map(|(action, _, command_id)| {
            let text = keys.get(action)?;
            let (flags, key, _) = parse(text).ok()?;
            Some((flags, key, command_id))
        })
        .collect()
}

/// All configurable commands, searched (port of
/// `searchable_commands`): (context, action, label, binding, display) rows,
/// player rows prefixed "Built-in Player — ".
#[must_use]
pub fn searchable_commands(
    config: &serde_json::Value,
    query: &str,
) -> Vec<(String, String, String, String, String)> {
    let needle = query.trim().to_lowercase();
    let mut rows = Vec::new();
    for context in ["main", "player"] {
        let bindings = effective(config, context);
        for (action, _) in defaults_for(context) {
            let name = label(context, &action).unwrap_or(&action).to_string();
            let mut display = format!("{}: {}", name, bindings[&action]);
            if context == "player" {
                display = format!("Built-in Player — {display}");
            }
            if needle.is_empty() || display.to_lowercase().contains(&needle) {
                let binding = bindings[&action].clone();
                rows.push((context.to_string(), action, name, binding, display));
            }
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_canonicalizes_and_validates() {
        assert_eq!(
            parse("ctrl+shift+p"),
            Ok((
                ACCEL_CTRL | ACCEL_SHIFT,
                u32::from('P'),
                "Ctrl+Shift+P".to_string()
            ))
        );
        assert_eq!(parse("PageUp").map(|v| v.2), Ok("PageUp".to_string()));
        assert_eq!(
            parse("F11").map(|v| (v.1, v.2)),
            Ok((350, "F11".to_string()))
        );
        assert!(parse("Bogus+Key").is_err());
        assert!(parse("").is_err());
    }

    #[test]
    fn validity_law_and_overrides() {
        assert!(!allowed("main", 0, u32::from('X')));
        assert!(allowed("player", 0, u32::from('X')));
        assert!(!allowed("player", ACCEL_CTRL, KEY_F1));
        let mut config =
            serde_json::json!({"shortcuts": {"main.record": "Ctrl+Alt+R", "main.cast": "R"}});
        let eff = effective(&config, "main");
        assert_eq!(eff["record"], "Ctrl+Alt+R");
        assert_eq!(eff["cast"], "Ctrl+Shift+C", "invalid override ignored");
        let entries = main_entries(&config);
        assert_eq!(entries.len(), 19);
        let rows = searchable_commands(&config, "recording");
        assert_eq!(rows.len(), 1, "only Start Recording matches");
        assert_eq!(rows[0].0, "main");
        let rows = searchable_commands(&config, "record");
        assert_eq!(rows.len(), 2);
        assert!(rows[1].4.starts_with("Built-in Player — "));
        assert_eq!(rows[1].3, "Ctrl+R", "raw binding rides along for editors");
        let _ = &mut config;
    }

    #[test]
    fn set_shortcut_errors_and_reset() {
        let mut config = serde_json::Map::new();
        assert_eq!(
            set_shortcut(&mut config, "main", "record", "F1"),
            Err("F1 is reserved for Help.".to_string())
        );
        assert_eq!(
            set_shortcut(&mut config, "main", "record", "R"),
            Err("Main-window shortcuts need Ctrl or Alt.".to_string())
        );
        assert_eq!(
            set_shortcut(&mut config, "main", "record", "Ctrl+Q"),
            Err("Shortcut conflicts with Exit.".to_string())
        );
        set_shortcut(&mut config, "main", "record", "Ctrl+Alt+R").expect("set");
        assert_eq!(
            config["shortcuts"]["main.record"],
            serde_json::json!("Ctrl+Alt+R")
        );
        set_shortcut(&mut config, "main", "record", "").expect("reset");
        assert!(config["shortcuts"].as_object().expect("map").is_empty());
        assert_eq!(
            set_shortcut(&mut config, "main", "nope", "Ctrl+X"),
            Err("Unknown command.".to_string())
        );
    }
}
