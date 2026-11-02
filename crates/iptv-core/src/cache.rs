//! Small text cache on disk (provider playlists): keyed files,
//! freshness checks, atomic writes.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};

/// A stable cache key from identifying parts (hex SHA-256).
#[must_use]
pub fn cache_key(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update(b"\0");
    }
    format!("{:x}", hasher.finalize())
}

/// The file for a key inside a cache directory.
#[must_use]
pub fn file_for(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!("{key}.cache"))
}

/// Read a cache file when it exists and is fresh enough
/// (`max_age: None` accepts any age — the stale fallback).
#[must_use]
pub fn read(path: &Path, max_age: Option<Duration>, now: SystemTime) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    if let Some(max_age) = max_age {
        let modified = metadata.modified().ok()?;
        match now.duration_since(modified) {
            Ok(age) if age <= max_age => {}
            _ => return None,
        }
    }
    std::fs::read_to_string(path).ok()
}

/// Write a cache file atomically (temp file, then rename).
///
/// # Errors
/// Returns the OS error text when the write or rename fails.
pub fn write(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let temp = path.with_extension("tmp");
    std::fs::write(&temp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&temp, path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::{cache_key, file_for, read, write};
    use std::time::{Duration, SystemTime};

    #[test]
    fn keys_are_stable_and_distinct() {
        assert_eq!(cache_key(&["a", "b"]), cache_key(&["a", "b"]));
        assert_ne!(cache_key(&["a", "b"]), cache_key(&["a", "c"]));
        assert_ne!(cache_key(&["ab"]), cache_key(&["a", "b"]));
    }

    #[test]
    fn write_read_and_freshness() {
        let dir = std::env::temp_dir().join(format!("iptv-cache-test-{}", std::process::id()));
        let path = file_for(&dir, &cache_key(&["xtream", "server", "user"]));
        write(&path, "playlist").unwrap();
        let now = SystemTime::now();
        assert_eq!(
            read(&path, Some(Duration::from_mins(1)), now).as_deref(),
            Some("playlist")
        );
        let later = now + Duration::from_hours(1);
        assert_eq!(read(&path, Some(Duration::from_mins(1)), later), None);
        assert_eq!(read(&path, None, later).as_deref(), Some("playlist"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
