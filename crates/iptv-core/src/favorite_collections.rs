//! Named, ordered favourite lists (Python
//! `favorite_collections.py`) and their portable export /
//! import (Python `favourite_transfer.py`). The lists live
//! in the `favorite_collections` config key, independent of
//! the default favourites; only safe names and opaque
//! identities ever leave the device in an export.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::reminders::{reminder_channel_key, safe_title};
use crate::Channel;

/// One stream's identity: its address plus its sorted
/// request headers (Python `media_identity`).
type MediaIdentity = crate::reminders::MediaIdentity;

/// Python `programme_reminders.media_identity` (the canonical
/// portal-aware identity in `reminders`).
fn media_identity(channel: &Channel) -> MediaIdentity {
    crate::reminders::media_identity(channel)
}

pub const MAX_LISTS: usize = 50;
pub const MAX_CHANNELS: usize = 2000;
pub const FORMAT: &str = "accessible-iptv-favourites-v1";
pub const TRANSFER_LIMIT: usize = 3 * 1024 * 1024;

/// One stored entry: an opaque channel key, its display
/// name at save time, and the optional guide / variant
/// identity parts Python's shape carries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FavEntry {
    pub key: String,
    pub name: String,
    pub guide: Option<String>,
    pub match_key: Option<String>,
    pub variant: Option<String>,
}

impl FavEntry {
    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        map.insert("key".to_string(), json!(self.key));
        map.insert("name".to_string(), json!(self.name));
        if let Some(guide) = &self.guide {
            map.insert("guide".to_string(), json!(guide));
        }
        if let Some(match_key) = &self.match_key {
            map.insert("match_key".to_string(), json!(match_key));
        }
        if let Some(variant) = &self.variant {
            map.insert("variant".to_string(), json!(variant));
        }
        Value::Object(map)
    }
}

/// One named list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FavList {
    pub id: String,
    pub name: String,
    pub entries: Vec<FavEntry>,
}

impl FavList {
    #[must_use]
    pub fn to_value(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "entries": self.entries.iter().map(FavEntry::to_value).collect::<Vec<_>>(),
        })
    }
}

fn is_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// Python `json.dumps` of one string (ASCII-escaped).
fn py_json_string(value: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                for unit in c.encode_utf16(&mut [0; 2]) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A JSON string with Python's `ensure_ascii=False` escaping:
/// only quotes, backslashes and control characters escape.
fn json_string_raw(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Python `clean_name`: a trimmed name of 1-80 characters
/// with no control characters.
///
/// # Errors
/// Returns Python's message when the name is unusable.
pub fn clean_name(value: &str) -> Result<String, &'static str> {
    let text = value.trim();
    if text.is_empty() || text.chars().count() > 80 || text.chars().any(|c| (c as u32) < 32) {
        return Err("Enter a name of 1 to 80 characters.");
    }
    Ok(text.to_string())
}

/// Python `normalize`: keep well-formed lists only - valid
/// ids, unique names (case-insensitive), unique well-formed
/// entry keys, caps of 50 lists and 2000 entries each.
#[must_use]
pub fn normalize(value: Option<&Value>) -> Vec<FavList> {
    let Some(items) = value.and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut output = Vec::new();
    let mut ids = BTreeSet::new();
    let mut names = BTreeSet::new();
    for item in items.iter().take(MAX_LISTS) {
        let Some(map) = item.as_object() else {
            continue;
        };
        let id = map.get("id").and_then(Value::as_str).unwrap_or("");
        if !is_hex(id, 32) || ids.contains(id) {
            continue;
        }
        let Ok(name) = clean_name(map.get("name").and_then(Value::as_str).unwrap_or("")) else {
            continue;
        };
        if names.contains(&name.to_lowercase()) {
            continue;
        }
        let mut entries = Vec::new();
        let mut seen = BTreeSet::new();
        if let Some(raw) = map.get("entries").and_then(Value::as_array) {
            for entry in raw.iter().take(MAX_CHANNELS) {
                let Some(entry) = entry.as_object() else {
                    continue;
                };
                let key = entry.get("key").and_then(Value::as_str).unwrap_or("");
                if !is_hex(key, 64) || seen.contains(key) {
                    continue;
                }
                seen.insert(key.to_string());
                let mut fav = FavEntry {
                    key: key.to_string(),
                    name: safe_title(entry.get("name").and_then(Value::as_str).unwrap_or("")),
                    guide: None,
                    match_key: None,
                    variant: None,
                };
                if let Some(guide) = entry.get("guide").and_then(Value::as_str) {
                    if is_hex(guide, 64) {
                        fav.guide = Some(guide.to_string());
                    }
                }
                let match_key = entry.get("match_key").and_then(Value::as_str);
                let variant = entry.get("variant").and_then(Value::as_str);
                if let (Some(match_key), Some(variant)) = (match_key, variant) {
                    if is_hex(match_key, 64) && is_hex(variant, 64) {
                        fav.match_key = Some(match_key.to_string());
                        fav.variant = Some(variant.to_string());
                    }
                }
                entries.push(fav);
            }
        }
        ids.insert(id.to_string());
        names.insert(name.to_lowercase());
        output.push(FavList {
            id: id.to_string(),
            name,
            entries,
        });
    }
    output
}

/// Python `create`: append a new empty list; returns the
/// rows and the new list's id.
///
/// # Errors
/// Python's messages for the cap and duplicate names.
///
/// # Panics
/// Only if the OS randomness source fails while minting the
/// new list's id.
pub fn create(collections: &Value, name: &str) -> Result<(Vec<FavList>, String), &'static str> {
    let mut rows = normalize(Some(collections));
    if rows.len() >= MAX_LISTS {
        return Err("The maximum number of favourite lists is 50.");
    }
    let name = clean_name(name)?;
    if rows
        .iter()
        .any(|row| row.name.to_lowercase() == name.to_lowercase())
    {
        return Err("A list with this name already exists.");
    }
    // Python uses uuid4 hex; the tree's uuid crate has no v4
    // feature, so mint the 32 hex chars from the OS RNG.
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("OS randomness for a list id");
    let id = hex(&bytes);
    rows.push(FavList {
        id: id.clone(),
        name,
        entries: Vec::new(),
    });
    Ok((rows, id))
}

/// Python `variant_labels`: duplicate labels gain a
/// "Variant {number}" suffix (the template arrives
/// translated from the app).
#[must_use]
pub fn variant_labels(labels: &[String], variant_template: &str) -> Vec<String> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for label in labels {
        *counts.entry(label.as_str()).or_default() += 1;
    }
    let mut seen: HashMap<&str, usize> = HashMap::new();
    labels
        .iter()
        .map(|label| {
            let n = {
                let entry = seen.entry(label.as_str()).or_default();
                *entry += 1;
                *entry
            };
            if counts[label.as_str()] > 1 {
                format!(
                    "{} — {}",
                    label,
                    variant_template.replace("{number}", &n.to_string())
                )
            } else {
                label.clone()
            }
        })
        .collect()
}

/// Python `variant_key`: sha256 of the JSON dump of the
/// media identity pair.
#[must_use]
pub fn variant_key(channel: &Channel) -> String {
    let identity = match crate::reminders::media_identity(channel) {
        crate::reminders::MediaIdentity::Portal(parts) => {
            let parts: Vec<String> = parts.iter().map(|v| py_json_string(v)).collect();
            format!("[{}]", parts.join(", "))
        }
        crate::reminders::MediaIdentity::Stream(url, headers_json) => {
            format!(
                "[{}, {}]",
                py_json_string(&url),
                py_json_string(&headers_json)
            )
        }
    };
    sha256_hex(identity.as_bytes())
}

/// Python `selected_entry`: the stored entry for a channel,
/// disambiguated to a variant when the same channel key
/// covers more than one distinct stream.
///
/// # Errors
/// Python's messages when the channel cannot be keyed or
/// vanished from the supplied channel list.
pub fn selected_entry(
    channel: &Channel,
    channels: Option<&[Channel]>,
) -> Result<FavEntry, &'static str> {
    let key = crate::reminders::station_key(channel);
    if key.is_empty() || !crate::reminders::playable(channel) {
        return Err("Channel Not Found");
    }
    let mut entry = FavEntry {
        key: key.clone(),
        name: safe_title(&channel.name),
        guide: None,
        match_key: None,
        variant: None,
    };
    if let Some(channels) = channels {
        let chosen = crate::reminders::media_identity(channel);
        let mut found = false;
        let mut ambiguous = false;
        for other in channels {
            if crate::reminders::playable(other) && crate::reminders::station_key(other) == key {
                if crate::reminders::media_identity(other) == chosen {
                    found = true;
                } else {
                    ambiguous = true;
                }
                if found && ambiguous {
                    break;
                }
            }
        }
        if !found {
            return Err("The channel list changed. Try again.");
        }
        if ambiguous {
            let variant = variant_key(channel);
            entry = FavEntry {
                key: sha256_hex(format!("{key}{variant}").as_bytes()),
                name: entry.name,
                guide: None,
                match_key: Some(key),
                variant: Some(variant),
            };
        }
    }
    Ok(entry)
}

/// The operations Python's `edit` applies, in its order.
#[derive(Default)]
pub struct Edit {
    pub name: Option<String>,
    pub add: Option<Channel>,
    pub selection: Option<FavEntry>,
    pub remove: Option<String>,
    pub move_entry: Option<(String, i64)>,
    pub delete: bool,
}

/// Python `edit`: rename, add, remove, move or delete one
/// list, identified by id.
///
/// # Errors
/// Python's messages for a vanished list, a bad or duplicate
/// name, and the channel cap.
pub fn edit(
    collections: &Value,
    identity: &str,
    edit: &Edit,
) -> Result<Vec<FavList>, &'static str> {
    let mut rows = normalize(Some(collections));
    let Some(position) = rows.iter().position(|row| row.id == identity) else {
        return Err("The selected list no longer exists.");
    };
    if edit.delete {
        rows.remove(position);
        return Ok(rows);
    }
    if let Some(name) = &edit.name {
        let name = clean_name(name)?;
        if rows
            .iter()
            .any(|row| row.id != identity && row.name.to_lowercase() == name.to_lowercase())
        {
            return Err("A list with this name already exists.");
        }
        rows[position].name = name;
    }
    if edit.selection.is_some() || edit.add.is_some() {
        let entry = match (&edit.selection, &edit.add) {
            (Some(selection), _) => selection.clone(),
            (None, Some(channel)) => selected_entry(channel, None)?,
            _ => unreachable!(),
        };
        let row = &mut rows[position];
        let duplicate = row.entries.iter().any(|e| {
            e.key == entry.key
                || (entry.variant.is_some()
                    && e.match_key == entry.match_key
                    && e.variant == entry.variant)
        });
        if !duplicate {
            if row.entries.len() >= MAX_CHANNELS {
                return Err("A favourite list can contain at most 2000 channels.");
            }
            row.entries.push(entry);
        }
    }
    if let Some(remove) = &edit.remove {
        rows[position].entries.retain(|e| &e.key != remove);
    }
    if let Some((key, delta)) = &edit.move_entry {
        if !matches!(delta, -1 | 1) {
            return Err("Invalid list movement");
        }
        let row = &mut rows[position];
        if let Some(index) = row.entries.iter().position(|e| &e.key == key) {
            let target = i64::try_from(index).unwrap_or(0) + delta;
            if let Ok(target) = usize::try_from(target) {
                if target < row.entries.len() {
                    row.entries.swap(index, target);
                }
            }
        }
    }
    Ok(rows)
}

/// Python `relink`: replace one stale entry in place,
/// preserving the order of the list.
///
/// # Errors
/// Python's messages for a vanished list or entry and for a
/// replacement already in the list.
pub fn relink(
    collections: &Value,
    identity: &str,
    old_key: &str,
    channel: &Channel,
    selection: Option<FavEntry>,
) -> Result<Vec<FavList>, &'static str> {
    let mut rows = normalize(Some(collections));
    let Some(position) = rows.iter().position(|row| row.id == identity) else {
        return Err("The selected list no longer exists.");
    };
    let row = &mut rows[position];
    let Some(index) = row.entries.iter().position(|e| e.key == old_key) else {
        return Err("The channel list changed. Try again.");
    };
    let replacement = match selection {
        Some(selection) => selection,
        None => selected_entry(channel, None)?,
    };
    let duplicate = row.entries.iter().enumerate().any(|(i, e)| {
        i != index
            && (e.key == replacement.key
                || (replacement.variant.is_some()
                    && e.match_key == replacement.match_key
                    && e.variant == replacement.variant))
    });
    if duplicate {
        return Err("Choose a different channel that is not already in this list.");
    }
    row.entries[index] = replacement;
    Ok(rows)
}

/// Python `resolve_channel` over pre-matched channels: the
/// single distinct stream, or None when zero or several.
fn resolve_one(matches: &[Channel]) -> Option<Channel> {
    let mut distinct: Vec<crate::reminders::MediaIdentity> = Vec::new();
    let mut chosen: Option<Channel> = None;
    for channel in matches {
        if !crate::reminders::playable(channel) {
            continue;
        }
        let identity = crate::reminders::media_identity(channel);
        if !distinct.contains(&identity) {
            distinct.push(identity);
            chosen = Some(channel.clone());
        }
    }
    if distinct.len() == 1 {
        chosen
    } else {
        None
    }
}

/// Python `resolve`: pair every entry with its channel, or
/// None when the entry is missing or ambiguous - missing
/// rows are preserved, never dropped.
#[must_use]
pub fn resolve(entries: &[FavEntry], channels: &[Channel]) -> Vec<(FavEntry, Option<Channel>)> {
    let wanted: BTreeSet<String> = entries
        .iter()
        .map(|e| e.match_key.clone().unwrap_or_else(|| e.key.clone()))
        .collect();
    let mut index: HashMap<String, Vec<Channel>> = HashMap::new();
    for channel in channels {
        let key = reminder_channel_key(channel);
        let mut keys = vec![key];
        if channel
            .unknown_attributes
            .get("provider-type")
            .is_some_and(|kind| kind == "stalker")
        {
            keys.push(crate::reminders::station_key(channel));
        }
        for key in keys {
            if wanted.contains(&key) {
                index.entry(key).or_default().push(channel.clone());
            }
        }
    }
    entries
        .iter()
        .map(|entry| {
            let key = entry.match_key.clone().unwrap_or_else(|| entry.key.clone());
            let empty: Vec<Channel> = Vec::new();
            let matches = index.get(&key).unwrap_or(&empty);
            let filtered: Vec<Channel> = match &entry.variant {
                Some(variant) => matches
                    .iter()
                    .filter(|ch| crate::reminders::playable(ch) && &variant_key(ch) == variant)
                    .cloned()
                    .collect(),
                None => matches.clone(),
            };
            (entry.clone(), resolve_one(&filtered))
        })
        .collect()
}

/// Python `programme_reminders.source_names`: playlist
/// id/provider id to cleaned name, plus `playlist_names`
/// aliases keyed "m3u:<sha256>". (The canonical reminders
/// port is PR #88; this copy serves the favourite dialogs
/// until both are on main.)
#[must_use]
pub fn source_names(config: &Value) -> BTreeMap<String, String> {
    let mut names = BTreeMap::new();
    if let Some(sources) = config.get("playlists").and_then(Value::as_array) {
        for source in sources {
            let Some(map) = source.as_object() else {
                continue;
            };
            let key = map
                .get("id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .or_else(|| map.get("provider_id").and_then(Value::as_str));
            if let Some(key) = key {
                names.insert(
                    key.to_string(),
                    safe_title(map.get("name").and_then(Value::as_str).unwrap_or("")),
                );
            }
        }
    }
    if let Some(aliases) = config.get("playlist_names").and_then(Value::as_object) {
        for (key, name) in aliases {
            if is_hex(key, 64) {
                if let Some(name) = name.as_str() {
                    names.insert(format!("m3u:{key}"), safe_title(name));
                }
            }
        }
    }
    names
}

// ----------------------------------------------------------------
// Portable transfer (Python `favourite_transfer.py`)
// ----------------------------------------------------------------

/// Python `public_name`: a name safe to export - no web
/// address or login details survive.
#[must_use]
pub fn public_name(value: &str) -> String {
    let value = safe_title(value);
    if value.is_empty() {
        return String::new();
    }
    if value.chars().any(|c| c.is_control() && c != ' ') {
        return String::new();
    }
    if value.contains(['@', '/', '\\', '?', '=']) {
        return String::new();
    }
    let lowered = value.to_lowercase();
    for word in ["password", "token", "cookie", "authorization"] {
        if let Some(at) = lowered.find(word) {
            let rest = lowered[at + word.len()..].trim_start();
            if rest.starts_with(':') {
                return String::new();
            }
        }
    }
    value
}

/// Python `guide_key`: sha256 of the channel's case-folded
/// tvg-id, or empty when it has none.
#[must_use]
pub fn guide_key(channel: &Channel) -> String {
    let guide = channel.tvg_id.trim().to_lowercase();
    if guide.is_empty() {
        String::new()
    } else {
        sha256_hex(guide.as_bytes())
    }
}

/// Python `export_list`: the portable JSON document for one
/// list (the caller writes the bytes). Guide keys come from
/// the resolved channel when there is one, else the stored
/// guide value.
///
/// # Errors
/// Python's message when the list name is not exportable.
pub fn export_list(row: &FavList, channels: &[Channel]) -> Result<Vec<u8>, &'static str> {
    let name = public_name(&row.name);
    if name.is_empty() {
        return Err("Choose a list name without a web address or login details.");
    }
    let mut entries_text = String::new();
    for (i, (entry, channel)) in resolve(&row.entries, channels).iter().enumerate() {
        let guide = match channel {
            Some(channel) => guide_key(channel),
            None => entry.guide.clone().unwrap_or_default(),
        };
        if i > 0 {
            entries_text.push_str(",\n");
        }
        entries_text.push_str("    {\n");
        {
            use std::fmt::Write as _;
            let _ = write!(
                entries_text,
                "      \"name\": {},\n      \"key\": {},\n      \"guide\": {}\n",
                json_string_raw(&public_name(&entry.name)),
                json_string_raw(&entry.key),
                json_string_raw(&guide),
            );
        }
        entries_text.push_str("    }");
    }
    let entries_block = if entries_text.is_empty() {
        "[]".to_string()
    } else {
        format!("[\n{entries_text}\n  ]")
    };
    let text = format!(
        "{{\n  \"format\": {},\n  \"name\": {},\n  \"entries\": {}\n}}",
        json_string_raw(FORMAT),
        json_string_raw(&name),
        entries_block,
    );
    Ok(text.into_bytes())
}

/// One portable entry from an import file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PortableEntry {
    pub name: String,
    pub key: String,
    pub guide: String,
}

/// Python `read_list`: read and strictly validate a portable
/// list document.
///
/// # Errors
/// "Invalid favourite list file." for every structural
/// problem; the name-length message propagates from
/// `clean_name`, as in Python.
pub fn read_list(data: &[u8]) -> Result<(String, Vec<PortableEntry>), &'static str> {
    const INVALID: &str = "Invalid favourite list file.";
    if data.len() > TRANSFER_LIMIT {
        return Err(INVALID);
    }
    let text = String::from_utf8_lossy(data);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let bundle: Value = serde_json::from_str(text).map_err(|_| INVALID)?;
    let Some(map) = bundle.as_object() else {
        return Err(INVALID);
    };
    if map.len() != 3
        || !map.contains_key("format")
        || !map.contains_key("name")
        || !map.contains_key("entries")
    {
        return Err(INVALID);
    }
    let name = map.get("name").and_then(Value::as_str).unwrap_or("");
    if map.get("format").and_then(Value::as_str) != Some(FORMAT)
        || public_name(name).is_empty()
        || public_name(name) != name
    {
        return Err(INVALID);
    }
    let Some(items) = map.get("entries").and_then(Value::as_array) else {
        return Err(INVALID);
    };
    if items.len() > MAX_CHANNELS {
        return Err(INVALID);
    }
    clean_name(name)?;
    let mut keys = BTreeSet::new();
    let mut entries = Vec::new();
    for item in items {
        let Some(item) = item.as_object() else {
            return Err(INVALID);
        };
        if item.len() != 3
            || !item.contains_key("name")
            || !item.contains_key("key")
            || !item.contains_key("guide")
        {
            return Err(INVALID);
        }
        let entry_name = item.get("name").and_then(Value::as_str).unwrap_or("");
        let key = item.get("key").and_then(Value::as_str).unwrap_or("");
        let guide = item.get("guide").and_then(Value::as_str).unwrap_or("");
        if public_name(entry_name) != entry_name
            || !is_hex(key, 64)
            || keys.contains(key)
            || (!guide.is_empty() && !is_hex(guide, 64))
        {
            return Err(INVALID);
        }
        keys.insert(key.to_string());
        entries.push(PortableEntry {
            name: entry_name.to_string(),
            key: key.to_string(),
            guide: guide.to_string(),
        });
    }
    Ok((name.to_string(), entries))
}

/// Python `match`: for each portable entry, the candidate
/// (key, channel) pairs on this device - by portable key
/// first, then by guide key, then by name.
#[must_use]
pub fn match_entries(
    entries: &[PortableEntry],
    channels: &[Channel],
) -> Vec<(PortableEntry, Vec<(String, Channel)>)> {
    let wanted: BTreeSet<&str> = entries.iter().map(|e| e.key.as_str()).collect();
    let guides: BTreeSet<&str> = entries
        .iter()
        .map(|e| e.guide.as_str())
        .filter(|g| !g.is_empty())
        .collect();
    let names: BTreeSet<String> = entries
        .iter()
        .map(|e| e.name.to_lowercase())
        .filter(|n| !n.is_empty())
        .collect();
    // key -> [(identity, channel)] in first-seen order.
    let mut by_key: HashMap<String, Vec<(MediaIdentity, Channel)>> = HashMap::new();
    let mut by_guide: HashMap<String, BTreeSet<String>> = HashMap::new();
    let mut by_name: HashMap<String, BTreeSet<String>> = HashMap::new();
    for channel in channels {
        if !crate::reminders::playable(channel)
            || matches!(
                channel.unknown_attributes.get("kind").map(String::as_str),
                Some("movie" | "series")
            )
        {
            continue;
        }
        let key = crate::reminders::station_key(channel);
        let guide = guide_key(channel);
        let name = public_name(&channel.name).to_lowercase();
        if !key.is_empty()
            && (wanted.contains(key.as_str())
                || guides.contains(guide.as_str())
                || names.contains(&name))
        {
            let identity = media_identity(channel);
            let bucket = by_key.entry(key.clone()).or_default();
            if !bucket.iter().any(|(existing, _)| *existing == identity) {
                bucket.push((identity, channel.clone()));
            }
            if guides.contains(guide.as_str()) {
                by_guide.entry(guide).or_default().insert(key.clone());
            }
            if names.contains(&name) {
                by_name.entry(name).or_default().insert(key);
            }
        }
    }
    entries
        .iter()
        .map(|item| {
            let keys: BTreeSet<String> = if by_key.contains_key(&item.key) {
                BTreeSet::from([item.key.clone()])
            } else {
                let by_guide_keys = by_guide.get(&item.guide);
                match by_guide_keys {
                    Some(set) if !set.is_empty() => set.clone(),
                    _ => by_name
                        .get(&item.name.to_lowercase())
                        .cloned()
                        .unwrap_or_default(),
                }
            };
            let mut options = Vec::new();
            for key in &keys {
                if let Some(bucket) = by_key.get(key) {
                    for (_, channel) in bucket {
                        options.push((key.clone(), channel.clone()));
                    }
                }
            }
            (item.clone(), options)
        })
        .collect()
}

/// Python `build_entries`: turn reviewed match rows and the
/// user's choices into stored entries. `choices[i]` is the
/// chosen option index for row i, or None to leave it
/// unresolved.
///
/// # Errors
/// Python's message when two rows resolve to one channel or
/// one entry key.
pub fn build_entries(
    rows: &[(PortableEntry, Vec<(String, Channel)>)],
    choices: &[Option<usize>],
) -> Result<Vec<FavEntry>, &'static str> {
    const DUPLICATE: &str = "Choose a different channel that is not already in this list.";
    let mut result = Vec::new();
    let mut used: BTreeSet<String> = BTreeSet::new();
    let mut selected_media: BTreeSet<(String, MediaIdentity)> = BTreeSet::new();
    for ((item, options), choice) in rows.iter().zip(choices.iter()) {
        let mut entry = FavEntry {
            key: item.key.clone(),
            name: item.name.clone(),
            guide: if item.guide.is_empty() {
                None
            } else {
                Some(item.guide.clone())
            },
            match_key: None,
            variant: None,
        };
        match choice {
            Some(choice) => {
                let (key, channel) = &options[*choice];
                let media = (key.clone(), media_identity(channel));
                if selected_media.contains(&media) {
                    return Err(DUPLICATE);
                }
                selected_media.insert(media);
                entry.key.clone_from(key);
                entry.name = public_name(&channel.name);
                entry.guide = {
                    let guide = guide_key(channel);
                    if guide.is_empty() {
                        None
                    } else {
                        Some(guide)
                    }
                };
                if options.iter().filter(|(k, _)| k == key).count() > 1 {
                    entry.key.clone_from(&item.key);
                    entry.match_key = Some(key.clone());
                    entry.variant = Some(variant_key(channel));
                }
            }
            None => {
                if options.iter().any(|(key, _)| key == &item.key) {
                    entry.key = sha256_hex(format!("unresolved-favourite:{}", item.key).as_bytes());
                }
            }
        }
        if used.contains(&entry.key) {
            return Err(DUPLICATE);
        }
        used.insert(entry.key.clone());
        result.push(entry);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn channel(name: &str, url: &str, attrs: &[(&str, &str)]) -> Channel {
        Channel {
            name: name.to_string(),
            group: String::new(),
            url: url.to_string(),
            tvg_id: String::new(),
            tvg_name: String::new(),
            tvg_logo: String::new(),
            unknown_attributes: attrs
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    #[test]
    fn normalize_drops_bad_rows_and_caps() {
        let value = json!([
            {"id": "xyz", "name": "Bad id", "entries": []},
            {"id": "b".repeat(32), "name": "Good", "entries": [
                {"key": "c".repeat(64), "name": "One"},
                {"key": "c".repeat(64), "name": "Dup"},
                {"key": "nope", "name": "Bad key"},
                {"key": "d".repeat(64), "name": "https://evil.example/x"},
            ]},
            {"id": "e".repeat(32), "name": "good", "entries": []},
        ]);
        let rows = normalize(Some(&value));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].entries.len(), 2);
        // safe_title blanks the URL-looking name.
        assert_eq!(rows[0].entries[1].name, "");
    }

    #[test]
    fn create_and_edit_flow() {
        let (rows, id) = create(&json!([]), "  My List ").unwrap();
        assert_eq!(rows[0].name, "My List");
        let value = Value::Array(rows.iter().map(FavList::to_value).collect());
        assert_eq!(
            create(&value, "my list").unwrap_err(),
            "A list with this name already exists."
        );
        let ch = channel("Chan", "http://x/1", &[("provider-id", "p1")]);
        let rows = edit(
            &value,
            &id,
            &Edit {
                add: Some(ch),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(rows[0].entries.len(), 1);
        let key = rows[0].entries[0].key.clone();
        let value = Value::Array(rows.iter().map(FavList::to_value).collect());
        // Adding the same channel again is a no-op.
        let ch2 = channel("Chan", "http://x/1", &[("provider-id", "p1")]);
        let rows = edit(
            &value,
            &id,
            &Edit {
                add: Some(ch2),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(rows[0].entries.len(), 1);
        let rows = edit(
            &value,
            &id,
            &Edit {
                remove: Some(key),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(rows[0].entries.is_empty());
    }

    #[test]
    fn selected_entry_variant_disambiguation() {
        let a = channel("Chan", "http://x/a", &[("provider-id", "p1")]);
        let b = channel("Chan", "http://x/b", &[("provider-id", "p1")]);
        let channels = vec![a.clone(), b];
        let entry = selected_entry(&a, Some(&channels)).unwrap();
        assert!(entry.variant.is_some());
        assert_eq!(
            entry.match_key.as_deref(),
            Some(reminder_channel_key(&a).as_str())
        );
        let resolved = resolve(&[entry], &channels);
        assert_eq!(
            resolved[0].1.as_ref().map(|c| c.url.as_str()),
            Some("http://x/a")
        );
        // Without the variant, the pair is ambiguous.
        let plain = FavEntry {
            key: reminder_channel_key(&a),
            name: "Chan".to_string(),
            guide: None,
            match_key: None,
            variant: None,
        };
        assert!(resolve(&[plain], &channels)[0].1.is_none());
    }

    #[test]
    fn transfer_round_trip_and_validation() {
        let ch = channel("Chan One", "http://x/1", &[("provider-id", "p1")]);
        let mut with_guide = ch.clone();
        with_guide.tvg_id = "Guide-1".to_string();
        let entry = selected_entry(&with_guide, None).unwrap();
        let row = FavList {
            id: "a".repeat(32),
            name: "Portable".to_string(),
            entries: vec![entry],
        };
        let bytes = export_list(&row, &[with_guide.clone()]).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("\"format\": \"accessible-iptv-favourites-v1\""));
        let (name, entries) = read_list(text.as_bytes()).unwrap();
        assert_eq!(name, "Portable");
        assert_eq!(entries[0].guide, guide_key(&with_guide));
        // A tampered file is invalid.
        let bad = text.replace("accessible-iptv-favourites-v1", "other");
        assert_eq!(
            read_list(bad.as_bytes()).unwrap_err(),
            "Invalid favourite list file."
        );
        // Match + build on the same device resolves by key.
        let rows = match_entries(&entries, &[with_guide.clone()]);
        assert_eq!(rows[0].1.len(), 1);
        let built = build_entries(&rows, &[Some(0)]).unwrap();
        assert_eq!(built[0].key, entries[0].key);
        // Leaving it unresolved rewrites the key.
        let built = build_entries(&rows, &[None]).unwrap();
        assert_ne!(built[0].key, entries[0].key);
    }

    #[test]
    fn public_name_filters_credentials() {
        assert_eq!(public_name("News 24"), "News 24");
        assert_eq!(public_name("user@example.com"), "");
        assert_eq!(public_name("token: abc"), "");
        assert_eq!(public_name("a/b"), "");
    }
}
