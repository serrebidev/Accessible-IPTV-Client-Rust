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

#[cfg(test)]
mod tests {
    use super::{export_settings, import_settings};

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
