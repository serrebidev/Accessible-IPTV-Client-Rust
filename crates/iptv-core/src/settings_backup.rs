//! Password-encrypted settings backup, byte-compatible with the
//! Python client's `settings_backup.py` (issue #99).
//!
//! Envelope (one JSON object, Python `json.dump` separators):
//! `{"format": "accessible-iptv-settings-v1", "salt": "...", "data": "..."}`
//! where `salt` is 16 random bytes in standard base64 and `data` is a
//! Fernet token (AES-128-CBC + HMAC-SHA256, urlsafe base64) over the
//! config JSON. The Fernet key is PBKDF2-HMAC-SHA256(password UTF-8,
//! `salt`, `600_000` iterations, 32 bytes): the first 16 bytes sign, the
//! last 16 encrypt — exactly Python's `_cipher`. Error strings are
//! Python's, verbatim.

use aes::Aes128;
use base64::engine::general_purpose::{STANDARD, URL_SAFE};
use base64::Engine;
use cbc::{Decryptor, Encryptor};
use hmac::{digest::KeyInit, Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;
type Aes128CbcEnc = Encryptor<Aes128>;
type Aes128CbcDec = Decryptor<Aes128>;

const FORMAT: &str = "accessible-iptv-settings-v1";
const ITERATIONS: u32 = 600_000;
const MAX_BACKUP_BYTES: usize = 16 * 1024 * 1024;
const SALT_LEN: usize = 16;
/// cryptography's Fernet rejects tokens stamped further than this
/// many seconds in the future, even without a TTL.
const MAX_CLOCK_SKEW_SECS: u64 = 60;

const ERR_PASSWORD_REQUIRED: &str = "A backup password is required.";
const ERR_WRONG_OR_DAMAGED: &str = "Wrong password or damaged backup.";
const ERR_NOT_A_BACKUP: &str = "Not an Accessible IPTV settings backup.";

/// Encrypt config JSON bytes into the backup envelope bytes.
///
/// # Errors
/// Python's messages: the plaintext-size limit is checked first
/// ("Settings are too large to back up."), then an empty password
/// ("A backup password is required.").
pub fn export_settings(config_json_bytes: &[u8], password: &str) -> Result<Vec<u8>, String> {
    if config_json_bytes.len() > MAX_BACKUP_BYTES {
        return Err("Settings are too large to back up.".to_string());
    }
    if password.is_empty() {
        return Err(ERR_PASSWORD_REQUIRED.to_string());
    }
    let mut salt = [0u8; SALT_LEN];
    getrandom::fill(&mut salt).map_err(|_| "Could not generate random salt.".to_string())?;
    let key = derive_key(password, &salt);
    let token = fernet_encrypt(&key, config_json_bytes)
        .map_err(|()| "Could not generate random IV.".to_string())?;
    Ok(format!(
        "{{\"format\": \"{FORMAT}\", \"salt\": \"{}\", \"data\": \"{token}\"}}",
        STANDARD.encode(salt)
    )
    .into_bytes())
}

/// Decrypt backup envelope bytes back to the config JSON bytes.
///
/// # Errors
/// Python's messages, in Python's check order: oversized file,
/// not a backup (bad JSON / wrong format), undecodable salt,
/// wrong-length salt, then any token/password problem as
/// "Wrong password or damaged backup." — including an empty
/// password, which Python's import path folds into that message.
/// A decrypted config without list-valued `playlists` and `epgs`
/// is "Backup settings are invalid."
pub fn import_settings(bytes: &[u8], password: &str) -> Result<Vec<u8>, String> {
    if bytes.len() > MAX_BACKUP_BYTES {
        return Err("Backup file is too large.".to_string());
    }
    let envelope: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| ERR_NOT_A_BACKUP.to_string())?;
    let object = envelope
        .as_object()
        .filter(|object| object.get("format").and_then(serde_json::Value::as_str) == Some(FORMAT));
    let Some(object) = object else {
        return Err(ERR_NOT_A_BACKUP.to_string());
    };
    let salt_text = object
        .get("salt")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| ERR_WRONG_OR_DAMAGED.to_string())?;
    let salt = STANDARD
        .decode(salt_text)
        .map_err(|_| ERR_WRONG_OR_DAMAGED.to_string())?;
    if salt.len() != SALT_LEN {
        return Err("Invalid backup salt.".to_string());
    }
    let token = object
        .get("data")
        .and_then(serde_json::Value::as_str)
        .filter(|token| token.is_ascii())
        .ok_or_else(|| ERR_WRONG_OR_DAMAGED.to_string())?;
    if password.is_empty() {
        return Err(ERR_WRONG_OR_DAMAGED.to_string());
    }
    let key = derive_key(password, &salt);
    let contents = fernet_decrypt(&key, token).ok_or_else(|| ERR_WRONG_OR_DAMAGED.to_string())?;
    let config: serde_json::Value =
        serde_json::from_slice(&contents).map_err(|_| ERR_WRONG_OR_DAMAGED.to_string())?;
    let valid = config.as_object().is_some_and(|config| {
        config
            .get("playlists")
            .is_some_and(serde_json::Value::is_array)
            && config.get("epgs").is_some_and(serde_json::Value::is_array)
    });
    if !valid {
        return Err("Backup settings are invalid.".to_string());
    }
    Ok(contents)
}

/// Create a recovery copy of the current settings and prove it
/// before handing it out (issue #136): the exported bytes are
/// decrypted again with the same password and must parse back
/// to the identical settings object. A copy that cannot be
/// verified is never returned.
///
/// # Errors
/// The export errors, or the verification errors below.
pub fn create_verified_recovery(
    config_json_bytes: &[u8],
    password: &str,
) -> Result<Vec<u8>, String> {
    let bytes = export_settings(config_json_bytes, password)?;
    verify_recovery(&bytes, password, config_json_bytes)?;
    Ok(bytes)
}

/// Verify recovery bytes against the settings they were made
/// from: decrypt with the password and compare the parsed
/// settings objects exactly.
///
/// # Errors
/// The import errors (wrong password, damaged or foreign
/// backup), an unreadable current-settings value, or a
/// mismatch between the copy and the expected settings.
pub fn verify_recovery(
    bytes: &[u8],
    password: &str,
    expected_config_json: &[u8],
) -> Result<(), String> {
    let decrypted = import_settings(bytes, password)?;
    let expected: serde_json::Value = serde_json::from_slice(expected_config_json)
        .map_err(|_| "Current settings could not be read for the recovery copy.".to_string())?;
    let restored: serde_json::Value = serde_json::from_slice(&decrypted)
        .map_err(|_| "The recovery copy could not be verified.".to_string())?;
    if expected != restored {
        return Err("The recovery copy could not be verified.".to_string());
    }
    Ok(())
}

fn derive_key(password: &str, salt: &[u8]) -> [u8; 32] {
    let mut key = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha256>(password.as_bytes(), salt, ITERATIONS, &mut key);
    key
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn fernet_mac(signing_key: &[u8], signed: &[u8]) -> HmacSha256 {
    let mut mac = HmacSha256::new_from_slice(signing_key).expect("HMAC accepts a 16-byte key");
    mac.update(signed);
    mac
}

/// Fernet encrypt: `0x80 || timestamp BE || IV || AES-128-CBC(PKCS7)
/// || HMAC-SHA256`, urlsafe-base64 with padding.
fn fernet_encrypt(key: &[u8; 32], plaintext: &[u8]) -> Result<String, ()> {
    let mut iv = [0u8; 16];
    getrandom::fill(&mut iv).map_err(|_| ())?;
    let ciphertext = {
        use cbc::cipher::{BlockModeEncrypt, KeyIvInit};
        Aes128CbcEnc::new_from_slices(&key[16..], &iv)
            .expect("16-byte key and IV")
            .encrypt_padded_vec::<cbc::cipher::block_padding::Pkcs7>(plaintext)
    };
    let mut blob = Vec::with_capacity(1 + 8 + 16 + ciphertext.len() + 32);
    blob.push(0x80);
    blob.extend_from_slice(&now_secs().to_be_bytes());
    blob.extend_from_slice(&iv);
    blob.extend_from_slice(&ciphertext);
    let tag = fernet_mac(&key[..16], &blob).finalize().into_bytes();
    blob.extend_from_slice(&tag);
    Ok(URL_SAFE.encode(blob))
}

/// Fernet decrypt; every failure is the same `None` (Python's
/// `InvalidToken`), so callers cannot leak which check failed.
fn fernet_decrypt(key: &[u8; 32], token: &str) -> Option<Vec<u8>> {
    use cbc::cipher::{BlockModeDecrypt, KeyIvInit};
    let blob = URL_SAFE.decode(token).ok()?;
    // version(1) + timestamp(8) + IV(16) + >=1 block(16) + HMAC(32)
    if blob.len() < 73 || blob[0] != 0x80 {
        return None;
    }
    let (signed, tag) = blob.split_at(blob.len() - 32);
    fernet_mac(&key[..16], signed).verify_slice(tag).ok()?;
    let timestamp = u64::from_be_bytes(signed[1..9].try_into().ok()?);
    if timestamp > now_secs() + MAX_CLOCK_SKEW_SECS {
        return None;
    }
    let iv: [u8; 16] = signed[9..25].try_into().ok()?;
    Aes128CbcDec::new_from_slices(&key[16..], &iv)
        .ok()?
        .decrypt_padded_vec::<cbc::cipher::block_padding::Pkcs7>(&signed[25..])
        .ok()
}

// ---------------------------------------------------------------------
// Full backups (Python `full_backup.py`, format v2): settings plus the
// recording schedule, reminders and saved-search history, restored
// through a journal before any background service starts.

/// Python `full_backup.FORMAT`.
pub const FORMAT_V2: &str = "accessible-iptv-settings-v2";
const BAD_BUNDLE: &str = "Backup settings are invalid.";

fn valid_time(value: Option<&serde_json::Value>) -> bool {
    value
        .and_then(serde_json::Value::as_f64)
        .is_some_and(|v| v.is_finite() && (0.0..=253_402_300_799.0).contains(&v))
}

fn valid_job_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Python `full_backup.validate`: check a bundle and make it safe to
/// restore. Running recordings and scheduled ones already due come
/// back as missed (a time repeat marked to queue its next future
/// occurrence); recording ids clear; reminders normalize and past
/// ones expire; the search history keeps only upcoming entries.
///
/// # Errors
/// Returns Python's "Backup settings are invalid." for any shape or
/// value it rejects.
#[allow(clippy::too_many_lines)] // one validation, in Python's order
pub fn validate_bundle(bundle: &serde_json::Value, now: f64) -> Result<serde_json::Value, String> {
    use serde_json::Value;
    let bad = || BAD_BUNDLE.to_string();
    let object = bundle.as_object().ok_or_else(bad)?;
    let keys: std::collections::BTreeSet<&str> = object.keys().map(String::as_str).collect();
    let full: std::collections::BTreeSet<&str> = ["config", "jobs", "reminders", "search_seen"]
        .into_iter()
        .collect();
    if !keys.is_subset(&full) {
        return Err(bad());
    }
    let config = object
        .get("config")
        .and_then(Value::as_object)
        .ok_or_else(bad)?;
    if !config.get("playlists").is_some_and(Value::is_array)
        || !config.get("epgs").is_some_and(Value::is_array)
    {
        return Err(bad());
    }
    let only_config: std::collections::BTreeSet<&str> = std::iter::once("config").collect();
    if keys != only_config && keys != full {
        return Err(bad());
    }
    let mut data = bundle.clone();
    if keys == only_config {
        return Ok(data);
    }
    let jobs = data["jobs"]
        .as_array_mut()
        .filter(|j| j.len() <= 10_000)
        .ok_or_else(bad)?;
    let mut ids = std::collections::BTreeSet::new();
    for job in jobs.iter_mut() {
        let map = job.as_object().ok_or_else(bad)?;
        let id = map.get("id").and_then(Value::as_str).ok_or_else(bad)?;
        let status = map.get("status").and_then(Value::as_str).ok_or_else(bad)?;
        let known = matches!(
            status,
            crate::dvr::STATUS_SCHEDULED
                | crate::dvr::STATUS_RECORDING
                | crate::dvr::STATUS_STOPPING
        ) || crate::dvr::DONE_STATUSES.contains(&status);
        let text_or_none = |key: &str| map.get(key).is_none_or(|v| v.is_null() || v.is_string());
        if !valid_job_id(id)
            || ids.contains(id)
            || !known
            || !valid_time(map.get("start_ts"))
            || !valid_time(map.get("stop_ts"))
            || map["stop_ts"].as_f64() <= map["start_ts"].as_f64()
            || !map.get("channel").is_some_and(Value::is_object)
            || !map.get("program").is_some_and(Value::is_object)
            || !text_or_none("title")
            || !text_or_none("repeat")
            || !map.get("format").is_some_and(Value::is_string)
        {
            return Err(bad());
        }
        let checked: DvrJobCheck = serde_json::from_value(job.clone()).map_err(|_| bad())?;
        if !checked.valid() {
            return Err(bad());
        }
        ids.insert(id.to_string());
        let start = map["start_ts"].as_f64().unwrap_or(0.0);
        let repeat = map
            .get("repeat")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let live = matches!(
            status,
            crate::dvr::STATUS_RECORDING | crate::dvr::STATUS_STOPPING
        ) || (status == crate::dvr::STATUS_SCHEDULED && start <= now);
        if live {
            if crate::dvr::TIME_REPEATS.contains(&repeat.as_str()) {
                job["restore_next_occurrence"] = Value::Bool(true);
            }
            job["status"] = crate::dvr::STATUS_MISSED.into();
            job["message"] = "".into();
        }
        job["recording_id"] = Value::Null;
    }
    let rows = data["reminders"].as_array().ok_or_else(bad)?;
    if rows.len() > crate::reminders::MAX_REMINDERS {
        return Err(bad());
    }
    let parsed: Vec<crate::reminders::Reminder> = rows
        .iter()
        .map(|row| serde_json::from_value(row.clone()))
        .collect::<Result<_, _>>()
        .map_err(|_| bad())?;
    let count = parsed.len();
    let mut normalized = crate::reminders::normalize_reminders(parsed);
    if normalized.len() != count {
        return Err(bad());
    }
    #[allow(clippy::cast_possible_truncation)] // whole seconds
    let now_secs = now as i64;
    for row in &mut normalized {
        if row.end <= now_secs
            && matches!(
                row.status,
                crate::reminders::ReminderStatus::Scheduled
                    | crate::reminders::ReminderStatus::Ready
            )
        {
            row.status = crate::reminders::ReminderStatus::Expired;
        }
    }
    data["reminders"] = serde_json::to_value(normalized).map_err(|_| bad())?;
    data["search_seen"] = crate::saved_searches::normalize_seen(&data["search_seen"], now)?;
    Ok(data)
}

/// The job checks Python makes through `dvr.valid_padding_pair`,
/// `valid_series_padding` and `checked_weekdays`.
#[derive(serde::Deserialize)]
struct DvrJobCheck {
    #[serde(default)]
    repeat: Option<String>,
    #[serde(default)]
    weekdays: Option<serde_json::Value>,
    #[serde(default)]
    future_padding: Option<serde_json::Value>,
    #[serde(default)]
    series_padding_changes: Option<serde_json::Value>,
    // PR #48: backup validation uses the same stored-job
    // check as DVR loading, which includes the pause period.
    #[serde(default)]
    pause_dates: Option<serde_json::Value>,
    #[serde(default)]
    start_ts: f64,
}

impl DvrJobCheck {
    fn valid(&self) -> bool {
        if let Some(pause) = &self.pause_dates {
            let period = pause.as_array().and_then(|row| {
                if row.len() == 2 {
                    Some([
                        row[0].as_str()?.to_string(),
                        row[1].as_str()?.to_string(),
                    ])
                } else {
                    None
                }
            });
            if period.as_ref().is_none_or(|p| !crate::dvr::valid_pause_period(p))
                || !(0.0..=253_402_300_799.0).contains(&self.start_ts)
            {
                return false;
            }
        }
        if self
            .future_padding
            .as_ref()
            .is_some_and(|v| !crate::dvr::valid_padding_pair(v))
            || self
                .series_padding_changes
                .as_ref()
                .is_some_and(|v| !crate::dvr::valid_series_padding(v))
        {
            return false;
        }
        if self.repeat.as_deref() == Some("weekdays") {
            return self.weekdays.as_ref().is_some_and(|days| {
                crate::dvr::checked_weekdays(days).is_ok_and(|d| !d.is_empty())
            });
        }
        true
    }
}

/// Python `full_backup.export_bundle`: validate (without changing the
/// snapshot) and encrypt the bundle into the v2 envelope.
///
/// # Errors
/// Validation errors, an empty password, randomness failures, and
/// Python's size message.
pub fn export_bundle(
    bundle: &serde_json::Value,
    password: &str,
    now: f64,
) -> Result<Vec<u8>, String> {
    validate_bundle(bundle, now)?;
    if password.is_empty() {
        return Err(ERR_PASSWORD_REQUIRED.to_string());
    }
    let payload = serde_json::to_vec(bundle).map_err(|e| e.to_string())?;
    let mut salt = [0u8; SALT_LEN];
    getrandom::fill(&mut salt).map_err(|_| "Could not generate random salt.".to_string())?;
    let key = derive_key(password, &salt);
    let token =
        fernet_encrypt(&key, &payload).map_err(|()| "Could not generate random IV.".to_string())?;
    let envelope = format!(
        "{{\"format\": \"{FORMAT_V2}\", \"salt\": \"{}\", \"data\": \"{token}\"}}",
        STANDARD.encode(salt)
    );
    if envelope.len() > MAX_BACKUP_BYTES {
        return Err("Settings are too large to back up.".to_string());
    }
    Ok(envelope.into_bytes())
}

/// Python `full_backup.import_bundle`: a v2 backup decrypted and
/// validated, or a v1 backup as settings only.
///
/// # Errors
/// Python's messages for a foreign file, a wrong password or damaged
/// backup, and an invalid bundle.
pub fn import_bundle(bytes: &[u8], password: &str, now: f64) -> Result<serde_json::Value, String> {
    if bytes.len() > MAX_BACKUP_BYTES {
        return Err("Backup file is too large.".to_string());
    }
    let envelope: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| ERR_NOT_A_BACKUP.to_string())?;
    match envelope.get("format").and_then(serde_json::Value::as_str) {
        Some(FORMAT) => {
            let config: serde_json::Value =
                serde_json::from_slice(&import_settings(bytes, password)?)
                    .map_err(|_| ERR_WRONG_OR_DAMAGED.to_string())?;
            return validate_bundle(&serde_json::json!({ "config": config }), now);
        }
        Some(FORMAT_V2) => {}
        _ => return Err(ERR_NOT_A_BACKUP.to_string()),
    }
    let damaged = || ERR_WRONG_OR_DAMAGED.to_string();
    let salt = envelope
        .get("salt")
        .and_then(serde_json::Value::as_str)
        .and_then(|text| STANDARD.decode(text).ok())
        .filter(|salt| salt.len() == SALT_LEN)
        .ok_or_else(damaged)?;
    let token = envelope
        .get("data")
        .and_then(serde_json::Value::as_str)
        .filter(|token| token.is_ascii())
        .ok_or_else(damaged)?;
    let key = derive_key(password, &salt);
    let contents = fernet_decrypt(&key, token).ok_or_else(damaged)?;
    let bundle: serde_json::Value = serde_json::from_slice(&contents).map_err(|_| damaged())?;
    validate_bundle(&bundle, now)
}

/// Where a restore writes (Python `full_backup.paths`).
#[derive(Clone, Debug)]
pub struct RestoreTargets {
    pub config: std::path::PathBuf,
    pub jobs: std::path::PathBuf,
    pub reminders: std::path::PathBuf,
    pub search_seen: std::path::PathBuf,
}

impl RestoreTargets {
    /// The config write target and the files beside the schedule.
    #[must_use]
    pub fn new(config: std::path::PathBuf, jobs: std::path::PathBuf) -> Self {
        Self {
            reminders: crate::reminders::reminders_path(&jobs),
            search_seen: jobs.with_file_name("programme-search-state.json"),
            config,
            jobs,
        }
    }

    /// Python `pending_path`: the restore journal.
    #[must_use]
    pub fn pending(&self) -> std::path::PathBuf {
        self.jobs.with_file_name("pending-restore.json")
    }
}

/// Python `atomic_json`: a synced temp file renamed over `path`.
fn atomic_json(path: &std::path::Path, value: &serde_json::Value) -> Result<(), String> {
    let text = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    let directory = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let temporary = directory.join(format!("restore-{}-{nanos}.tmp", std::process::id()));
    let written = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        std::io::Write::write_all(&mut file, &text)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)
    })();
    if let Err(error) = written {
        let _ = std::fs::remove_file(&temporary);
        return Err(error.to_string());
    }
    Ok(())
}

/// Python `stage_restore`: write the checked bundle as the journal the
/// next start finishes.
///
/// # Errors
/// Validation errors, Python's size message, and write errors.
pub fn stage_restore(
    bundle: &serde_json::Value,
    targets: &RestoreTargets,
    now: f64,
) -> Result<(), String> {
    let checked = validate_bundle(bundle, now)?;
    if serde_json::to_vec(&checked).map_or(usize::MAX, |b| b.len()) > MAX_BACKUP_BYTES {
        return Err("Backup file is too large.".to_string());
    }
    atomic_json(&targets.pending(), &checked)
}

/// Python `apply_pending`: finish a staged restore before anything
/// else reads these files. Returns whether one was applied; the
/// journal is removed only after every file is written, so a crash
/// finishes the same restore on the next start.
///
/// # Errors
/// Returns read, validation and write errors; the journal then stays.
pub fn apply_pending(targets: &RestoreTargets, now: f64) -> Result<bool, String> {
    let pending = targets.pending();
    let bytes = match std::fs::read(&pending) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.to_string()),
    };
    if bytes.len() > MAX_BACKUP_BYTES {
        return Err("Backup file is too large.".to_string());
    }
    let staged: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let bundle = validate_bundle(&staged, now)?;
    let object = bundle.as_object().ok_or_else(|| BAD_BUNDLE.to_string())?;
    for (key, value) in object {
        match key.as_str() {
            "config" => atomic_json(&targets.config, value)?,
            "jobs" => atomic_json(
                &targets.jobs,
                &serde_json::json!({"version": 1, "jobs": restored_jobs(value, now)?}),
            )?,
            "reminders" => atomic_json(
                &targets.reminders,
                &serde_json::json!({"version": 1, "reminders": value}),
            )?,
            "search_seen" => atomic_json(&targets.search_seen, value)?,
            _ => {}
        }
    }
    std::fs::remove_file(&pending).map_err(|e| e.to_string())?;
    Ok(true)
}

/// Python's job step in `apply_pending`: a repeat that was live in
/// the backup queues its next future occurrence, unless one for the
/// same channel, title, days and programme window already exists.
fn restored_jobs(value: &serde_json::Value, now: f64) -> Result<Vec<serde_json::Value>, String> {
    let jobs: Vec<serde_json::Value> = value.as_array().cloned().unwrap_or_default();
    let slot = |job: &crate::dvr::DvrJob| {
        #[allow(clippy::cast_precision_loss)] // padding minutes
        let start = job.start_ts + job.pre_padding_minutes as f64 * 60.0;
        #[allow(clippy::cast_precision_loss)]
        let end = job.stop_ts - job.post_padding_minutes as f64 * 60.0;
        let days = if job.repeat == "weekdays" {
            let mut days = job.weekdays.clone().unwrap_or_default();
            days.sort_unstable();
            days.dedup();
            days
        } else {
            Vec::new()
        };
        format!(
            "{}|{}|{}|{days:?}|{start}|{end}",
            crate::reminders::reminder_channel_key(&crate::dvr::channel_from_snapshot(
                &job.channel
            )),
            job.title,
            job.repeat
        )
    };
    let typed: Vec<crate::dvr::DvrJob> = jobs
        .iter()
        .map(|job| serde_json::from_value(job.clone()).map_err(|_| BAD_BUNDLE.to_string()))
        .collect::<Result<_, _>>()?;
    let mut existing: std::collections::BTreeSet<String> = typed
        .iter()
        .filter(|job| crate::dvr::TIME_REPEATS.contains(&job.repeat.as_str()))
        .map(slot)
        .collect();
    let mut out = Vec::with_capacity(jobs.len());
    let mut ids: std::collections::BTreeSet<String> =
        typed.iter().map(|job| job.id.clone()).collect();
    let mut successors = Vec::new();
    for mut job in typed {
        let flagged =
            job.extra.remove("restore_next_occurrence") == Some(serde_json::Value::Bool(true));
        if flagged
            && job.status == crate::dvr::STATUS_MISSED
            && crate::dvr::TIME_REPEATS.contains(&job.repeat.as_str())
        {
            let next = crate::dvr::next_time_occurrence_after(&job, now, &chrono::Local, true)?;
            let key = slot(&next);
            if !existing.contains(&key) && ids.insert(next.id.clone()) {
                existing.insert(key);
                successors.push(next);
            }
        }
        out.push(serde_json::to_value(&job).map_err(|e| e.to_string())?);
    }
    for next in successors {
        out.push(serde_json::to_value(&next).map_err(|e| e.to_string())?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_bundle(now: f64) -> serde_json::Value {
        let channel = crate::Channel {
            name: "News".to_string(),
            url: "http://example.com/s".to_string(),
            ..crate::Channel::default()
        };
        let spec = |start: i64, repeat: &'static str| crate::dvr::NewJob {
            channel: &channel,
            programme_title: "Show",
            programme_start: start,
            programme_stop: start + 3600,
            format: "provider_mkv",
            pre_padding_minutes: 0,
            post_padding_minutes: 0,
            repeat,
            episode: "",
            subtitle: "",
            description: "",
        };
        #[allow(clippy::cast_possible_truncation)]
        let at = now as i64;
        let mut live = crate::dvr::build_job(&spec(at - 600, "daily"), now).unwrap();
        live.id = "live".to_string();
        live.status = crate::dvr::STATUS_RECORDING.to_string();
        live.recording_id = Some("7".to_string());
        let mut later = crate::dvr::build_job(&spec(at + 7200, ""), now).unwrap();
        later.id = "later".to_string();
        let mut reminder_channel = channel.clone();
        reminder_channel
            .unknown_attributes
            .insert("playlist-id".to_string(), "m3u:abc".to_string());
        let programme = crate::epg::Programme {
            start: at + 3600,
            stop: at + 7200,
            title: "Film".to_string(),
            ..crate::epg::Programme::default()
        };
        let reminder =
            crate::reminders::build_reminder(&reminder_channel, &programme, 5, at).unwrap();
        serde_json::json!({
            "config": {"playlists": [], "epgs": [], "volume": 80},
            "jobs": [live, later],
            "reminders": [reminder],
            "search_seen": {"a".repeat(64): now + 600.0},
        })
    }

    #[test]
    fn full_backup_round_trip_restores_running_work_as_missed() {
        let now = 1_795_000_000.0;
        let bundle = sample_bundle(now);
        let bytes = export_bundle(&bundle, "pw", now).unwrap();
        let envelope: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(envelope["format"], FORMAT_V2);
        assert_eq!(
            import_bundle(&bytes, "wrong", now),
            Err(ERR_WRONG_OR_DAMAGED.to_string())
        );
        let restored = import_bundle(&bytes, "pw", now).unwrap();
        assert_eq!(restored["config"], bundle["config"]);
        let live = &restored["jobs"][0];
        assert_eq!(live["status"], crate::dvr::STATUS_MISSED);
        assert_eq!(live["restore_next_occurrence"], true);
        assert!(live["recording_id"].is_null());
        assert_eq!(restored["jobs"][1]["status"], crate::dvr::STATUS_SCHEDULED);
        assert_eq!(restored["reminders"], bundle["reminders"]);
        assert_eq!(restored["search_seen"], bundle["search_seen"]);
    }

    #[test]
    fn invalid_bundles_are_refused() {
        let now = 1_795_000_000.0;
        let bad = Err(BAD_BUNDLE.to_string());
        let mut extra = sample_bundle(now);
        extra["other"] = serde_json::json!(1);
        assert_eq!(validate_bundle(&extra, now), bad);
        let mut partial = sample_bundle(now);
        partial.as_object_mut().unwrap().remove("reminders");
        assert_eq!(validate_bundle(&partial, now), bad);
        let mut bad_id = sample_bundle(now);
        bad_id["jobs"][1]["id"] = serde_json::json!("has space");
        assert_eq!(validate_bundle(&bad_id, now), bad);
        let mut bad_days = sample_bundle(now);
        bad_days["jobs"][1]["repeat"] = serde_json::json!("weekdays");
        assert_eq!(validate_bundle(&bad_days, now), bad);
        // PR #48: an invalid pause period refuses the bundle,
        // like the DVR loader's stored-job check; a valid one
        // passes.
        let mut bad_pause = sample_bundle(now);
        bad_pause["jobs"][1]["pause_dates"] = serde_json::json!(["2027-02-01", "2027-01-01"]);
        assert_eq!(validate_bundle(&bad_pause, now), bad);
        let mut good_pause = sample_bundle(now);
        good_pause["jobs"][1]["pause_dates"] = serde_json::json!(["2027-01-01", "2027-02-01"]);
        assert!(validate_bundle(&good_pause, now).is_ok());
        let only = serde_json::json!({"config": {"playlists": [], "epgs": []}});
        assert_eq!(validate_bundle(&only, now), Ok(only.clone()));
        // A settings-only (v1) backup imports as settings only.
        let v1 = export_settings(br#"{"playlists": [], "epgs": []}"#, "pw").unwrap();
        assert_eq!(import_bundle(&v1, "pw", now).unwrap(), only);
    }

    #[test]
    fn staged_restore_finishes_on_the_next_start() {
        let now = 1_795_000_000.0;
        let dir = std::env::temp_dir().join(format!(
            "iptv-restore-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let targets = RestoreTargets::new(
            dir.join("iptvclient.conf"),
            dir.join("scheduled_recordings.json"),
        );
        stage_restore(&sample_bundle(now), &targets, now).unwrap();
        assert!(targets.pending().exists());
        assert_eq!(apply_pending(&targets, now), Ok(true));
        assert!(!targets.pending().exists());
        let config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&targets.config).unwrap()).unwrap();
        assert_eq!(config["volume"], 80);
        let schedule = crate::dvr::DvrSchedule::load(targets.jobs.clone(), now);
        assert!(
            schedule.load_warning.is_empty(),
            "{}",
            schedule.load_warning
        );
        let live = schedule.get("live").unwrap();
        assert_eq!(live.status, crate::dvr::STATUS_MISSED);
        assert!(!live.extra.contains_key("restore_next_occurrence"));
        // The daily repeat that was recording queues its next future occurrence.
        assert_eq!(schedule.jobs().len(), 3);
        assert!(schedule
            .jobs()
            .iter()
            .any(|j| j.status == crate::dvr::STATUS_SCHEDULED
                && j.start_ts > now
                && j.repeat == "daily"));
        let reminders = crate::reminders::load_reminder_file(&targets.reminders);
        assert_eq!(reminders.rows.len(), 1);
        assert!(targets.search_seen.exists());
        assert_eq!(apply_pending(&targets, now), Ok(false));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Cross-check with the Python client: set `IPTV_BACKUP_ORACLE` to a
    /// folder holding `python.aiptv` (exported by Python with password
    /// `pw`); this test imports it and writes `rust.aiptv` for Python to
    /// import (`tools/backup-oracle.py`). Skipped when unset.
    #[test]
    fn python_full_backup_oracle() {
        let Some(dir) = std::env::var_os("IPTV_BACKUP_ORACLE").map(std::path::PathBuf::from) else {
            return;
        };
        let now = 1_795_000_000.0;
        let python = std::fs::read(dir.join("python.aiptv")).unwrap();
        let bundle = import_bundle(&python, "pw", now).unwrap();
        assert!(bundle["jobs"].as_array().is_some_and(|j| !j.is_empty()));
        std::fs::write(
            dir.join("rust.aiptv"),
            export_bundle(&sample_bundle(now), "pw", now).unwrap(),
        )
        .unwrap();
    }

    /// A real backup written by the pinned Python client's
    /// `export_settings` (cryptography 50.0.1) for the config in
    /// `PYTHON_CONFIG_JSON`, password "correct horse battery staple".
    const PYTHON_BACKUP: &str = "{\"format\": \"accessible-iptv-settings-v1\", \"salt\": \"G884XRt9aPfsay30PATf9A==\", \"data\": \"gAAAAABq6E1rcvpxoY29X2ZhfoGO4pvj26J4T5F0pM9uDCv1SWrFwXFYIwZckTyk2vMo4GI_oc9cF2dnzHoYitE6iD-7Xyv_5MpQj3D_vb0XxrRFAeJWeKGmkhGwtS8sjMLw3H8zYqFERzRa19aYL1D7c0a2rcM2edOLjFAFdi9juq_VspP3T3bF14jpl8En2QDqXlhWpHC2pJpNCZibLsvpb_LEg4sl0nuIFBRZEQTish8v0GC6NdeEyFxBi60A8cfOMqQRVURLNhVdOb7za8yAF5KwmHK5srb2E1LH5fxWk6xFNL9a9SI=\"}";
    /// Python's `json.dumps(config, ensure_ascii=False)` for that
    /// config — the exact bytes the token must decrypt to.
    const PYTHON_CONFIG_JSON: &str = "{\"playlists\": [\"http://example.invalid/list.m3u\", {\"type\": \"xtream\", \"name\": \"Café TV\"}], \"epgs\": [\"http://example.invalid/guide.xml\"], \"language\": \"Français\", \"volume\": 80}";
    const PASSWORD: &str = "correct horse battery staple";

    #[test]
    fn decrypts_a_python_written_backup_byte_for_byte() {
        let contents = import_settings(PYTHON_BACKUP.as_bytes(), PASSWORD).unwrap();
        assert_eq!(contents, PYTHON_CONFIG_JSON.as_bytes());
    }

    /// A second Python-written fixture, generated fresh from the
    /// pinned reference on 2026-11-02 for issue #133, carrying
    /// synthetic credential-shaped values (never real ones).
    const PYTHON_BACKUP_133: &str = "{\"format\": \"accessible-iptv-settings-v1\", \"salt\": \"EZVTllZNDL+CNYOvAqJUaA==\", \"data\": \"gAAAAABq6V3oORiJEfDNz-grK_a5wEYWmN-Ex2oD0ohPK6r_BeM27sIwqLf3-y6jLaZ_V0Fiyj5NEJEMFQiDWRSr9rklRio7CZK5uuKtIvEnY5YL-mXgGARdKY3iIXUbjv21IglEO9ElNpjuGvN65yMwf9Ly2SWTxNf4ZD2DUElsTiCzAhgLKGJmtswZFPJDMVu2qhOmNRVpBJGl9FIE32xJHVQFxztYUrLNDrH4gGqTNlUJ3MqeDAEPY_RUuLaKDrivM9uCUvbNZld3elzoymdHBVH05IlkllAkXezeX8unCFZAoF4po-WSHVYYamN1mqOhrcyVN0HscjIkcgIjVhQxjn3a0mGG2mqJIFvfhV_FrM13hbupViVRAUac7I5Pd-iN1DD0L-zb7B30TXcdALSBW8nVZsN4fA==\"}";
    const PYTHON_CONFIG_133: &str = "{\"playlists\": [{\"type\": \"xtream\", \"name\": \"Synthetic Provider\", \"server\": \"http://example.invalid:8080\", \"username\": \"synthetic-user-133\", \"password\": \"synthetic-pass-133\"}], \"epgs\": [\"http://example.invalid/guide.xml\"], \"language\": \"en\", \"volume\": 65}";

    #[test]
    fn decrypts_the_fresh_python_fixture_with_synthetic_credentials() {
        let contents = import_settings(PYTHON_BACKUP_133.as_bytes(), "interop-133-test").unwrap();
        assert_eq!(contents, PYTHON_CONFIG_133.as_bytes());
        assert_eq!(
            import_settings(PYTHON_BACKUP_133.as_bytes(), "wrong").unwrap_err(),
            "Wrong password or damaged backup."
        );
    }

    #[test]
    fn roundtrip_preserves_bytes_and_unicode() {
        let config = "{\"playlists\": [], \"epgs\": [], \"note\": \"Café — déjà vu\"}";
        let backup = export_settings(config.as_bytes(), "pässwörd").unwrap();
        let envelope: serde_json::Value = serde_json::from_slice(&backup).unwrap();
        assert_eq!(envelope["format"], "accessible-iptv-settings-v1");
        let salt = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            envelope["salt"].as_str().unwrap(),
        )
        .unwrap();
        assert_eq!(salt.len(), 16);
        let back = import_settings(&backup, "pässwörd").unwrap();
        assert_eq!(back, config.as_bytes());
    }

    #[test]
    fn wrong_password_and_tampering_report_wrong_or_damaged() {
        assert_eq!(
            import_settings(PYTHON_BACKUP.as_bytes(), "not the password").unwrap_err(),
            "Wrong password or damaged backup."
        );
        let mut tampered = PYTHON_BACKUP.to_string();
        let position = tampered.find("cvpxoY29").unwrap();
        tampered.replace_range(position..=position, "A");
        assert_eq!(
            import_settings(tampered.as_bytes(), PASSWORD).unwrap_err(),
            "Wrong password or damaged backup."
        );
    }

    #[test]
    fn empty_password_messages_match_python() {
        // Export: Python raises it from the cipher helper, uncaught.
        assert_eq!(
            export_settings(b"{\"playlists\": [], \"epgs\": []}", "").unwrap_err(),
            "A backup password is required."
        );
        // Import: Python raises it inside the decrypt try, so it
        // surfaces as wrong-or-damaged.
        assert_eq!(
            import_settings(PYTHON_BACKUP.as_bytes(), "").unwrap_err(),
            "Wrong password or damaged backup."
        );
    }

    #[test]
    fn envelope_validation_matches_python() {
        assert_eq!(
            import_settings(b"not json", PASSWORD).unwrap_err(),
            "Not an Accessible IPTV settings backup."
        );
        assert_eq!(
            import_settings(
                b"{\"format\": \"other\", \"salt\": \"\", \"data\": \"\"}",
                PASSWORD
            )
            .unwrap_err(),
            "Not an Accessible IPTV settings backup."
        );
        let short_salt = format!(
            "{{\"format\": \"accessible-iptv-settings-v1\", \"salt\": \"{}\", \"data\": \"eA==\"}}",
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, [7u8; 8])
        );
        assert_eq!(
            import_settings(short_salt.as_bytes(), PASSWORD).unwrap_err(),
            "Invalid backup salt."
        );
    }

    #[test]
    fn decrypted_config_must_have_playlists_and_epgs() {
        let backup = export_settings(b"{\"hello\": []}", PASSWORD).unwrap();
        assert_eq!(
            import_settings(&backup, PASSWORD).unwrap_err(),
            "Backup settings are invalid."
        );
    }

    #[test]
    fn verified_recovery_round_trips_and_rejects_tampering() {
        use super::{create_verified_recovery, verify_recovery};
        let config = PYTHON_CONFIG_133.as_bytes();
        let bytes = create_verified_recovery(config, "recovery-pass").unwrap();
        // The copy on its own decrypts to the same settings.
        assert_eq!(import_settings(&bytes, "recovery-pass").unwrap(), config);
        assert!(verify_recovery(&bytes, "recovery-pass", config).is_ok());
        // A wrong confirmation password cannot verify it.
        assert!(verify_recovery(&bytes, "wrong-pass", config).is_err());
        // A different settings object cannot verify against it.
        assert!(verify_recovery(&bytes, "recovery-pass", PYTHON_CONFIG_JSON.as_bytes()).is_err());
        // Tampered ciphertext fails verification.
        let mut tampered = bytes.clone();
        let middle = tampered.len() / 2;
        tampered[middle] ^= 0x01;
        assert!(verify_recovery(&tampered, "recovery-pass", config).is_err());
        // An empty recovery password is refused at creation.
        assert!(create_verified_recovery(config, "").is_err());
    }
}
