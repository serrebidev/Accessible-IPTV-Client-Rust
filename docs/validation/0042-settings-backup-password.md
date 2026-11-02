# 0042 — Password-protected settings backup (issue #99)

Date: 2026-11-02.

Settings backup now uses Python's encrypted format, byte
interchangeable with the Python client:

- Core (crates/iptv-core/src/settings_backup.rs) mirrors
  settings_backup.py exactly: envelope {format:
  "accessible-iptv-settings-v1", salt: standard base64, 16
  random bytes; data: Fernet token}; key = PBKDF2-HMAC-
  SHA256(password, salt, 600,000 iterations) split into
  signing/encryption halves; AES-128-CBC + PKCS7 with
  HMAC-SHA256; urlsafe-base64 token, 0x80 version byte,
  big-endian timestamp with the +60 s future-skew rejection.
- Proof: a core test decrypts a backup written by Python's
  own cryptography stack byte-for-byte, and the reverse
  direction was verified live during development (Python's
  import_settings opened a Rust-written backup and rejected
  a wrong password on it). Empty-password and envelope
  validation errors match Python's messages.
- Adapter flow follows Python's main.py: export asks the
  password twice (masked dialogs captioned "Settings
  Backup"; "A backup password is required." /
  "The passwords do not match." as modal warnings), then a
  save dialog for "Accessible IPTV backup (*.aiptv)", then
  "Encrypted settings backup saved." Import: open dialog,
  one password entry, decrypt errors reported as "Could not
  import settings: {error}", a Yes/No "Replace current
  settings with this backup? Restart the app after
  importing." confirmation, then the config file swap and
  "Settings imported. Restart the app to use them."
- The old plain-JSON export/import is replaced outright;
  Python never shipped one.

Evidence class: automated — fmt clean, clippy zero warnings,
workspace tests pass (including the Python fixture test),
playlist parity PASS. Owner-heard NVDA evidence remains #86.
