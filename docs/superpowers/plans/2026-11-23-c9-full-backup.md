# C9 Full Backup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Python v1.151.6's full backup (`full_backup.py`, format `accessible-iptv-settings-v2`) in Rust, interchangeable with Python both ways, plus Python's reminder file, which Rust never used.

**Architecture:** Core `settings_backup.rs` gains the v2 bundle (validate, export, import, stage, apply); core `reminders.rs` gains Python's file store (`programme-reminders.json`); the app exports and imports bundles and finishes a staged restore at startup before any scheduler starts.

**Spec:** `docs/superpowers/specs/2026-11-23-python-parity-round3-design.md` (C9). Python `full_backup.py`, `programme_reminders.py` (`normalize`, `ReminderStore`), `saved_searches.py` (`normalize_seen`, `read_seen`), `personal_tools_ui.py` (`_export_full_settings`, `_import_full_settings`), `main.py:966`.

## Global Constraints

- Envelope `{"format": "accessible-iptv-settings-v2", "salt": <b64 16 bytes>, "data": <Fernet token>}`, key and token exactly as v1 (`settings_backup._cipher`). 16 MiB limit. A v1 backup imports as settings only.
- Bundle keys exactly `{"config"}` or `{"config", "jobs", "reminders", "search_seen"}`; Python's validation messages verbatim.
- Restore paths: config write target, `scheduled_recordings.json`, `programme-reminders.json`, `programme-search-state.json` beside the schedule; journal `pending-restore.json` beside the schedule.
- Reminder file `{"version": 1, "reminders": [...]}`, Python `normalize` rules (sha256 ids, 500 cap, `notify_at` kept), corrupt-file preservation as the schedule's.

## Review Focus

- A Python-written v2 backup imports in Rust, and a Rust export imports in Python (round trip through `python -I` with the Python checkout).
- A crash between staging and startup finishes the restore on the next start, before the DVR scheduler loads; a failed apply leaves the journal for the next start.
- Reminders a Rust build kept in the config key move to the file once and are not lost or duplicated.
- A recording that was running or a scheduled one already due in the backup comes back as missed, a time repeat queues its next future occurrence once.

---

### Task 1: Reminder file store
- [ ] Tests: Python-shaped file round-trips (including `notify_at`); invalid rows skipped with a `.corrupt-` copy and Python's warning; legacy config rows migrate once.
- [ ] `Reminder` gains `notify_at: Option<i64>`; `normalize_reminders` follows Python `normalize` (id = sha256 of `json.dumps([channel_key, start])`, lead 0 to 120, status set, `notify_at` kept only inside `[start - lead*60, end)`, ordering and 500 cap).
- [ ] `ReminderStore { path, rows, load_warning, load_error }` with `load`, `save(rows) -> Result`; app `Persist::reminders`/`save_reminders` use it; migration from `programme_reminders` config key.
- [ ] Commit `fix(reminders): keep reminders in Python's reminder file`.

### Task 2: Bundle core
- [ ] Tests: validate rejects extra keys and bad jobs; running and due jobs become missed with `restore_next_occurrence`; export then import round trip; v1 import gives settings only; Python oracle round trip both ways (`tools/backup-oracle.py`).
- [ ] `validate_bundle`, `export_bundle`, `import_bundle`, `stage_restore`, `apply_pending` in `settings_backup.rs`; `normalize_seen` in a new `saved_searches.rs` (C11 grows it).
- [ ] Commit `feat(backup): Python's full backup with schedules, reminders and searches`.

### Task 3: App
- [ ] Options > Export Settings writes v2 with Python's success text; Import Settings previews counts, refuses while recordings or downloads run, stages, and closes the app; startup applies a staged restore before loading the schedule and shows Python's failure box when it cannot.
- [ ] Validation 0103; ledger C9 fixed. Commit `feat(backup): export and import full backups`.
