# 0102 — C1: recording planning engine (round 3)

Port target: Python v1.151.6 (`a29ad9a3`) `dvr.py`, `recording_edits.py`,
`recording_edits_ui.py`, `planning_tools.py` (weekday recurrence),
`manual_schedule.py`, `recording_outcome.py` and the Scheduled Recordings
window in `main.py`. Plan: `docs/superpowers/plans/2026-11-23-c1-recording-planning.md`.

## What landed

- Guide keeps XMLTV episode numbers (`xmltv_ns` first) and sub-titles;
  the store migrates old databases in place (`epg.rs`, `epgstore.rs`).
- `scheduled_recordings.json` keeps every field Python writes (weekdays,
  series_source, future_padding, series_padding_changes, manual, any
  newer key). Entries are validated as Python does; an unreadable file,
  or one with invalid entries, is copied to `<path>.corrupt-<hex>` first,
  and writes are refused when that copy fails. Saves are synced
  temp-file renames; user edits are transactions that leave the
  schedule unchanged when the write fails.
- Weekday recurrence with Python's wall-clock rules across clock
  changes; stable successor ids (`uuid5(NAMESPACE_URL, "iptv-dvr:<id>")`);
  missed, failed and restart-interrupted repeats queue their successor;
  Skip This Occurrence; Repeat on Weekdays.
- Episode identity (`program_episode_key`) and rerun suppression for
  series rules; series episodes carry `series_source`; cancelling or
  deleting a rule takes its pending episodes; a finished rule can be
  cancelled.
- Outcomes: the engine records `stop_reason`; Python's `classify` order;
  scheduled jobs finish `partial` or `stopped`; Scheduled Recordings
  shows Python's translated status words.
- Padding for This Recording (with Apply to future occurrences too),
  Find Another Airing, Review Guide Changes and the after-import notice
  option, all as reviewed edits published against the snapshot they
  were computed from.
- Recordings > Schedule by Time... for a new recording (it was missing:
  Rust only had the Edit Times form), with the weekday list and
  Python's clock-change choice for a repeated local time.
- Fixed on the way: View EPG, catch-up lists and the series scan now
  use the same guide match as now/next, so channels matched by name
  have a guide; every scheduled recording reached the Recording
  Library as "Stopped by user".

## Evidence

- Core tests beside the code: `dvr.rs` (round trip of a Python job,
  corrupt-file preservation, write block, recurrence, skip, weekdays,
  episode keys, series booking, outcomes, manual jobs),
  `recording_edits.rs` (Python's padding, move and series-child tests,
  other airings, guide changes), `epgstore.rs`, `epg.rs`,
  `recording_library.rs`.
- Recurrence goldens were checked against Python itself under
  `TZ=Europe/Budapest` (WSL); episode-key goldens are Python's output.
- `cargo test --workspace` and `cargo clippy --workspace --all-targets
  -- -D warnings` green on Windows.

## Brandon's NVDA check

1. Recordings > Schedule by Time...: the fields read as labelled, the
   weekday list is reachable with Tab and Space toggles a day; a start
   on an unchosen day is refused with Python's message.
2. Scheduled Recordings: Tab reaches the notify check box, Edit Times,
   Recording Conflicts, Repeat on Weekdays, Review Guide Changes.
3. Applications key on a pending recurring entry: Padding for This
   Recording, Find Another Airing, Skip This Occurrence; Skip asks
   first and then says the next recording time.
4. Find Another Airing: the list of airings is focused and each row
   reads title, channel, source, times and Same episode or Possible
   repeat.
5. A schedule file from the Python client opens with every recording,
   and saving it keeps weekdays and series links (check the file or
   reopen Python).

## Residuals

- Recover from Channel Archive is C2.
- Guide assignment (C10) is not yet consulted by Review Guide Changes
  or Find Another Airing; the matcher is `match_guide_channel` until
  C10 lands.
- Python's background operation guard (one save at a time per window)
  is not mirrored: Rust edits run on the UI thread and finish before
  the next command can start.
