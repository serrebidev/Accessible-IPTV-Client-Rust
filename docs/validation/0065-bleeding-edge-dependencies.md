# 0065 — Bleeding-edge dependency pass (2026-11-03)

Brandon's standing rule (2026-11-03): "Automatically resolve. I am not
married to old dependencies. I want bleeding edge unless I can't."
Dependency conflicts and conservative pins resolve toward the newest
versions, including major bumps with code adaptation; anything that
genuinely cannot move gets named with its reason.

## What moved (workspace requirements + lockfile)

- sha2 0.10 → 0.11, hmac 0.12 → 0.13, pbkdf2 0.12 → 0.13 (RustCrypto
  generation jump; aes 0.8 → 0.9, cbc 0.1 → 0.2 underneath)
- quick-xml 0.36 → 0.42 (the string-based API generation)
- rusqlite 0.32 → 0.41 (bundled libsqlite3-sys 0.39)
- libloading 0.8 → 0.9, base64 0.22 → 0.23, getrandom 0.2 → 0.4
- Exact pins on wxdragon and live-region relaxed to caret ranges
  (both already at their newest published versions: 0.9.24 / 0.3.3)
- Caret refresh in the same `cargo update`: serde 1.0.229,
  serde_json 1.0.154, chrono 0.4.45, chrono-tz 0.10.4,
  flate2 1.1.10, ureq 3.5.1

## What could not move, and why

- wxdragon 0.9.24, live-region 0.3.3, ship-shape 0.5.0, minisign
  0.10.0: already the newest versions published on crates.io
  (verified 2026-11-03). Nothing newer exists to move to.
- The lockfile still carries the previous RustCrypto generation
  (sha2 0.10.9, digest 0.10.7, hmac 0.12.1, pbkdf2 0.12.2,
  libloading 0.8.9, getrandom 0.2.17) as transitive dependencies of
  minisign 0.10 / ship-shape 0.5.0. Our own crates are all on the
  new generation; the old stack retires when those two publish
  against it.

## Code adaptation

- settings_backup.rs: `getrandom::fill` (0.4 rename); cbc 0.2's
  `BlockModeEncrypt`/`BlockModeDecrypt` + `encrypt_padded_vec` /
  `decrypt_padded_vec`; hmac 0.13 `KeyInit` import for
  `new_from_slice`.
- sha2 0.11's digest output has no `LowerHex`: new shared
  `crate::hex_lower` helper (lib.rs), used at all five digest sites
  (bookmarks, cache, sources ×2, proxy token).
- epg.rs on quick-xml 0.42: names and attribute keys/values are
  `&str`-based; text events arrive entity-decoded from the reader
  (`unescape()` is gone); CDATA pushes its `&str` content directly;
  `local_name` works on `&str`.

## Evidence

- Local: `cargo fmt --all -- --check` clean; `cargo clippy
  --workspace --all-targets -- -D warnings` exit 0; `cargo test
  --workspace` 310 passed, 0 failed — including the settings-backup
  Fernet/PBKDF2-HMAC-SHA256 interop fixtures (the Python-written
  synthetic backup still decrypts byte-for-byte on the new crypto
  generation) and the full EPG parsing suite on quick-xml 0.42.
- Parity: `tools/check-playlist-parity.py` against the pinned
  Python clone (9aa89e053225ef2c83c4f3659775a00723012ff7) PASS.
- `git diff --check` clean.
