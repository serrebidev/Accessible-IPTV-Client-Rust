//! Update checking (port of the Python app's `updater.py` check
//! path): version comparison, the latest-release fetch, and the
//! update manifest, with the reference's host-bound credential rule.
//!
//! The environment's GitHub token is the user's own credential; the
//! release manifest names download URLs and travels unsigned, so the
//! token only ever goes to GitHub hosts.

use std::io::Read;
use std::sync::mpsc::{Receiver, Sender};
use std::thread;

use sha2::Digest;

/// Parse `v1.2` / `1.2.3` into (major, minor, patch) (port of
/// `parse_version` over the reference's anchored regex).
#[must_use]
pub fn parse_version(value: &str) -> Option<(u64, u64, u64)> {
    let text = value.trim().strip_prefix('v').unwrap_or(value.trim());
    let mut parts = text.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next()?.parse::<u64>().ok()?;
    let patch = match parts.next() {
        Some(part) => part.parse::<u64>().ok()?,
        None => 0,
    };
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// Normalize a tag to `major.minor.patch` (port of
/// `normalize_version_tag`).
#[must_use]
pub fn normalize_version_tag(tag: &str) -> Option<String> {
    let (major, minor, patch) = parse_version(tag)?;
    Some(format!("{major}.{minor}.{patch}"))
}

/// True when `latest` is strictly newer than `current` (port of
/// `is_newer_version`; unparseable versions are never newer).
#[must_use]
pub fn is_newer_version(current: &str, latest: &str) -> bool {
    match (parse_version(current), parse_version(latest)) {
        (Some(current), Some(latest)) => latest > current,
        _ => false,
    }
}

/// True for the hosts a GitHub release is served from (port of
/// `_is_github_host`).
#[must_use]
pub fn is_github_host(url: &str) -> bool {
    let host = url
        .split("://")
        .nth(1)
        .unwrap_or("")
        .split(['/', ':'])
        .next()
        .unwrap_or("")
        .to_lowercase();
    host == "github.com"
        || host.ends_with(".github.com")
        || host.ends_with(".githubusercontent.com")
}

/// The User-Agent the updater sends (from `_build_request`).
pub const USER_AGENT: &str = "AccessibleIPTVClient-Updater";

/// The update manifest for a release (port of `UpdateManifest`).
#[derive(Clone, Debug, PartialEq)]
pub struct UpdateManifest {
    /// The release version text.
    pub version: String,
    /// The portable asset's filename.
    pub asset_filename: String,
    /// The portable asset's download URL.
    pub download_url: String,
    /// The portable asset's expected SHA-256 hex.
    pub sha256: String,
    /// The release publication date text.
    pub published_date: String,
    /// An optional release-notes summary.
    pub release_notes_summary: Option<String>,
    /// An optional installer's filename.
    pub installer_asset_filename: Option<String>,
    /// An optional installer's download URL.
    pub installer_download_url: Option<String>,
    /// An optional installer's expected SHA-256 hex.
    pub installer_sha256: Option<String>,
}

/// The manifest asset's download URL from a release payload (port of
/// the asset search in `fetch_update_manifest`).
///
/// # Errors
/// Returns the reference's strings when the asset or its URL is
/// missing.
pub fn manifest_asset_url(
    release: &serde_json::Value,
    manifest_name: &str,
) -> Result<String, String> {
    let assets = release
        .get("assets")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let asset = assets.iter().find(|asset| {
        asset
            .get("name")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|name| name.eq_ignore_ascii_case(manifest_name))
    });
    let Some(asset) = asset else {
        return Err("Update manifest was not found in the latest release.".to_string());
    };
    let url = asset
        .get("browser_download_url")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if url.is_empty() {
        return Err("Update manifest download URL is missing.".to_string());
    }
    Ok(url.to_string())
}

/// Build a manifest from its JSON payload (port of the field
/// extraction in `fetch_update_manifest`).
///
/// # Errors
/// Returns the reference's "missing required fields" string when a
/// required field is absent.
pub fn manifest_from_json(data: &serde_json::Value) -> Result<UpdateManifest, String> {
    let required = |key: &str| -> Result<String, String> {
        data.get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| "Update manifest is missing required fields.".to_string())
    };
    let installer = data.get("installer");
    let optional = |key: &str| -> Option<String> {
        installer
            .and_then(|installer| installer.get(key))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .filter(|value| !value.is_empty())
    };
    // The installer filename accepts three key spellings, in the
    // reference's order.
    let installer_filename = ["asset", "asset_name", "asset_filename"]
        .iter()
        .find_map(|key| optional(key));
    Ok(UpdateManifest {
        version: required("version")?,
        asset_filename: required("asset_filename")?,
        download_url: required("download_url")?,
        sha256: required("sha256")?,
        published_date: data
            .get("published_date")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
        release_notes_summary: data
            .get("release_notes_summary")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        installer_asset_filename: installer_filename,
        installer_download_url: optional("download_url"),
        installer_sha256: optional("sha256"),
    })
}

/// The update-check outcome.
#[derive(Clone, Debug)]
pub enum UpdateEvent {
    /// The latest release's manifest and whether it is newer than
    /// the running version.
    Checked {
        /// The manifest from the latest release.
        manifest: UpdateManifest,
        /// Whether the manifest version beats the running version.
        newer: bool,
    },
    /// The update package downloaded and verified; the path is
    /// ready to open.
    Downloaded {
        /// The downloaded package's path.
        path: std::path::PathBuf,
    },
    /// The check or download failed with a user-facing message.
    Failed(String),
}

/// One unit of update work.
enum UpdateJob {
    Check {
        owner: String,
        repo: String,
        manifest_name: String,
        current: String,
    },
    Download {
        manifest: UpdateManifest,
        dest_dir: std::path::PathBuf,
    },
}

/// Runs update checks off the UI thread.
pub struct UpdateWorker {
    sender: Sender<UpdateJob>,
    receiver: Receiver<UpdateEvent>,
}

impl Default for UpdateWorker {
    fn default() -> Self {
        Self::new()
    }
}

impl UpdateWorker {
    /// Create the worker.
    #[must_use]
    pub fn new() -> Self {
        let (job_sender, job_receiver) = std::sync::mpsc::channel::<UpdateJob>();
        let (event_sender, event_receiver) = std::sync::mpsc::channel::<UpdateEvent>();
        thread::spawn(move || {
            while let Ok(job) = job_receiver.recv() {
                let event = match job {
                    UpdateJob::Check {
                        owner,
                        repo,
                        manifest_name,
                        current,
                    } => check(&owner, &repo, &manifest_name, &current),
                    UpdateJob::Download { manifest, dest_dir } => download(&manifest, &dest_dir),
                };
                if event_sender.send(event).is_err() {
                    return;
                }
            }
        });
        Self {
            sender: job_sender,
            receiver: event_receiver,
        }
    }

    /// Queue a check of `owner/repo`'s latest release.
    pub fn check(&self, owner: &str, repo: &str, manifest_name: &str, current_version: &str) {
        let _ = self.sender.send(UpdateJob::Check {
            owner: owner.to_string(),
            repo: repo.to_string(),
            manifest_name: manifest_name.to_string(),
            current: current_version.to_string(),
        });
    }

    /// Queue a download of the manifest's package (the installer
    /// asset when present, else the portable asset) into a
    /// directory; the result is verified against the manifest's
    /// SHA-256 before `Downloaded` is reported.
    pub fn download(&self, manifest: &UpdateManifest, dest_dir: std::path::PathBuf) {
        let _ = self.sender.send(UpdateJob::Download {
            manifest: manifest.clone(),
            dest_dir,
        });
    }

    /// Take the next pending check outcome, when one is ready.
    #[must_use]
    pub fn try_recv(&self) -> Option<UpdateEvent> {
        self.receiver.try_recv().ok()
    }
}

fn get_json(url: &str) -> Result<serde_json::Value, String> {
    let mut request = ureq::get(url)
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/vnd.github+json");
    if is_github_host(url) {
        if let Ok(token) = std::env::var("GITHUB_TOKEN") {
            let token = token.trim();
            if !token.is_empty() {
                request = request.header("Authorization", format!("token {token}"));
            }
        }
    }
    let mut response = request
        .call()
        .map_err(|_| "Unable to reach GitHub. Please check your connection.".to_string())?;
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|error| error.to_string())?;
    serde_json::from_str(&text).map_err(|error| error.to_string())
}

fn check(owner: &str, repo: &str, manifest_name: &str, current: &str) -> UpdateEvent {
    let url = format!("https://api.github.com/repos/{owner}/{repo}/releases/latest");
    let release = match get_json(&url) {
        Ok(release) => release,
        Err(error) => return UpdateEvent::Failed(error),
    };
    let manifest_url = match manifest_asset_url(&release, manifest_name) {
        Ok(url) => url,
        Err(error) => return UpdateEvent::Failed(error),
    };
    match get_json(&manifest_url) {
        Ok(data) => match manifest_from_json(&data) {
            Ok(manifest) => {
                let newer = is_newer_version(current, &manifest.version);
                UpdateEvent::Checked { manifest, newer }
            }
            Err(error) => UpdateEvent::Failed(error),
        },
        Err(error) => UpdateEvent::Failed(error),
    }
}

/// Download the manifest's package into `dest_dir` and verify
/// its SHA-256.
fn download(manifest: &UpdateManifest, dest_dir: &std::path::Path) -> UpdateEvent {
    let (filename, url, sha) = match (
        &manifest.installer_asset_filename,
        &manifest.installer_download_url,
    ) {
        (Some(name), Some(url)) => (
            name.clone(),
            url.clone(),
            manifest.installer_sha256.clone().unwrap_or_default(),
        ),
        _ => (
            manifest.asset_filename.clone(),
            manifest.download_url.clone(),
            manifest.sha256.clone(),
        ),
    };
    if let Err(error) = std::fs::create_dir_all(dest_dir) {
        return UpdateEvent::Failed(format!("Could not create the updates folder: {error}"));
    }
    let dest = dest_dir.join(filename);
    match download_file_sha256(&url, &dest) {
        Ok(actual) if sha.is_empty() || actual.eq_ignore_ascii_case(&sha) => {
            UpdateEvent::Downloaded { path: dest }
        }
        Ok(_) => UpdateEvent::Failed(
            "The downloaded update failed its integrity check and was not used.".to_string(),
        ),
        Err(error) => UpdateEvent::Failed(format!("Update download failed: {error}")),
    }
}

/// Download `url` to `dest`, returning the SHA-256 hex digest of the
/// bytes written (port of `download_file_with_sha256`'s core).
///
/// # Errors
/// Returns a message when the request or the write fails.
pub fn download_file_sha256(url: &str, dest: &std::path::Path) -> Result<String, String> {
    let mut response = ureq::get(url)
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|error| error.to_string())?;
    let mut file = std::fs::File::create(dest).map_err(|error| error.to_string())?;
    let mut reader = response.body_mut().as_reader();
    let mut hasher = sha2::Sha256::new();
    let mut buffer = vec![0_u8; 65536];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        std::io::Write::write_all(&mut file, &buffer[..read]).map_err(|error| error.to_string())?;
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_like_python() {
        assert_eq!(parse_version("v1.150.58"), Some((1, 150, 58)));
        assert_eq!(parse_version("1.2"), Some((1, 2, 0)));
        assert_eq!(parse_version("1.2.3.4"), None);
        assert_eq!(parse_version("x"), None);
        assert_eq!(normalize_version_tag("v2.5"), Some("2.5.0".to_string()));
        assert!(is_newer_version("0.1.0", "0.2.0"));
        assert!(!is_newer_version("0.2.0", "0.2.0"));
        assert!(!is_newer_version("bad", "1.0.0"));
    }

    #[test]
    fn manifest_extraction_and_errors() {
        let release = serde_json::json!({
            "assets": [
                {"name": "app.zip", "browser_download_url": "https://x/app.zip"},
                {"name": "Update-Manifest.json", "browser_download_url": "https://x/manifest.json"}
            ]
        });
        assert_eq!(
            manifest_asset_url(&release, "update-manifest.json"),
            Ok("https://x/manifest.json".to_string())
        );
        assert!(manifest_asset_url(&release, "other.json")
            .unwrap_err()
            .contains("was not found"));
        let data = serde_json::json!({
            "version": "1.2.3",
            "asset_filename": "app.zip",
            "download_url": "https://x/app.zip",
            "sha256": "abc",
            "installer": {"asset": "setup.exe", "download_url": "https://x/setup.exe", "sha256": "def"}
        });
        let manifest = manifest_from_json(&data).expect("manifest");
        assert_eq!(manifest.version, "1.2.3");
        assert_eq!(
            manifest.installer_asset_filename.as_deref(),
            Some("setup.exe")
        );
        assert!(manifest_from_json(&serde_json::json!({"version": "1"})).is_err());
        assert!(is_github_host("https://api.github.com/x"));
        assert!(is_github_host("https://objects.githubusercontent.com/x"));
        assert!(!is_github_host("https://example.com/x"));
    }
}
