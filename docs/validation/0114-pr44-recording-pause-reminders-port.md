# 0114 — Python PR #44 port: recording-rule pauses, reminder guide changes, exit drain

Port target: Python `serrebi/Accessible-IPTV-Client` PR #44
"Improve recording reliability, scheduling and local playback"
(Tibor), merged on Python main as f0820393 and shipped as Python
v1.152.2. Ported under Brandon's 2026-11-27 standing rule: every
PR accepted into the Python client also lands in the Rust client,
Python behaviour as the spec, porting only what Rust lacks.

Brandon's seven items mapped:

1. Unfinished recordings in the Library with Resume — already
   present (PR #36 port, v0.4.28: partial/stopped rows play and
   resume from the recording store). No portal-side recording
   downloads exist in either client; nothing to add.
2. Recording plans sorted by start time — already present
   (`dvr.rs` sorts jobs by `start_ts`).
3. Stable series identity and a duplicate-plan guard — already
   present (`program_episode_key`, `taken_episode_keys`,
   `find_duplicate`).
4. An active recording's job survives a restart — already
   present (`DvrSchedule::load` re-arms recording/stopping jobs
   exactly like Python: window open re-schedules, window closed
   fails the job "Interrupted by application restart." and books
   the next occurrence; jobs are never auto-deleted).
5. Completed recordings play in-app from the Library — FIXED in
   this port. The PR #36 port deliberately played through the
   configured player; the Library's Play now forces the built-in
   player (Python's `force_internal`) and starts directly at the
   saved recording position. Failed or undownloaded entries
   already produced a visible error instead of opening a player.
6. A portal recording whose stream start fails gets a visible
   error state — the app side is the Stalker play path below: a
   failed portal resolution now shows a Playback Error box
   instead of only a status line. (No portal-side recording
   list exists in Python's providers; nothing else to port.)
7. Portal recordings list state — no counterpart in the Python
   code (its providers expose no portal recordings list); the
   Rust DVR views already report job state from the schedule.

## What landed

Core (iptv-core):

- DVR recording-rule pause windows: `DvrJob.pause_dates`
  (serde skip-if-none, Python file shape, so plan files load
  both ways), `valid_pause_period`, paused-occurrence checks in
  the tick (a due paused episode is skipped and the next
  occurrence booked; a window that closed during a pause is
  skipped, not missed), series scans and `taken_episode_keys`
  honour pauses, and `DvrSchedule::set_pause_period` with
  Python's snapshot/stale errors. Next-occurrence helpers take
  an `earliest_date`, matching Python's post-pause search.
- Pause-aware planning: connection-limit peaks, conflict rows
  and the wake-up planner skip paused occurrences; a paused
  time-repeat rule wakes for its first post-pause occurrence.
- `planning_tools::recording_calendar` (upcoming, unpaused
  recordings as .ics with stable `booking:<id>` UIDs) and
  `pause_preview` (first skipped / next following recording).
- Portal station identity: `station_key`, `playable`,
  `media_identity` in reminders; favourites, saved searches,
  other-airings and transfers match Stalker channels by the
  canonical identity as well as the legacy key. Reminder
  guide/episode fingerprints, `retime_reminder`,
  `guide_changes`, and the `notify_reminder_guide_changes`
  setting (default off).
- Recording engine shutdown gate: `begin_shutdown` stops
  actives with reason "exit" and refuses new starts with "The
  application is closing."; `classify_outcome` follows PR #44's
  order (an exit stop with output is partial, without is
  stopped). Exit prompts name active recordings
  (`exit_prompts_with_active`, catch-up downloads excluded).
- Recording Library display folders (`folders_display`, no
  directory creation or canonicalisation), cast proxy bind
  retry when the address is not yet available, subtitle cue
  `<br>` read as a space, recording format labels renamed to
  "MP4 (original video, AAC audio)" and "MKA (all audio
  tracks, original encoding)", `LatestTask::cancel`.

App (iptv-app):

- Scheduled Recordings: "Paused" label, Pause Recording
  Rule... dialog (two date pickers, Remove the pause,
  confirmation naming the first skipped and next following
  recordings), Export Selected Recordings to Calendar....
- Programme Reminders: Review Guide Changes... with the
  confirm and keep-postponement flow, the notify opt-in
  checkbox, and the after-guide-update notice.
- Exit flow: prompts run through `exit_prompts_with_active`;
  once they pass the app marks itself closing, stops the DVR
  tick and recording notifications, asks recordings to stop,
  and either finishes inline or defers — the status reads
  "Finishing recordings before exit…" and `poll_recordings`
  completes the exit when the last recording reports. A DVR
  job stopped by the exit reports "Stopped because the app
  exited."
- Stalker playback: the existing provider-worker resolution
  gains Python's latest-wins guards — a newer play or a stop
  (playback generation) or a newer Stalker dispatch drops a
  late result, closing drops it too; the status reads
  "Connecting to {channel}…" and a failure shows the
  Playback Error box.
- Recording Library: one scan worker per folder with a scan
  version (stale deliveries dropped, folder choice live
  during scans, visited-folder dedupe), display-only folder
  list, and Play forced to the built-in player at the saved
  position.
- Player: Playback menu gains Find Text in Recording
  Subtitles..., Recording Playback Speed... and Normal speed
  for seekable local recordings; the rate applies through a
  new optional `libvlc_media_player_set_rate` binding and a
  fresh load resets a non-1.0 rate. Subtitle matches seek to
  the cue start plus the current subtitle delay.
- Favourite Lists replacement search accepts any playable
  channel (a Stalker channel with no direct URL included).
- Report an Issue: a failed browser launch loops a
  Try Again / Copy Report / Cancel dialog; the copy uses the
  flushing clipboard helper.

## Deliberate divergences

- The two Python spots that match a displayed channel by
  `media_identity` (programme → channel select-and-play) have
  no Rust counterpart: the Rust channel model carries exact
  playlist indices, which is stronger than identity matching.
- The provider worker has no close API; on exit its thread
  ends with the process and pending play results are dropped
  by the closing guard, which is the observable Python
  behaviour.
- The reminders notify checkbox uses the same `set_and_save`
  as the existing recording one; Python's revert-on-save-
  failure cannot be detected through that API.
- Library scans resolve the default folder through the
  existing path helper (no %-expansion), as before this port.
- Subtitle-find matching uses Unicode lowercase; Python uses
  casefold. Identical for the Latin scripts the catalogues
  ship; noted for completeness.

## Data compatibility

`pause_dates` serialises exactly as Python writes it (a
two-element list of `YYYY-MM-DD` strings, absent when unset);
reminder fingerprints are optional fields Python ignores when
absent and reads when present. No other file shapes changed.

## Translations

34 new msgids per language × 13 languages, every one
harvested from the Python catalogues at the PR #44 head with
matching placeholders (msgfmt --check clean). The remaining
new strings were already in the Rust catalogues.

## Gate

fmt --check clean; clippy --locked --workspace --all-targets
-D warnings clean; cargo test --locked --workspace: 555 passed,
0 failed (iptv-core 510, iptv-app 25, iptv-vlc 9, the rest
small suites); playlist parity PASS against the pinned Python
tree (a29ad9a3, v1.151.6); git diff --check clean.
Brandon's NVDA pass over the new dialogs and flows is the
acceptance check, as usual.
