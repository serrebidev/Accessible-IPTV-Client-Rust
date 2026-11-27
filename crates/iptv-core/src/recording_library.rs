//! Python `recording_library`: the local recording inventory.
//! Media and metadata stay in their recording folder; each folder
//! carries a `.iptv-recordings.json` index keyed by file name.
//! Ported at the post-PR-#36 state: per-directory locks, bounded
//! index reads with corrupt-byte preservation, sanitised info,
//! the pure `query_rows` filter/sort, and exact-signature resume
//! resolution.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::UNIX_EPOCH;

use serde_json::{Map, Value};

use crate::bookmarks;
use crate::reminders::safe_title;
use crate::Channel;

pub const INDEX_NAME: &str = ".iptv-recordings.json";
pub const TRASH_DIR_NAME: &str = ".iptv-recording-trash";
pub const MAX_INDEX_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_FOLDERS: usize = 20;
pub const METADATA_WARNING: &str =
    "Recording metadata could not be read. The media files are still available.";

const EXTENSIONS: &[&str] = &[
    ".mkv", ".mp4", ".ts", ".mts", ".m2ts", ".mp3", ".m4a", ".aac", ".flac", ".ogg", ".wav",
    ".mka", ".opus", ".webm", ".avi", ".mov", ".mpg",
];

const STATUSES: &[&str] = &["completed", "partial", "stopped", "failed", "unknown"];

/// One library row (Python's row dict): a media file plus its
/// sanitised index metadata.
#[derive(Clone, Debug, PartialEq)]
pub struct LibraryRow {
    pub path: String,
    pub title: String,
    pub channel: String,
    pub duration: Option<f64>,
    pub status: String,
    pub size: u64,
    pub modified_ns: i64,
    /// Local `"%Y-%m-%d %H:%M"` of the file mtime.
    pub date: String,
    /// The resolved folder the row was scanned from.
    pub directory: String,
    /// Trash subfolder name (trash rows only).
    pub trash_id: Option<String>,
    /// Original file name (trash rows only).
    pub original_name: Option<String>,
}

// ---------------------------------------------------------------------
// Per-directory locks (Python `_lock_for`): a slow USB library
// must not serialise metadata work for a different drive.

static LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();

fn lock_for(directory: &Path) -> Arc<Mutex<()>> {
    let key = norm_key(&absolute_normalized(directory));
    let registry = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = registry.lock().unwrap();
    map.entry(key)
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// `os.path.realpath` + `os.path.normcase`: canonicalise when the
/// path exists, else lexically normalise the absolute form;
/// lowercased on Windows only.
fn absolute_normalized(path: &Path) -> PathBuf {
    if let Ok(real) = fs::canonicalize(path) {
        return real;
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(path),
            Err(_) => path.to_path_buf(),
        }
    };
    let mut out = PathBuf::new();
    for part in absolute.components() {
        use std::path::Component;
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Make a path absolute and resolve `.`/`..` lexically,
/// without touching the filesystem.
fn lexical_absolute(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(path),
            Err(_) => path.to_path_buf(),
        }
    };
    let mut out = PathBuf::new();
    for part in absolute.components() {
        use std::path::Component;
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn norm_key(path: &Path) -> String {
    let text = path.to_string_lossy().into_owned();
    if cfg!(windows) {
        text.to_lowercase()
    } else {
        text
    }
}

fn resolve(path: &Path) -> PathBuf {
    absolute_normalized(path)
}

#[allow(clippy::cast_possible_truncation)] // nanoseconds since the epoch fit an i64 until 2262
fn modified_ns(metadata: &fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_nanos() as i64)
}

fn date_text(mtime_seconds: i64) -> String {
    crate::epg::local_datetime(mtime_seconds)
}

/// Local `"%Y-%m-%d %H:%M:%S"` for a Unix timestamp (the
/// conflicts dialog's `local_time`); `"Unknown time"` when
/// unformattable, as Python's fallback.
#[must_use]
pub fn local_datetime_seconds(epoch_seconds: i64) -> String {
    chrono::DateTime::from_timestamp(epoch_seconds, 0).map_or_else(
        || "Unknown time".to_string(),
        |utc| {
            utc.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        },
    )
}

fn random_hex() -> String {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).expect("OS randomness for a library id");
    crate::hex_lower(&bytes)
}

// ---------------------------------------------------------------------
// Index read/write.

type Index = Vec<(String, Value)>;

fn index_get<'a>(index: &'a Index, name: &str) -> Option<&'a Value> {
    index.iter().find(|(key, _)| key == name).map(|(_, v)| v)
}

/// Python `_read`. Without `preserve`, a corrupt index reads as
/// empty (with the user warning appended). With `preserve`, the
/// unreadable bytes are first copied verbatim to a
/// `.corrupt-<uuidhex>` sibling; if that backup fails the error
/// propagates so a mutating caller aborts instead of overwriting
/// unpreserved data.
fn read_index(
    directory: &Path,
    preserve: bool,
    warnings: &mut Vec<String>,
) -> Result<Index, String> {
    let path = directory.join(INDEX_NAME);
    let mut raw = Vec::new();
    let read_outcome = fs::File::open(&path).and_then(|mut file| {
        file.by_ref()
            .take((MAX_INDEX_BYTES + 1) as u64)
            .read_to_end(&mut raw)
    });
    match read_outcome {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return corrupt(directory, preserve, warnings),
        Ok(_) => {}
    }
    if raw.len() > MAX_INDEX_BYTES {
        return corrupt(directory, preserve, warnings);
    }
    let parsed: Result<Value, _> = serde_json::from_slice(&raw);
    match parsed {
        Ok(Value::Object(map)) => Ok(map.into_iter().collect()),
        _ => corrupt(directory, preserve, warnings),
    }
}

fn corrupt(directory: &Path, preserve: bool, warnings: &mut Vec<String>) -> Result<Index, String> {
    crate::applog::warn(
        "recording-library",
        &format!(
            "Could not read recording metadata: {}",
            directory.join(INDEX_NAME).display()
        ),
    );
    warnings.push(METADATA_WARNING.to_string());
    if preserve {
        preserve_backup(&directory.join(INDEX_NAME))?;
    }
    Ok(Vec::new())
}

fn preserve_backup(path: &Path) -> Result<(), String> {
    let raw = fs::read(path).map_err(|error| error.to_string())?;
    let backup = path.with_file_name(format!("{INDEX_NAME}.corrupt-{}", random_hex()));
    let outcome = (|| -> std::io::Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&backup)?;
        std::io::Write::write_all(&mut file, &raw)?;
        file.sync_all()
    })();
    if let Err(error) = outcome {
        let _ = fs::remove_file(&backup);
        return Err(error.to_string());
    }
    Ok(())
}

fn write_index(directory: &Path, index: &Index) -> Result<(), String> {
    let mut map = Map::new();
    for (key, value) in index {
        map.insert(key.clone(), value.clone());
    }
    let text = serde_json::to_string(&Value::Object(map)).map_err(|e| e.to_string())?;
    let temporary = directory.join(format!(".iptv-library-{}.tmp", random_hex()));
    let outcome = (|| -> std::io::Result<()> {
        let mut file = fs::File::create(&temporary)?;
        std::io::Write::write_all(&mut file, text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, directory.join(INDEX_NAME))
    })();
    if outcome.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    outcome.map_err(|error| error.to_string())
}

/// Python `_clean_info`: whitelist the status, validate the
/// duration, and pass title/channel through `safe_title`.
fn clean_info(info: &Value) -> Value {
    let empty = Map::new();
    let source = info.as_object().unwrap_or(&empty);
    let mut cleaned = source.clone();
    let duration = source.get("duration").and_then(|value| {
        value
            .as_f64()
            .filter(|number| number.is_finite() && *number >= 0.0)
    });
    cleaned.insert(
        "duration".to_string(),
        duration.map_or(Value::Null, |number| serde_json::json!(number)),
    );
    let status = source
        .get("status")
        .and_then(Value::as_str)
        .filter(|status| STATUSES.contains(status))
        .unwrap_or("unknown");
    cleaned.insert("status".to_string(), Value::String(status.to_string()));
    for key in ["title", "channel"] {
        let raw = source.get(key).and_then(Value::as_str).unwrap_or("");
        cleaned.insert(key.to_string(), Value::String(safe_title(raw)));
    }
    Value::Object(cleaned)
}

fn info_number(info: &Value, key: &str) -> Option<i64> {
    info.get(key).and_then(Value::as_i64)
}

// ---------------------------------------------------------------------
// Scanning.

fn active_keys(active_paths: &[PathBuf]) -> Vec<String> {
    active_paths
        .iter()
        .map(|path| norm_key(&absolute_normalized(path)))
        .collect()
}

/// Python `scan`: every media file in one folder (index-named
/// files included even without a media extension), minus the
/// active paths, sorted newest first. A top-level folder read
/// failure is the only error; unreadable entries are skipped.
///
/// # Errors
/// Returns the folder read error text when the directory itself
/// cannot be listed.
/// # Panics
/// Panics if a per-directory library lock is poisoned.
#[allow(clippy::cast_possible_wrap)] // file sizes fit an i64 in practice
pub fn scan(
    directory: &Path,
    active_paths: &[PathBuf],
    canceled: &dyn Fn() -> bool,
    warnings: &mut Vec<String>,
) -> Result<Vec<LibraryRow>, String> {
    let root = resolve(directory);
    let index = {
        let lock = lock_for(&root);
        let _guard = lock.lock().unwrap();
        read_index(&root, false, warnings).unwrap_or_default()
    };
    if !root.exists() {
        return Ok(Vec::new());
    }
    let active = active_keys(active_paths);
    let entries = fs::read_dir(&root).map_err(|error| error.to_string())?;
    let mut result = Vec::new();
    for entry in entries {
        if canceled() {
            return Ok(Vec::new());
        }
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        let Ok(link_meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if link_meta.file_type().is_symlink() || !link_meta.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let extension = path
            .extension()
            .map(|ext| format!(".{}", ext.to_string_lossy().to_lowercase()))
            .unwrap_or_default();
        if !EXTENSIONS.contains(&extension.as_str()) && index_get(&index, &name).is_none() {
            continue;
        }
        if active.contains(&norm_key(&path)) {
            continue;
        }
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        let size = metadata.len();
        let mtime_ns = modified_ns(&metadata);
        // Index metadata is only trusted when the stored
        // signature still matches the file.
        let mut info = index_get(&index, &name).cloned().unwrap_or(Value::Null);
        if !info.is_object()
            || info_number(&info, "size") != Some(size as i64)
            || info_number(&info, "modified_ns") != Some(mtime_ns)
        {
            info = Value::Null;
        }
        let info = clean_info(&info);
        let title = {
            let cleaned = info
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if cleaned.is_empty() {
                path.file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
                    .unwrap_or_default()
            } else {
                cleaned
            }
        };
        result.push(LibraryRow {
            path: path.to_string_lossy().into_owned(),
            title,
            channel: info
                .get("channel")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            duration: info.get("duration").and_then(Value::as_f64),
            status: info
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
            size,
            modified_ns: mtime_ns,
            date: date_text(mtime_ns / 1_000_000_000),
            directory: root.to_string_lossy().into_owned(),
            trash_id: None,
            original_name: None,
        });
    }
    result.sort_by(|a, b| {
        b.modified_ns
            .cmp(&a.modified_ns)
            .then_with(|| a.path.cmp(&b.path))
    });
    Ok(result)
}

/// Python `checked_path`: the row's file must still exist,
/// unchanged and unused, directly inside the folder.
///
/// # Errors
/// Returns Python's user-facing messages for a vanished, changed
/// or in-use recording.
/// # Panics
/// Panics if a per-directory library lock is poisoned.
pub fn checked_path(
    directory: &Path,
    row: &LibraryRow,
    active_paths: &[PathBuf],
) -> Result<PathBuf, String> {
    let root = resolve(directory);
    let path = PathBuf::from(&row.path);
    let unavailable = || "The recording file is no longer available.".to_string();
    if fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err(unavailable());
    }
    let canonical = fs::canonicalize(&path).map_err(|_| unavailable())?;
    if canonical.parent() != Some(root.as_path()) || !path.is_file() {
        return Err(unavailable());
    }
    let metadata = fs::metadata(&path).map_err(|_| unavailable())?;
    if metadata.len() != row.size || modified_ns(&metadata) != row.modified_ns {
        return Err("The recording file changed. Refresh the library.".to_string());
    }
    let resolved_key = norm_key(&canonical);
    if active_paths
        .iter()
        .any(|active| norm_key(&absolute_normalized(active)) == resolved_key)
    {
        return Err("The recording is still in use.".to_string());
    }
    Ok(path)
}

// ---------------------------------------------------------------------
// Mutations.

/// Python `delete`: drop the index row, then unlink the file; a
/// failed unlink restores the index.
///
/// # Errors
/// Propagates the checked-path, preservation and unlink errors.
/// # Panics
/// Panics if a per-directory library lock is poisoned.
pub fn delete(directory: &Path, row: &LibraryRow, active_paths: &[PathBuf]) -> Result<(), String> {
    let lock = lock_for(directory);
    let _guard = lock.lock().unwrap();
    let path = checked_path(directory, row, active_paths)?;
    let parent = path.parent().unwrap_or(Path::new("."));
    let mut warnings = Vec::new();
    let original = read_index(parent, true, &mut warnings)?;
    let rows: Index = original
        .iter()
        .filter(|(name, _)| name != &file_name(&path))
        .cloned()
        .collect();
    write_index(parent, &rows)?;
    if let Err(error) = fs::remove_file(&path) {
        let _ = write_index(parent, &original);
        return Err(error.to_string());
    }
    Ok(())
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Python `rename_title`: rename the displayed title without
/// invalidating playback identities.
///
/// # Errors
/// Returns Python's validation message for a bad title, plus the
/// checked-path and preservation errors.
/// # Panics
/// Panics if a per-directory library lock is poisoned.
pub fn rename_title(
    directory: &Path,
    row: &LibraryRow,
    title: &str,
    active_paths: &[PathBuf],
) -> Result<(), String> {
    let title = title.trim();
    if title.is_empty() || title.chars().count() > 240 || title.chars().any(|c| (c as u32) < 32) {
        return Err("Enter a recording title of 1 to 240 characters.".to_string());
    }
    let lock = lock_for(directory);
    let _guard = lock.lock().unwrap();
    let path = checked_path(directory, row, active_paths)?;
    let parent = path.parent().unwrap_or(Path::new("."));
    let mut warnings = Vec::new();
    let mut rows = read_index(parent, true, &mut warnings)?;
    let entry = serde_json::json!({
        "title": title,
        "channel": row.channel,
        "duration": row.duration,
        "status": row.status,
        "size": row.size,
        "modified_ns": row.modified_ns,
    });
    let name = file_name(&path);
    if let Some(slot) = rows.iter_mut().find(|(key, _)| *key == name) {
        slot.1 = entry;
    } else {
        rows.push((name, entry));
    }
    write_index(parent, &rows)
}

// ---------------------------------------------------------------------
// Trash.

fn trash_root(directory: &Path, create: bool) -> Result<PathBuf, String> {
    let root = resolve(directory);
    let trash = root.join(TRASH_DIR_NAME);
    let unavailable = || "The recording trash folder is not available.".to_string();
    if let Ok(meta) = fs::symlink_metadata(&trash) {
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Err(unavailable());
        }
        if fs::canonicalize(&trash).ok().as_deref() != Some(trash.as_path()) {
            return Err(unavailable());
        }
    }
    if create {
        fs::create_dir_all(&trash).map_err(|error| error.to_string())?;
    }
    Ok(trash)
}

fn entry_json(row: &LibraryRow) -> Value {
    serde_json::json!({
        "title": row.title,
        "channel": row.channel,
        "duration": row.duration,
        "status": row.status,
        "size": row.size,
        "modified_ns": row.modified_ns,
    })
}

/// Python `trash`: journal the metadata, then atomically move
/// the media into a fresh trash subfolder. Never erases media.
///
/// # Errors
/// Propagates the checked-path and move errors; a failed move
/// leaves the source in place.
/// # Panics
/// Panics if a per-directory library lock is poisoned.
pub fn trash(directory: &Path, row: &LibraryRow, active_paths: &[PathBuf]) -> Result<(), String> {
    let lock = lock_for(directory);
    let _guard = lock.lock().unwrap();
    let path = checked_path(directory, row, active_paths)?;
    let target = trash_root(directory, true)?.join(random_hex());
    fs::create_dir(&target).map_err(|error| error.to_string())?;
    let name = file_name(&path);
    let journal = vec![
        ("file".to_string(), Value::String(name.clone())),
        ("entry".to_string(), entry_json(row)),
    ];
    // `_write` writes one JSON object; the manifest's keys are
    // "file" and "entry", not an index — write it directly.
    let outcome = write_manifest(&target, &journal)
        .and_then(|()| fs::rename(&path, target.join(&name)).map_err(|e| e.to_string()));
    if let Err(error) = outcome {
        let _ = fs::remove_file(target.join(INDEX_NAME));
        let _ = fs::remove_dir(&target);
        return Err(error);
    }
    Ok(())
}

fn write_manifest(directory: &Path, entries: &[(String, Value)]) -> Result<(), String> {
    write_index(directory, &entries.to_vec())
}

/// Python `scan_trash`: the recoverable recordings in a folder's
/// trash, manifests validated against the real files.
///
/// # Errors
/// Returns the trash-folder error when the trash root itself is
/// not available.
#[allow(clippy::cast_possible_wrap)] // file sizes fit an i64 in practice
pub fn scan_trash(
    directory: &Path,
    canceled: &dyn Fn() -> bool,
    warnings: &mut Vec<String>,
) -> Result<Vec<LibraryRow>, String> {
    let root = trash_root(directory, false)?;
    if !root.exists() {
        return Ok(Vec::new());
    }
    let entries = fs::read_dir(&root).map_err(|error| error.to_string())?;
    let mut rows = Vec::new();
    for entry in entries {
        if canceled() {
            return Ok(Vec::new());
        }
        let Ok(entry) = entry else { continue };
        let folder = entry.path();
        let id = entry.file_name().to_string_lossy().into_owned();
        let Ok(meta) = fs::symlink_metadata(&folder) else {
            continue;
        };
        if meta.file_type().is_symlink()
            || !meta.is_dir()
            || id.len() != 32
            || !id
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
            || fs::canonicalize(&folder).ok().as_deref() != Some(folder.as_path())
        {
            continue;
        }
        let manifest = read_index(&folder, false, warnings).unwrap_or_default();
        let Some(name) = index_get(&manifest, "file").and_then(Value::as_str) else {
            continue;
        };
        let Some(info) = index_get(&manifest, "entry").filter(|value| value.is_object()) else {
            continue;
        };
        if Path::new(name).file_name().and_then(|n| n.to_str()) != Some(name)
            || name == "."
            || name == ".."
        {
            continue;
        }
        let path = folder.join(name);
        if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink())
            || fs::canonicalize(&path).ok().as_deref() != Some(path.as_path())
            || !path.is_file()
        {
            continue;
        }
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        if info_number(info, "size") != Some(metadata.len() as i64)
            || info_number(info, "modified_ns") != Some(modified_ns(&metadata))
        {
            continue;
        }
        let info = clean_info(info);
        rows.push(LibraryRow {
            path: path.to_string_lossy().into_owned(),
            title: info
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            channel: info
                .get("channel")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            duration: info.get("duration").and_then(Value::as_f64),
            status: info
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
            size: metadata.len(),
            modified_ns: modified_ns(&metadata),
            date: date_text(modified_ns(&metadata) / 1_000_000_000),
            directory: resolve(directory).to_string_lossy().into_owned(),
            trash_id: Some(id),
            original_name: Some(name.to_string()),
        });
    }
    rows.sort_by(|a, b| {
        b.modified_ns
            .cmp(&a.modified_ns)
            .then_with(|| a.path.cmp(&b.path))
    });
    Ok(rows)
}

fn checked_trash(directory: &Path, row: &LibraryRow) -> Result<(PathBuf, PathBuf), String> {
    let root = trash_root(directory, false)?;
    let identity = row.trash_id.clone().unwrap_or_default();
    let unavailable = || "The recording file is no longer available.".to_string();
    if identity.len() != 32
        || !identity
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
    {
        return Err(unavailable());
    }
    let folder = root.join(&identity);
    if fs::symlink_metadata(&folder).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err(unavailable());
    }
    let current = scan_trash(directory, &|| false, &mut Vec::new())?
        .into_iter()
        .find(|item| item.trash_id.as_deref() == Some(identity.as_str()));
    match current {
        Some(current) if current == *row => Ok((folder, PathBuf::from(&current.path))),
        _ => Err("The recording file changed. Refresh the library.".to_string()),
    }
}

fn cleanup_trash_folder(folder: &Path) {
    let _ = fs::remove_file(folder.join(INDEX_NAME));
    let _ = fs::remove_dir(folder);
}

/// Python `restore`: bring a trashed recording back without ever
/// replacing an existing file.
///
/// # Errors
/// Returns Python's message when a file with the same name
/// already exists, plus the checked-trash and copy errors.
/// # Panics
/// Panics if a per-directory library lock is poisoned.
#[allow(clippy::cast_sign_loss)] // stored nanosecond stamps are non-negative
pub fn restore(directory: &Path, row: &LibraryRow) -> Result<(), String> {
    let lock = lock_for(directory);
    let _guard = lock.lock().unwrap();
    let (folder, path) = checked_trash(directory, row)?;
    let destination = resolve(directory).join(file_name(&path));
    if destination.exists()
        || fs::symlink_metadata(&destination).is_ok_and(|m| m.file_type().is_symlink())
    {
        return Err("A file with this name already exists in the recording folder.".to_string());
    }
    let parent = destination.parent().unwrap_or(Path::new("."));
    let mut warnings = Vec::new();
    let mut index = read_index(parent, true, &mut warnings)?;
    let name = file_name(&destination);
    let entry = entry_json(row);
    if let Some(slot) = index.iter_mut().find(|(key, _)| *key == name) {
        slot.1 = entry;
    } else {
        index.push((name, entry));
    }
    write_index(parent, &index)?;
    match fs::hard_link(&path, &destination) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(error.to_string());
        }
        Err(_) => {
            // Exclusive copy fallback for filesystems without
            // hard links (Python's `open(destination, "xb")`).
            let copy = (|| -> Result<(), String> {
                let mut source = fs::File::open(&path).map_err(|e| e.to_string())?;
                let mut target = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&destination)
                    .map_err(|e| e.to_string())?;
                std::io::copy(&mut source, &mut target).map_err(|e| e.to_string())?;
                target.sync_all().map_err(|e| e.to_string())?;
                let mtime = UNIX_EPOCH + std::time::Duration::from_nanos(row.modified_ns as u64);
                target.set_modified(mtime).map_err(|e| e.to_string())?;
                Ok(())
            })();
            if let Err(error) = copy {
                let _ = fs::remove_file(&destination);
                return Err(error);
            }
        }
    }
    if let Err(error) = fs::remove_file(&path) {
        let _ = fs::remove_file(&destination);
        return Err(error.to_string());
    }
    cleanup_trash_folder(&folder);
    Ok(())
}

/// Python `delete_trashed`: permanently remove a trashed file.
///
/// # Errors
/// Propagates the checked-trash and unlink errors.
/// # Panics
/// Panics if a per-directory library lock is poisoned.
pub fn delete_trashed(directory: &Path, row: &LibraryRow) -> Result<(), String> {
    let lock = lock_for(directory);
    let _guard = lock.lock().unwrap();
    let (folder, path) = checked_trash(directory, row)?;
    fs::remove_file(&path).map_err(|error| error.to_string())?;
    cleanup_trash_folder(&folder);
    Ok(())
}

// ---------------------------------------------------------------------
// Folders, playback, resume, queries.

/// Python `folders`: the primary recordings directory (resolved
/// by the caller, as the app owns the default) followed by the
/// configured `recording_library_folders`, deduplicated by real
/// path and capped at 20.
#[must_use]
pub fn folders(config: &Value, primary: &str) -> Vec<String> {
    let mut values = vec![primary.to_string()];
    if let Some(extra) = config
        .get("recording_library_folders")
        .and_then(Value::as_array)
    {
        values.extend(
            extra
                .iter()
                .filter_map(Value::as_str)
                .filter(|path| !path.contains('\0'))
                .map(str::to_string),
        );
    }
    let mut result = Vec::new();
    let mut seen = Vec::new();
    for path in values {
        if path.trim().is_empty() {
            continue;
        }
        let absolute = absolute_normalized(Path::new(&path));
        let key = norm_key(&absolute);
        if !seen.contains(&key) {
            seen.push(key);
            result.push(absolute.to_string_lossy().into_owned());
        }
    }
    result.truncate(MAX_FOLDERS);
    result
}

/// Python `folders(display_only=True)`: the same list, but
/// deduplicated by the lexical absolute path instead of the
/// resolved real path, so showing the list never touches the
/// filesystem (an offline share cannot block the dialog).
#[must_use]
pub fn folders_display(config: &Value, primary: &str) -> Vec<String> {
    let mut values = vec![primary.to_string()];
    if let Some(extra) = config
        .get("recording_library_folders")
        .and_then(Value::as_array)
    {
        values.extend(
            extra
                .iter()
                .filter_map(Value::as_str)
                .filter(|path| !path.contains('\0'))
                .map(str::to_string),
        );
    }
    let mut result = Vec::new();
    let mut seen = Vec::new();
    for path in values {
        if path.trim().is_empty() {
            continue;
        }
        let absolute = lexical_absolute(Path::new(&path));
        let key = norm_key(&absolute);
        if !seen.contains(&key) {
            seen.push(key);
            result.push(absolute.to_string_lossy().into_owned());
        }
    }
    result.truncate(MAX_FOLDERS);
    result
}

/// A `file://` URI for an absolute path (Python `Path.as_uri`).
#[must_use]
pub fn file_uri(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let slashed = if text.len() >= 2 && text.as_bytes()[1] == b':' {
        format!("/{text}")
    } else {
        text
    };
    let mut uri = String::from("file://");
    for byte in slashed.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b':' | b'~' | b'-' | b'.' | b'_') {
            uri.push(byte as char);
        } else {
            use std::fmt::Write;
            let _ = write!(uri, "%{byte:02X}");
        }
    }
    uri
}

/// Python `playback_channel`: the playable channel for a library
/// row, its resume identity bound to the file's exact signature
/// so a replaced file never inherits the position.
///
/// # Errors
/// Propagates the checked-path errors.
pub fn playback_channel(
    directory: &Path,
    row: &LibraryRow,
    active_paths: &[PathBuf],
) -> Result<Channel, String> {
    let path = checked_path(directory, row, active_paths)?;
    let mut channel = Channel {
        name: row.title.clone(),
        group: String::new(),
        url: file_uri(&path),
        tvg_id: String::new(),
        tvg_name: String::new(),
        tvg_logo: String::new(),
        unknown_attributes: std::collections::BTreeMap::new(),
    };
    channel.unknown_attributes.insert(
        "provider-id".to_string(),
        format!("local-recording:{}:{}", row.size, row.modified_ns),
    );
    channel.unknown_attributes.insert(
        "local-recording".to_string(),
        path.to_string_lossy().into_owned(),
    );
    Ok(channel)
}

/// Python `problem_log` (C7): the log for a library row, only
/// when its own summary footer identifies this media file.
/// The summary header is found by scanning backwards in
/// blocks so an ordinary log reads one small tail block.
/// A `.part` base falls back to the final file's log name.
/// Legacy summaries (no `# Media signature:` line) are checked
/// by their started/ended wall-clock window instead.
///
/// # Errors
/// `No matching recording log is available for this file.`
/// for every refusal, plus the checked-path errors.
pub fn problem_log(
    directory: &Path,
    row: &LibraryRow,
    active_paths: &[PathBuf],
) -> Result<PathBuf, String> {
    use std::io::Read as _;
    use std::io::{Seek, SeekFrom};

    let unavailable = || "No matching recording log is available for this file.".to_string();
    let path = checked_path(directory, row, active_paths)?;
    let base = if path
        .extension()
        .is_some_and(|ext| ext.to_string_lossy().eq_ignore_ascii_case("part"))
    {
        path.with_extension("")
    } else {
        path.clone()
    };
    let parent = path.parent().unwrap_or(Path::new("."));
    let log = crate::recording_log::recording_log_path(parent, &base);
    if fs::symlink_metadata(&log).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err(unavailable());
    }
    // Python `log.resolve() != log` (symlink rejection). The
    // lexical absolute form is `log` itself; canonicalize
    // follows a link, so a mismatch names a symlink.
    let resolved = fs::canonicalize(&log).map_err(|_| unavailable())?;
    if resolved != absolute_normalized(&log) {
        return Err(unavailable());
    }
    if !log.is_file() {
        return Err(unavailable());
    }
    // Find the summary header: scan backwards in blocks,
    // keeping an overlap of marker length - 1 bytes.
    let marker = "# ===== Recording summary =====".as_bytes();
    let mut file = fs::File::open(&log).map_err(|_| unavailable())?;
    let end_len = file
        .metadata()
        .map(|meta| meta.len())
        .map_err(|_| unavailable())?;
    let mut end = end_len;
    let mut overlap: Vec<u8> = Vec::new();
    let header: Vec<String> = loop {
        if end == 0 {
            return Err(unavailable());
        }
        let start = end.saturating_sub(65_536);
        file.seek(SeekFrom::Start(start))
            .map_err(|_| unavailable())?;
        let len = usize::try_from(end - start).map_err(|_| unavailable())?;
        let mut data = vec![0_u8; len];
        file.read_exact(&mut data).map_err(|_| unavailable())?;
        data.extend_from_slice(&overlap);
        if let Some(offset) = rfind(&data, marker) {
            file.seek(SeekFrom::Start(start + offset as u64))
                .map_err(|_| unavailable())?;
            let mut lines = Vec::new();
            let mut text = String::new();
            file.read_to_string(&mut text).map_err(|_| unavailable())?;
            for line in text.lines().take(5) {
                lines.push(line.trim_end().to_string());
                if line == "# ===== End of summary =====" {
                    break;
                }
            }
            break lines;
        }
        let keep = marker.len() - 1;
        overlap = data.iter().rev().take(keep).rev().copied().collect();
        end = start;
    };
    if header.len() < 3 || !header[1].starts_with("# File: ") {
        return Err(unavailable());
    }
    let recorded = PathBuf::from(header[1]["# File: ".len()..].trim());
    if fs::canonicalize(&recorded).unwrap_or(recorded.clone()) != path
        && absolute_normalized(&recorded) != absolute_normalized(&path)
    {
        return Err(unavailable());
    }
    if header[2].starts_with("# Media signature: ") {
        let expected = format!("# Media signature: {} {}", row.size, row.modified_ns);
        if header[2] != expected {
            return Err(unavailable());
        }
        return Ok(log);
    }
    // Legacy: compare local wall times, including both folds
    // of a clock change, as Python does (the earliest/latest
    // local interpretations bracket the window).
    let started = parse_log_stamp(&header[2]).ok_or_else(unavailable)?;
    let ended_line = header.get(3).ok_or_else(unavailable)?;
    let ended_text = ended_line
        .split(" (ran for ")
        .next()
        .unwrap_or(ended_line.as_str());
    let ended = parse_log_stamp(ended_text).ok_or_else(unavailable)?;
    #[allow(clippy::cast_precision_loss)] // wall-clock window comparison, as Python's float
    let modified = row.modified_ns as f64 / 1_000_000_000.0;
    let earliest = started.min(ended);
    let latest = started.max(ended);
    let log_mtime_ns = fs::metadata(&log)
        .map_err(|_| unavailable())?
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| {
            i64::try_from(duration.as_nanos()).unwrap_or(i64::MAX)
        });
    if !(earliest - 2.0 <= modified && modified <= latest + 2.0) || log_mtime_ns < row.modified_ns {
        return Err(unavailable());
    }
    Ok(log)
}

fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .rposition(|window| window == needle)
}

/// Parse a `# Started:` / `# Ended:` line into epoch seconds
/// (naive local, as Python's `strptime` + `timestamp`).
fn parse_log_stamp(line: &str) -> Option<f64> {
    use chrono::TimeZone as _;
    let stamp = line
        .strip_prefix("# Started: ")
        .or_else(|| line.strip_prefix("# Ended: "))?;
    let naive = chrono::NaiveDateTime::parse_from_str(stamp, "%Y-%m-%d %H:%M:%S").ok()?;
    let epoch = chrono::Local
        .from_local_datetime(&naive)
        .earliest()?
        .timestamp();
    #[allow(clippy::cast_precision_loss)] // epoch seconds in a window comparison, as Python's float
    Some(epoch as f64)
}

/// Python `resolve_resume` (worker-only): find the library row
/// whose exact file signature matches a saved resume key.
/// Provider items are never resolved here — the caller checks
/// store membership first.
#[must_use]
pub fn resolve_resume(
    key: &str,
    entries: &Value,
    directories: &[String],
    active_paths: &[PathBuf],
) -> Option<Channel> {
    if !bookmarks::normalize(Some(entries)).contains_key(key) {
        return None;
    }
    for directory in directories {
        let mut warnings = Vec::new();
        let Ok(rows) = scan(Path::new(directory), active_paths, &|| false, &mut warnings) else {
            continue;
        };
        for row in &rows {
            let Ok(channel) = playback_channel(Path::new(directory), row, active_paths) else {
                continue;
            };
            if bookmarks::item_key(&channel) == key {
                return Some(channel);
            }
        }
    }
    None
}

/// Python `query_rows`: pure filter + sort over scanned rows;
/// the input is not mutated.
#[must_use]
pub fn query_rows(
    rows: &[LibraryRow],
    query: &str,
    status: &str,
    sort: &str,
    descending: bool,
) -> Vec<LibraryRow> {
    let needle = query.trim().to_lowercase();
    let mut result: Vec<LibraryRow> = rows
        .iter()
        .filter(|row| {
            (status.is_empty() || row.status == status)
                && (needle.is_empty()
                    || format!(
                        "{} {} {} {}",
                        row.title, row.channel, row.date, row.directory
                    )
                    .to_lowercase()
                    .contains(&needle))
        })
        .cloned()
        .collect();
    result.sort_by(|a, b| {
        let ordering = match sort {
            "title" => a
                .title
                .to_lowercase()
                .cmp(&b.title.to_lowercase())
                .then_with(|| a.path.cmp(&b.path)),
            "size" => a.size.cmp(&b.size).then_with(|| a.path.cmp(&b.path)),
            "duration" => a
                .duration
                .unwrap_or(-1.0)
                .partial_cmp(&b.duration.unwrap_or(-1.0))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.path.cmp(&b.path)),
            _ => a
                .modified_ns
                .cmp(&b.modified_ns)
                .then_with(|| a.path.cmp(&b.path)),
        };
        if descending {
            ordering.reverse()
        } else {
            ordering
        }
    });
    result
}

// ---------------------------------------------------------------------
// Remembering finished recordings.

/// What [`classify_outcome`] reads about a finished capture.
pub struct Outcome<'a> {
    /// The output file exists and is not empty.
    pub produced: bool,
    /// The app was exiting, or ffmpeg had to be killed before it
    /// finished the file (Python `exiting`, `finalize_timed_out`).
    pub interrupted: bool,
    /// Why the engine stopped it: `user`, `scheduled`, `storage`,
    /// or empty when ffmpeg ended by itself.
    pub stop_reason: &'a str,
    pub stopped_by_user: bool,
    pub exit_code: i32,
    pub written_seconds: Option<f64>,
    pub planned_seconds: Option<f64>,
}

/// Python `recording_outcome.classify`: nothing produced is
/// `failed`; an interrupted capture or a low-space stop is
/// `partial`; a user stop (not the scheduler's end-of-window stop)
/// is `stopped`; a nonzero exit or a shortfall against the planned
/// length beyond max(5, min(30, 2 %)) is `partial`; else `completed`.
#[must_use]
pub fn classify_outcome(outcome: &Outcome<'_>) -> &'static str {
    if !outcome.produced {
        return "failed";
    }
    if outcome.interrupted {
        return "partial";
    }
    // PR #44: a capture stopped because the app exited keeps
    // its real outcome — "stopped" when ffmpeg finished the
    // file cleanly, "partial" when it did not.
    if outcome.stop_reason == "exit" {
        return if outcome.exit_code == 0 {
            "stopped"
        } else {
            "partial"
        };
    }
    if outcome.stop_reason == "storage" {
        return "partial";
    }
    if outcome.stopped_by_user && outcome.stop_reason != "scheduled" {
        return "stopped";
    }
    if outcome.exit_code != 0 {
        return "partial";
    }
    if let (Some(expected), Some(written)) = (outcome.planned_seconds, outcome.written_seconds) {
        if expected.is_finite() && written.is_finite() && expected > 0.0 {
            let slack = (expected * 0.02).clamp(5.0, 30.0);
            if written + slack < expected {
                return "partial";
            }
        }
    }
    "completed"
}

/// What `remember` needs about a finished recording (Python's
/// recorder record, projected).
pub struct RememberInput {
    pub out_path: PathBuf,
    pub title: String,
    pub channel_name: String,
    pub media_seconds: Option<f64>,
    pub outcome: Option<String>,
    pub produced: bool,
    pub stopped_by_user: bool,
    pub stop_reason: String,
    pub interrupted: bool,
    pub exit_code: i32,
    pub planned_seconds: Option<f64>,
}

/// Python `remember`: index a finished recording in its folder.
/// A missing or symlinked output is silently skipped.
///
/// # Errors
/// Propagates the corrupt-index preservation failure and the
/// index write error, as Python's mutating callers abort.
/// # Panics
/// Panics if a per-directory library lock is poisoned.
pub fn remember(input: &RememberInput) -> Result<(), String> {
    let path = &input.out_path;
    let Ok(meta) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Ok(());
    }
    let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
    let seconds = input
        .media_seconds
        .filter(|value| value.is_finite() && *value >= 0.0);
    let status = input.outcome.clone().unwrap_or_else(|| {
        classify_outcome(&Outcome {
            produced: input.produced,
            interrupted: input.interrupted,
            stop_reason: &input.stop_reason,
            stopped_by_user: input.stopped_by_user,
            exit_code: input.exit_code,
            written_seconds: seconds,
            planned_seconds: input.planned_seconds,
        })
        .to_string()
    });
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let title = {
        let cleaned = safe_title(&input.title);
        if cleaned.is_empty() {
            stem
        } else {
            cleaned
        }
    };
    let entry = serde_json::json!({
        "title": title,
        "channel": safe_title(&input.channel_name),
        "duration": seconds,
        "size": metadata.len(),
        "modified_ns": modified_ns(&metadata),
        "status": status,
    });
    let parent = path.parent().unwrap_or(Path::new("."));
    let lock = lock_for(parent);
    let _guard = lock.lock().unwrap();
    let mut warnings = Vec::new();
    let mut rows = read_index(parent, true, &mut warnings)?;
    let name = file_name(path);
    rows.retain(|(key, _)| key != &name);
    rows.push((name, entry));
    if rows.len() > 10_000 {
        rows.drain(..rows.len() - 10_000);
    }
    write_index(parent, &rows)
}

/// One CSV cell with spreadsheet formula protection (Python
/// `planning_tools.csv_cell`): quoting does not stop formula
/// execution, so a value whose first non-space character is
/// one of = + - @, or that starts with a tab or newline,
/// gains a leading apostrophe.
#[must_use]
pub fn csv_cell(value: &str) -> String {
    let trimmed = value.trim_start();
    if trimmed.starts_with(['=', '+', '-', '@']) || value.starts_with(['\t', '\r', '\n']) {
        format!("'{value}")
    } else {
        value.to_string()
    }
}

/// One field in Python's csv module default dialect:
/// quote when the field contains a comma, quote, CR or LF;
/// internal quotes double.
fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

/// The Recording Library CSV export (Python
/// `planning_tools.recording_csv`): a UTF-8 BOM, then the
/// header and one row per library row, CRLF line endings.
/// No paths or URLs are included. The header and status
/// labels arrive translated from the app, as Python
/// translates them at write time: `header` is (Title,
/// Channel, Date, Duration (seconds), Status, Size
/// (bytes)); `status_labels` is (Completed, Partial,
/// Stopped by user, Failed, Unknown).
#[must_use]
pub fn recording_csv(
    rows: &[LibraryRow],
    header: &[&str; 6],
    status_labels: &[&str; 5],
) -> Vec<u8> {
    let mut out = String::from("\u{feff}");
    let mut line = |fields: &[String]| {
        let cells: Vec<String> = fields.iter().map(|f| csv_field(f)).collect();
        out.push_str(&cells.join(","));
        out.push_str("\r\n");
    };
    line(&header.iter().map(|h| (*h).to_string()).collect::<Vec<_>>());
    for row in rows {
        let status = match row.status.as_str() {
            "completed" => status_labels[0],
            "partial" => status_labels[1],
            "stopped" => status_labels[2],
            "failed" => status_labels[3],
            _ => status_labels[4],
        };
        line(&[
            csv_cell(&row.title),
            csv_cell(&row.channel),
            csv_cell(&row.date),
            row.duration.map_or_else(String::new, |d| format!("{d}")),
            status.to_string(),
            row.size.to_string(),
        ]);
    }
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn csv_row(title: &str, status: &str) -> LibraryRow {
        LibraryRow {
            path: "/secret/path.mkv".to_string(),
            title: title.to_string(),
            channel: "Chan, One".to_string(),
            duration: Some(90.0),
            status: status.to_string(),
            size: 2048,
            modified_ns: 0,
            date: "2026-11-01 20:00".to_string(),
            directory: "/secret".to_string(),
            trash_id: None,
            original_name: None,
        }
    }

    #[test]
    fn recording_csv_shape_protection_and_no_paths() {
        let header = [
            "Title",
            "Channel",
            "Date",
            "Duration (seconds)",
            "Status",
            "Size (bytes)",
        ];
        let labels = [
            "Completed",
            "Partial",
            "Stopped by user",
            "Failed",
            "Unknown",
        ];
        let mut second = csv_row("Plain", "weird");
        second.duration = None;
        let bytes = recording_csv(&[csv_row("=2+5", "stopped"), second], &header, &labels);
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.starts_with('\u{feff}'));
        let lines: Vec<&str> = text
            .strip_prefix('\u{feff}')
            .unwrap()
            .split("\r\n")
            .collect();
        assert_eq!(
            lines[0],
            "Title,Channel,Date,Duration (seconds),Status,Size (bytes)"
        );
        assert_eq!(
            lines[1],
            "'=2+5,\"Chan, One\",2026-11-01 20:00,90,Stopped by user,2048"
        );
        assert_eq!(
            lines[2],
            "Plain,\"Chan, One\",2026-11-01 20:00,,Unknown,2048"
        );
        assert!(!text.contains("/secret"));
    }

    #[test]
    fn csv_cell_prefix_rules() {
        assert_eq!(csv_cell("=x"), "'=x");
        assert_eq!(csv_cell("  +x"), "'  +x");
        assert_eq!(csv_cell("-x"), "'-x");
        assert_eq!(csv_cell("@x"), "'@x");
        assert_eq!(csv_cell("\tx"), "'\tx");
        assert_eq!(csv_cell(" x"), " x");
        assert_eq!(csv_cell(""), "");
    }
    use std::sync::mpsc;
    use std::time::Duration;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("iptv-library-test-{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn media_file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(bytes).unwrap();
        path
    }

    fn scan_one(dir: &Path) -> LibraryRow {
        let rows = scan(dir, &[], &|| false, &mut Vec::new()).unwrap();
        assert_eq!(rows.len(), 1, "rows: {rows:?}");
        rows.into_iter().next().unwrap()
    }

    #[test]
    fn corrupt_index_is_preserved_before_a_mutation() {
        let dir = temp_dir("corrupt");
        let media = media_file(&dir, "show.ts", b"media-bytes");
        let damaged = b"{\"original title\": ";
        fs::write(dir.join(INDEX_NAME), damaged).unwrap();
        let row = LibraryRow {
            path: media.to_string_lossy().into_owned(),
            title: "show".to_string(),
            channel: String::new(),
            duration: None,
            status: "unknown".to_string(),
            size: 11,
            modified_ns: modified_ns(&fs::metadata(&media).unwrap()),
            date: String::new(),
            directory: dir.to_string_lossy().into_owned(),
            trash_id: None,
            original_name: None,
        };
        rename_title(&dir, &row, "Recovered Title", &[]).unwrap();
        let backups: Vec<PathBuf> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().contains(".corrupt-"))
            })
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(fs::read(&backups[0]).unwrap(), damaged);
        assert_eq!(fs::read(&media).unwrap(), b"media-bytes");
        // And the rename landed in a fresh, valid index.
        let rows = scan(&dir, &[], &|| false, &mut Vec::new()).unwrap();
        assert_eq!(rows[0].title, "Recovered Title");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn malformed_info_is_sanitised_on_scan() {
        let dir = temp_dir("clean");
        let media = media_file(&dir, "clip.mp4", b"12345");
        let metadata = fs::metadata(&media).unwrap();
        let index = serde_json::json!({
            "clip.mp4": {
                "title": "Clip", "channel": "Chan", "status": [],
                "duration": "not-a-number", "size": metadata.len(),
                "modified_ns": modified_ns(&metadata),
            }
        });
        fs::write(dir.join(INDEX_NAME), serde_json::to_string(&index).unwrap()).unwrap();
        let row = scan_one(&dir);
        assert_eq!(row.status, "unknown");
        assert_eq!(row.duration, None);
        assert_eq!(row.title, "Clip");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn oversize_index_reads_as_corrupt() {
        let dir = temp_dir("oversize");
        media_file(&dir, "big.ts", b"x");
        let mut warnings = Vec::new();
        let index = read_index(&dir, false, &mut warnings).unwrap();
        assert!(index.is_empty());
        // Write an index beyond the cap: scan must warn and treat
        // every row as unindexed rather than trusting it.
        let padding = " ".repeat(MAX_INDEX_BYTES + 16);
        fs::write(dir.join(INDEX_NAME), format!("{{{padding}}}")).unwrap();
        let mut warnings = Vec::new();
        let rows = scan(&dir, &[], &|| false, &mut warnings).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(warnings, [METADATA_WARNING.to_string()]);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn per_directory_locks_do_not_block_each_other() {
        let dir_a = temp_dir("lock-a");
        let dir_b = temp_dir("lock-b");
        media_file(&dir_b, "b.ts", b"bb");
        let row_b = scan_one(&dir_b);
        let lock_a = lock_for(&dir_a);
        let guard_a = lock_a.lock().unwrap();
        let (tx, rx) = mpsc::channel();
        let dir_b2 = dir_b.clone();
        let worker = std::thread::spawn(move || {
            let outcome = rename_title(&dir_b2, &row_b, "Bee", &[]);
            tx.send(outcome).unwrap();
        });
        let outcome = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("folder B rename blocked behind folder A's lock");
        outcome.unwrap();
        drop(guard_a);
        worker.join().unwrap();
        fs::remove_dir_all(&dir_a).ok();
        fs::remove_dir_all(&dir_b).ok();
    }

    #[test]
    fn query_rows_filters_sorts_and_stays_pure() {
        let row = |path: &str, size: u64, duration: Option<f64>, status: &str| LibraryRow {
            path: path.to_string(),
            title: format!("Title {path}"),
            channel: String::new(),
            duration,
            status: status.to_string(),
            size,
            modified_ns: 0,
            date: String::new(),
            directory: String::new(),
            trash_id: None,
            original_name: None,
        };
        let rows = vec![
            row("a", 100, Some(5.0), "completed"),
            row("b", 300, None, "completed"),
            row("c", 200, Some(9.0), "failed"),
        ];
        let before = rows.clone();
        let sized = query_rows(&rows, "", "completed", "size", true);
        assert_eq!(sized.iter().map(|r| r.size).collect::<Vec<_>>(), [300, 100]);
        assert_eq!(rows, before, "query_rows must not mutate its input");

        let mut accented = row("x", 1, None, "completed");
        accented.title = "Árvíz".to_string();
        let found = query_rows(&[accented], "ÁRVÍZ", "", "date", true);
        assert_eq!(found.len(), 1);

        let durations = vec![
            row("1", 0, Some(3.0), "completed"),
            row("2", 0, None, "completed"),
            row("3", 0, Some(1.0), "completed"),
        ];
        let ordered = query_rows(&durations, "", "", "duration", false);
        assert_eq!(
            ordered.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(),
            ["2", "3", "1"],
            "None duration sorts as -1, first when ascending"
        );
    }

    #[test]
    fn trash_restore_and_delete_round_trip() {
        let dir = temp_dir("trash");
        media_file(&dir, "movie.mkv", b"movie");
        let row = scan_one(&dir);
        trash(&dir, &row, &[]).unwrap();
        assert!(!Path::new(&row.path).exists());
        assert!(scan(&dir, &[], &|| false, &mut Vec::new())
            .unwrap()
            .is_empty());
        let trashed = scan_trash(&dir, &|| false, &mut Vec::new()).unwrap();
        assert_eq!(trashed.len(), 1);
        assert_eq!(trashed[0].title, row.title);
        // Restore refuses to overwrite an existing destination.
        media_file(&dir, "movie.mkv", b"other");
        let conflict = restore(&dir, &trashed[0]).unwrap_err();
        assert!(conflict.contains("already exists"), "{conflict}");
        fs::remove_file(dir.join("movie.mkv")).unwrap();
        restore(&dir, &trashed[0]).unwrap();
        assert_eq!(fs::read(dir.join("movie.mkv")).unwrap(), b"movie");
        assert!(scan_trash(&dir, &|| false, &mut Vec::new())
            .unwrap()
            .is_empty());
        // Trash again and delete permanently.
        let row = scan_one(&dir);
        trash(&dir, &row, &[]).unwrap();
        let trashed = scan_trash(&dir, &|| false, &mut Vec::new()).unwrap();
        delete_trashed(&dir, &trashed[0]).unwrap();
        assert!(scan_trash(&dir, &|| false, &mut Vec::new())
            .unwrap()
            .is_empty());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_resume_needs_the_exact_file_signature() {
        let dir = temp_dir("resume");
        media_file(&dir, "ep.ts", b"episode-bytes");
        let row = scan_one(&dir);
        let channel = playback_channel(&dir, &row, &[]).unwrap();
        let key = bookmarks::item_key(&channel);
        assert!(!key.is_empty());
        let entries = serde_json::json!({
            key.clone(): {"position_ms": 60_000, "length_ms": 600_000, "updated": 1}
        });
        let directories = vec![dir.to_string_lossy().into_owned()];
        let found = resolve_resume(&key, &entries, &directories, &[]);
        assert!(found.is_some());
        // Replace the file: same path, different signature.
        media_file(&dir, "ep.ts", b"episode-bytes-but-longer");
        let found = resolve_resume(&key, &entries, &directories, &[]);
        assert!(found.is_none());
        fs::remove_dir_all(&dir).ok();
    }

    /// Every branch of Python `recording_outcome.classify`, in its
    /// order.
    #[test]
    fn classify_outcome_follows_python() {
        let c = |produced, interrupted, reason, user, rc, written: Option<f64>, planned| {
            classify_outcome(&Outcome {
                produced,
                interrupted,
                stop_reason: reason,
                stopped_by_user: user,
                exit_code: rc,
                written_seconds: written,
                planned_seconds: planned,
            })
        };
        assert_eq!(c(false, true, "user", true, 0, None, None), "failed");
        assert_eq!(c(true, true, "", false, 0, None, None), "partial");
        assert_eq!(c(true, false, "storage", true, 0, None, None), "partial");
        // PR #44: an exit stop keeps its real outcome.
        assert_eq!(c(true, false, "exit", true, 0, None, None), "stopped");
        assert_eq!(c(true, false, "exit", true, 1, None, None), "partial");
        // A capture that had to be killed stays partial even on exit.
        assert_eq!(c(true, true, "exit", true, 0, None, None), "partial");
        assert_eq!(c(true, false, "user", true, 1, None, None), "stopped");
        // The scheduler's own end-of-window stop is not a user stop.
        assert_eq!(
            c(
                true,
                false,
                "scheduled",
                true,
                0,
                Some(3600.0),
                Some(3600.0)
            ),
            "completed"
        );
        assert_eq!(
            c(true, false, "scheduled", true, 255, None, None),
            "partial"
        );
        assert_eq!(c(true, false, "", false, 1, None, None), "partial");
        // Shortfall tolerance max(5, min(30, 2 %)).
        assert_eq!(
            c(true, false, "", false, 0, Some(3575.0), Some(3600.0)),
            "completed"
        );
        assert_eq!(
            c(true, false, "", false, 0, Some(3520.0), Some(3600.0)),
            "partial"
        );
        assert_eq!(
            c(true, false, "", false, 0, Some(95.0), Some(100.0)),
            "completed"
        );
        assert_eq!(
            c(true, false, "", false, 0, Some(94.0), Some(100.0)),
            "partial"
        );
        assert_eq!(c(true, false, "", false, 0, None, Some(100.0)), "completed");
    }

    #[test]
    fn remember_indexes_a_finished_recording() {
        let dir = temp_dir("remember");
        let out = media_file(&dir, "rec.ts", b"recorded");
        remember(&RememberInput {
            out_path: out.clone(),
            title: "The Rec".to_string(),
            channel_name: "Chan One".to_string(),
            media_seconds: Some(120.0),
            outcome: None,
            produced: true,
            stopped_by_user: false,
            stop_reason: String::new(),
            interrupted: false,
            exit_code: 0,
            planned_seconds: Some(120.0),
        })
        .unwrap();
        let row = scan_one(&dir);
        assert_eq!(row.title, "The Rec");
        assert_eq!(row.channel, "Chan One");
        assert_eq!(row.duration, Some(120.0));
        assert_eq!(row.status, "completed");
        // A user stop classifies as stopped.
        remember(&RememberInput {
            out_path: out,
            title: "The Rec".to_string(),
            channel_name: "Chan One".to_string(),
            media_seconds: Some(30.0),
            outcome: None,
            produced: true,
            stopped_by_user: true,
            stop_reason: "user".to_string(),
            interrupted: false,
            exit_code: 1,
            planned_seconds: Some(120.0),
        })
        .unwrap();
        assert_eq!(scan_one(&dir).status, "stopped");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn folders_dedupes_and_caps() {
        let config = serde_json::json!({
            "recording_library_folders": ["/data/b", "/data/b", " ", "/data/c"]
        });
        let result = folders(&config, "/data/a");
        assert_eq!(result.len(), 3);
        // The absolutized paths use the platform separator
        // (backslashes on Windows), so compare normalized.
        let normalized: Vec<String> = result.iter().map(|path| path.replace('\\', "/")).collect();
        assert!(normalized[0].ends_with("data/a"));
        assert!(normalized[1].ends_with("data/b"));
        assert!(normalized[2].ends_with("data/c"));
    }

    #[test]
    fn file_uri_percent_encodes_like_python() {
        assert_eq!(file_uri(Path::new("/tmp/plain.ts")), "file:///tmp/plain.ts");
        assert_eq!(
            file_uri(Path::new("/tmp/a b/ü.ts")),
            "file:///tmp/a%20b/%C3%BC.ts"
        );
    }

    /// C7 `problem_log`: a legacy summary whose window covers
    /// the file's mtime resolves; a different file, a replaced
    /// file outside the window, and a missing log all refuse.
    #[test]
    fn problem_log_checks_the_summary_footer() {
        let dir = temp_dir("problem-log");
        media_file(&dir, "show.ts", b"media-bytes");
        let row = scan_one(&dir);
        let out_path = dir.join("show.ts");
        let now = chrono::Local::now().timestamp();
        let stamp = |epoch: i64| {
            chrono::DateTime::from_timestamp(epoch, 0)
                .unwrap()
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        };
        let logs = dir.join("logs");
        fs::create_dir_all(&logs).unwrap();
        let log_path = logs.join("show.log");
        let write_log = |file_line: &str, started: i64, ended: i64| {
            std::fs::write(
                &log_path,
                format!(
                    "2026-01-01 00:00:00 [error] boom\n# ===== Recording summary =====\n# File: {file_line}\n# Started: {}\n# Ended: {} (ran for 0:01:00)\n# How it ended: ok\n",
                    stamp(started),
                    stamp(ended)
                ),
            )
            .unwrap();
        };
        // The row's mtime is now: a window around it resolves.
        write_log(&out_path.to_string_lossy(), now - 60, now + 60);
        assert_eq!(
            absolute_normalized(&problem_log(&dir, &row, &[]).unwrap()),
            absolute_normalized(&log_path)
        );
        // A summary naming another file refuses.
        write_log("/elsewhere/other.ts", now - 60, now + 60);
        assert!(problem_log(&dir, &row, &[]).is_err());
        // A window the file's mtime misses (replaced file) refuses.
        write_log(&out_path.to_string_lossy(), now - 3600, now - 3500);
        assert!(problem_log(&dir, &row, &[]).is_err());
        // No log at all refuses.
        fs::remove_file(&log_path).unwrap();
        assert_eq!(
            problem_log(&dir, &row, &[]).unwrap_err(),
            "No matching recording log is available for this file."
        );
        fs::remove_dir_all(&dir).ok();
    }

    /// C7 `problem_log`: Python's `# Media signature:` line is
    /// checked exactly when present.
    #[test]
    fn problem_log_checks_the_media_signature() {
        let dir = temp_dir("problem-sig");
        media_file(&dir, "show.ts", b"media-bytes");
        let row = scan_one(&dir);
        let out_path = dir.join("show.ts");
        let logs = dir.join("logs");
        fs::create_dir_all(&logs).unwrap();
        let log_path = logs.join("show.log");
        let expected = format!("# Media signature: {} {}", row.size, row.modified_ns);
        std::fs::write(
            &log_path,
            format!(
                "# ===== Recording summary =====\n# File: {}\n{expected}\n",
                out_path.to_string_lossy()
            ),
        )
        .unwrap();
        assert_eq!(
            absolute_normalized(&problem_log(&dir, &row, &[]).unwrap()),
            absolute_normalized(&log_path)
        );
        std::fs::write(
            &log_path,
            format!(
                "# ===== Recording summary =====\n# File: {}\n# Media signature: 1 2\n",
                out_path.to_string_lossy()
            ),
        )
        .unwrap();
        assert!(problem_log(&dir, &row, &[]).is_err());
        fs::remove_dir_all(&dir).ok();
    }
}
