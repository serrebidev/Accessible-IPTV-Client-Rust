# C1 Recording Planning Engine Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port Python v1.151.6's recording planning engine (`dvr.py`, `recording_edits.py`, `planning_tools.py` weekday recurrence, `recording_outcome.py`) and its Scheduled Recordings commands into the Rust client, keeping `scheduled_recordings.json` interchangeable with Python.

**Architecture:** Core logic in `crates/iptv-core/src/dvr.rs` plus one new `crates/iptv-core/src/recording_edits.rs`; guide metadata in `epg.rs`/`epgstore.rs`; UI in `crates/iptv-app/src/main.rs` Scheduled Recordings window and Schedule by Time dialog. Every schedule mutation returns `Result` and never loses the previous file.

**Tech Stack:** Rust, serde_json, chrono (`Local` in production, chrono-tz in tests), rusqlite, `uuid` with the `v5` feature (new: nothing in the tree does SHA-1), wxDragon.

**Spec:** `docs/superpowers/specs/2026-11-23-python-parity-round3-design.md` (C1). Python reference `C:\Users\admin\git\Accessible-IPTV-Client` at `a29ad9a3`.

## Global Constraints

- Python behavior wins; strings verbatim from Python, translated through patois catalogues.
- `scheduled_recordings.json` stays `{"version": 1, "jobs": [...]}`; every field Python writes survives a Rust load and save.
- Statuses: `scheduled recording stopping completed partial stopped failed missed canceled skipped`, plus Rust's existing `waiting` (re-armed to `scheduled` on load, as today).
- Repeats: `"" daily weekly series weekdays`; time repeats are `daily weekly weekdays`.
- Padding values are integers 0 to 180.
- No UI-thread blocking: guide queries (Review Guide Changes, Find Another Airing) run on a worker with the latest-wins `background::LatestTask`.
- clippy `-D warnings`, rustfmt, tests beside the code.

## Review Focus

- A Python-written schedule containing `weekdays`, `series_source`, `future_padding`, `series_padding_changes`, `manual`, and a `program` with `episode`/`subtitle`/`description` must round-trip byte-for-field through Rust load and save. Test in Task 1.
- A schedule file that is not JSON, or has one invalid job, must be copied to `<path>.corrupt-<32 hex>` before any write; if the copy fails, every write must refuse. Test in Task 1.
- A weekday recurrence across a DST change (Europe/Budapest 2026-03-29 and 2026-10-25) keeps wall-clock times and skips a nonexistent 02:30 start. Test in Task 2, with `TZ` fixed through chrono-tz, not the host zone.
- A series rule must not book the same episode twice when the guide spells the episode `0.4.`, `S01E05` and `1x5`. Test in Task 3.
- Successor ids are deterministic (`uuid5(NAMESPACE_URL, "iptv-dvr:" + id).hex`), so a restart cannot queue a second copy of the next occurrence. Test in Task 2.

---

### Task 0: Guide carries episode and sub-title

**Files:** Modify `crates/iptv-core/src/epg.rs` (XMLTV programme parse, `Programme`), `crates/iptv-core/src/epgstore.rs` (schema migration adding `episode TEXT NOT NULL DEFAULT ''`, `subtitle TEXT NOT NULL DEFAULT ''`; inserts; reads). Test in the same files.

**Interfaces:** Produces `Programme { channel_id, start, stop, title, description, episode: String, subtitle: String }`.

- [ ] Test `parses_episode_and_subtitle`: a programme with two `<episode-num>` (`onscreen` "S01E05" first, `xmltv_ns` "0.4." second) and `<sub-title>` yields `episode == "0.4."`, `subtitle` trimmed. A programme with only `onscreen` yields that text.
- [ ] Test `store_round_trips_episode_fields` and `migrates_old_store_without_columns` (an old-schema file gains the columns, rows read back with empty strings).
- [ ] Implement: xmltv_ns wins, else first non-empty; first non-empty sub-title. Re-import merge keeps Python's rule: a changed title replaces episode/subtitle; an empty stored value is filled.
- [ ] Run `cargo test -p iptv-core epg` and commit `feat(epg): keep episode numbers and sub-titles from the guide`.

### Task 1: Schedule store fidelity and recovery

**Files:** Modify `crates/iptv-core/src/dvr.rs`.

**Interfaces:** `DvrJob` gains `#[serde(default)]` on every field, `weekdays: Option<Vec<u8>>`, `future_padding: Option<[i64; 2]>`, `series_padding_changes: Option<Vec<(String, i64, i64)>>`, `series_source: Option<String>`, `manual: Option<bool>`, and `#[serde(flatten)] extra: serde_json::Map<String, Value>` (all `skip_serializing_if` empty/None). `DvrSchedule::load` sets `pub load_warning: String`; `save(&self) -> Result<(), String>` refuses while the load error is set and writes temp file + fsync + rename.

- [ ] Test `python_schedule_round_trips` with a fixture job carrying every field above; assert the saved JSON value equals the input for every key.
- [ ] Test `unreadable_schedule_is_preserved`: invalid JSON and one invalid job (status `"bogus"`) each produce exactly one `.corrupt-` copy with the original bytes and Python's warning text; valid jobs still load.
- [ ] Test `blocked_when_backup_fails` (read-only directory): `save()` returns `Err` with Python's "could not be read or backed up" text.
- [ ] Implement `valid_stored_job` per Python `_valid_stored_job` and the load flow per Python `load` (interrupted recurring jobs that ended get a successor; see Task 2's `next_time_occurrence`).
- [ ] Switch every mutating method to return `Result` and the app to show the error and keep the previous state (Python: "If saving a cancellation, deletion or repeat-rule change fails, the previous schedule is kept and an error is shown").
- [ ] `cargo test -p iptv-core dvr`, commit `fix(dvr): keep every schedule field and preserve an unreadable schedule`.

### Task 2: Weekday recurrence, skip and successors

**Files:** Modify `crates/iptv-core/src/dvr.rs`; Python sources `planning_tools.next_weekday_job`, `manual_schedule.local_candidates`, `dvr.next_time_occurrence`, `skip_occurrence`, `set_weekdays`, `edit_timing(weekdays=)`.

**Interfaces:** `pub fn next_weekday_job(job: &DvrJob, now: f64, weekdays: &[u8], future_only: bool, tz: &impl TimeZone) -> Result<Option<DvrJob>, String>`; `pub fn next_time_occurrence(job, now, tz) -> Result<DvrJob, String>` setting the uuid5 id; `DvrSchedule::{set_weekdays, skip_occurrence(job_id, expected_start)}`; `edit_timing` gains `weekdays: Option<&[u8]>`. Production passes `chrono::Local`.

- [ ] Tests (Europe/Budapest via chrono-tz): Monday/Wednesday rule from a Monday 21:00 job gives Wednesday 21:00; overnight 23:30 to 00:30 keeps the one-day offset; a 02:30 start on 2026-03-29 is skipped to the next selected day; on 2026-10-25 the fold uses the original occurrence; `future_padding` replaces padding on the successor and is removed from it; successor id equals `uuid5(NAMESPACE_URL, "iptv-dvr:<id>")` hex (golden from Python: compute with `python -c "import uuid;print(uuid.uuid5(uuid.NAMESPACE_URL,'iptv-dvr:abc').hex)"` and pin the value).
- [ ] Tests: `skip_occurrence` marks `skipped` and adds exactly one successor; a stale `expected_start` errors with "Select one pending recurring recording."; missed and failed time repeats queue a successor (Python `_set_status`).
- [ ] Add `uuid = { version = "1", default-features = false, features = ["v5"] }` to `[workspace.dependencies]` and `iptv-core` (the lockfile has no SHA-1 crate; checked 2026-11-23).
- [ ] Replace the old `schedule_next_occurrence` day-shift with `next_time_occurrence`.
- [ ] Commit `feat(dvr): weekday recurrence, skip occurrence and stable successors`.

### Task 3: Episode identity and series rules

**Files:** Modify `crates/iptv-core/src/dvr.rs`, `crates/iptv-app/src/main.rs` (`apply_series_programmes`, programme snapshots in every `build_job` caller).

**Interfaces:** `pub fn program_episode_key(program: &Value) -> Option<String>`; `pub fn taken_episode_keys(jobs: &[DvrJob], rule_id: &str) -> HashSet<String>`; `DvrSchedule::add_series_episodes(rule: &DvrJob, candidates: Vec<DvrJob>) -> Result<bool, String>`; `NewJob` gains `episode`, `subtitle`, `description` written into `program`; `cancel_job`/`remove_job` cascade to scheduled children with `series_source == rule id` (messages "Series recording canceled.").

- [ ] Tests: the key goldens from Python `tests/test_dvr_episode_key.py` (port every case); `taken_episode_keys` counts active and completed only; `add_series_episodes` skips taken episodes, same-slot duplicates and ended airings, and refuses when the rule changed (any of Python's compared fields).
- [ ] Implement; series padding for a candidate comes from `series_padding_at(rule, programme_start_xmltv)` (Task 5 adds the writer; reader lands here).
- [ ] Commit `feat(dvr): skip repeats of an episode already recorded`.

### Task 4: Partial and stopped outcomes

**Files:** Modify `crates/iptv-core/src/dvr.rs` (`mark_finished` takes `outcome: Option<&str>`), the recording engine's finish report (`crates/iptv-core/src/recording.rs`) and `poll_recordings` in the app.

**Interfaces:** `pub fn classify_outcome(produced: bool, exiting: bool, finalize_timed_out: bool, stop_reason: &str, stopped_by_user: bool, returncode: i32, planned_seconds: Option<f64>, written_seconds: Option<f64>) -> &'static str` returning `failed partial stopped completed` per Python `recording_outcome.classify`.

- [ ] Table test of every branch of `classify`, including the tolerance `max(5, min(30, expected * 0.02))`.
- [ ] Wire: a user stop of a scheduled job finishes `stopped`; exit during a recording finishes `partial`; Scheduled Recordings shows Python's status words.
- [ ] Commit `feat(dvr): mark interrupted recordings partial and user stops as stopped`.

### Task 5: Padding, retime, move and warnings

**Files:** Create `crates/iptv-core/src/recording_edits.rs` (register in `lib.rs`); modify `dvr.rs` for `edit_snapshot() -> (u64, BTreeMap<String, DvrJob>)` and `apply_reviewed_edit(revision, changes, future_start) -> Result<(), String>`.

**Interfaces:** `with_padding`, `edit_padding(job, before, after, future, jobs, now)`, `move_airing(job, channel, programme, fmt, jobs, now)`, `retime_booking(job, programme, jobs, now)`, `warnings(config, changes, jobs) -> Vec<String>`; each returns `Result<BTreeMap<String, DvrJob>, String>` (or the list) with Python's messages.

- [ ] Port the cases of Python `tests/test_recording_edits_and_transfer.py` and `tests/test_recording_guide_changes.py` that do not touch wx.
- [ ] Commit `feat(dvr): per-recording padding, retime and move as reviewed edits`.

### Task 6: Guide comparisons

**Files:** Modify `crates/iptv-core/src/recording_edits.rs`; reads through `epgstore.rs`.

**Interfaces:** `changed_guide_times(store, jobs, now, canceled) -> Vec<(DvrJob, Programme, bool)>`; `other_airings(store, job, channels, now, canceled, maximum=100) -> (Vec<(Channel, Programme, bool)>, bool)`, guide channel per `guide_match` plus the C10 override once it lands (until then, automatic matching only; leave a `ponytail:` comment naming C10).

- [ ] Tests with an in-memory store: six-hour window, unchanged broadcast clears candidates, exact episode match vs overlap, manual and time-repeat jobs ignored; other airings over 14 days, numbered episode filtering including `:part` rules, same-time other-source rule, truncation flag at 100.
- [ ] Commit `feat(dvr): find guide changes and other airings for a booking`.

### Task 7: Scheduled Recordings commands

**Files:** Modify `crates/iptv-app/src/main.rs` (Scheduled Recordings context menu, Schedule by Time dialog, Options menu, post-import hook), catalogues under `crates/iptv-app/po/`.

- [ ] Context menu on a pending entry, in Python's order and wording (`main.py`, `recording_edits_ui.py`): Skip This Occurrence (confirmation names the next time), Repeat on Weekdays..., Padding for This Recording... (with "Apply to future occurrences too"), Find Another Airing..., and the window's Review Guide Changes... button. Each async query shows Python's progress and result texts on the live region, never moves focus.
- [ ] Schedule by Time gains Python's seven weekday check boxes as one labelled group; first start must fall on a selected day.
- [ ] Recordings menu option "Notify me of recording time changes after guide updates" (config key and default from Python `main.py`); after a successful import, run `changed_guide_times` on a worker and queue Python's notice through `show_or_queue_notification`.
- [ ] Manual check list for Brandon in `docs/validation/0102-c1-recording-planning.md`; ledger C1 marked fixed.
- [ ] `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, Windows build; commit `feat(recordings): Scheduled Recordings planning commands`.
