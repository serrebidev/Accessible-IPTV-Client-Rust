# 0060 — Verified recovery backups before imports (issue #136)

Owner gate: Brandon accepted the proposed behavior as written
in the ticket, in the main chat on 2026-11-02 ("Accept both
behaviours as proposed"); the acceptance is recorded on #136.
Builds on the #133 backup format: same envelope, same
cryptography, fully interoperable with Python.

## Behavior

Import Settings now runs in this order, and every early exit
leaves the settings untouched:

1. Active-work check: an in-progress recording or an
   unfinished download stops the import before anything else.
2. Recovery password: entered and confirmed through the same
   double-entry prompt Export uses; held only in local
   variables for the duration of the import, never persisted.
3. Recovery location: a save dialog suggesting a timestamped
   name (`settings-recovery-<date time>.aiptv`); the native
   overwrite prompt confirms replacing an existing file; the
   file stays where the user put it until the user deletes it.
4. Create and verify: the core `create_verified_recovery`
   encrypts the current settings and immediately decrypts the
   result, requiring the parsed settings to match exactly.
   The copy is written, read back from disk, and verified
   again against the current settings — a copy that fails at
   either stage blocks the import with the reason announced.
5. Only now is the incoming backup chosen and read, its
   password entered, and its contents decrypted and checked.
6. The replace confirmation names the recovery copy's path.
   At the moment of applying, the active-work check runs
   again: recordings or downloads that started while the
   dialogs were open cancel the swap, settings unchanged, and
   the message says the recovery copy remains saved.

Canceling any dialog (password, save, open, confirm) ends the
import quietly with settings unchanged; if the recovery copy
was already saved it simply remains as a backup the user owns.

## Automated evidence (2026-11-02)

- `cargo test --workspace`: 184 passed, 0 failed. New core
  test: a verified recovery copy decrypts back to the exact
  settings; a wrong confirmation password cannot verify it; a
  different settings object cannot verify against it;
  tampered ciphertext fails; an empty recovery password is
  refused at creation. Ordering is structural in the adapter:
  the incoming file is not read until `make_recovery_copy`
  has returned a verified path.
- fmt clean, clippy `--workspace --all-targets -D warnings`
  clean, playlist parity PASS against the pinned Python.

## Not covered here

The dialog sequence under NVDA (focus returning to the main
window after each modal step) is owner-heard evidence tracked
by the replacement-gate pack (#137) and is not claimed here.
