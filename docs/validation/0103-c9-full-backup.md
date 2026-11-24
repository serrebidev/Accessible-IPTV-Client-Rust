# 0103 — C9: full backup and Python's reminder file (round 3)

Port target: Python v1.151.6 `full_backup.py`, `programme_reminders.py`
(`normalize`, `ReminderStore`), `saved_searches.py` (`normalize_seen`,
`read_seen`), `personal_tools_ui.py` (`_export_full_settings`,
`_import_full_settings`), `main.py:966`. Plan:
`docs/superpowers/plans/2026-11-23-c9-full-backup.md`.

## What landed

- Reminders live in Python's `programme-reminders.json` (Rust kept them
  in the config key `programme_reminders`, which Python never read, so
  reminders did not cross between the clients). Times are written as
  Python's floats, because Python's ids hash them that way; `notify_at`
  is kept and honoured; invalid rows are skipped after the original is
  preserved; writes stop when it cannot be. Rows in the old config key
  move into the file once.
- Full backups in Python's v2 format: settings, the recording schedule,
  reminders and the saved-search history, with Python's validation and
  messages. Export writes it; import previews the counts, refuses while
  recordings or downloads run, stages `pending-restore.json`, and closes
  the app; the next start finishes the restore before any scheduler
  reads these files. A running or due recording comes back as missed,
  and a time repeat queues its next future occurrence once. A v1 backup
  imports as settings only.
- Kept from Rust (issue #136): the verified recovery copy before an
  import, now a full backup so the schedule and reminders can be
  restored too.

## Evidence

- Core tests: `reminders.rs` (Python file round trip byte for byte,
  invalid rows preserved), `settings_backup.rs` (round trip, validation,
  journal applied on the next start, v1 import), `saved_searches.rs`.
- `tools/backup-oracle.py`: a Python-made v2 backup imports in Rust and
  a Rust-made one imports in Python (both run 2026-11-23).
- `cargo test --workspace` and clippy green on Windows.

## Brandon's NVDA check

1. With reminders set in the Python client, start Rust: Programme
   Reminders lists them.
2. Options > Export Settings: Python's saved message is read.
3. Options > Import Settings with a Python backup: after the recovery
   copy, the confirmation reads the recording and reminder counts and
   says the app will close; after reopening, Scheduled Recordings and
   Programme Reminders show the backup's entries.

## Residuals

- The saved-search rules and scans are C11; the backup already carries
  their history file.
- Python's backup worker runs off the UI thread; Rust's export and
  import run on it (PBKDF2 takes about a second in a release build).
