# 0116 — Python PR #46 port: recording workflows and playback tools

Date: 2026-11-27. Branch: port-pr46. Ships as: v0.4.41.
Python spec: serrebi/Accessible-IPTV-Client PR #46 (htibcsike),
"Improve recording workflows and playback tools", merged e3b6c22a6a
(head 5ae62f9c96), shipped as Python v1.152.4. Port rule: Brandon,
2026-11-27 — every accepted Python PR is ported to the Rust client.
Data compatibility both ways stays in force: the only persisted
shapes touched are the `saved_recording_queues` config value (the
same JSON Python writes: name → entries with path, directory,
title, size, modified_ns) and recording plan/job files, which are
unchanged.

## Ported

Core (iptv-core):

- `planning_tools::filter_recording_schedule` — the Scheduled
  Recordings views (all / week / problems / paused) with a
  title+channel word match; Rust's "waiting" counts as Python's
  "scheduled", and the paused view keeps completed series rules
  whose pause is current or upcoming.
- `recording_library::classify_outcome` — the late-start rule: a
  recording that started more than 5 s after its scheduled start
  classifies "partial" (after the exit-code check, before the
  shortfall check), with a written-length fallback from the
  started/ended timestamps. The app supplies the planned window
  for DVR finishes and shows "The recording did not cover the
  full scheduled time." for a partial finish with a clean exit.
- `exit_guard::upcoming_recording_jobs` — the exit confirmation's
  upcoming list maps a paused time repeat to its first occurrence
  after the pause (dropped if that is still paused) and drops
  paused series rules and one-offs, deduplicated and sorted.
- `storage_monitor::check_background` — free-space probes run on
  their own threads per destination into a shared result map;
  cached measurements are reused until due plus one interval of
  grace, and a blocked probe cannot stall the others (the sync
  check and the background check share one apply tail). Test:
  a healthy drive still stops its recording while another
  probe is blocked.
- `recording::publish_new_file` — Windows rename; elsewhere a
  hard link with an exclusive-create fallback on
  EPERM/EOPNOTSUPP/ENOSYS. The excerpt publish and the buffer
  clip save both go through it.
- `recording_media::transcript` (timestamped text, UTF-8 BOM)
  and `save_excerpt` — ffmpeg re-encode of a chosen range to a
  new .mkv through a staging directory beside the target,
  cancel checked before the source is touched, a two-hour
  deadline, and Python's error strings.
- `recording_queue` — the opening state: a successful hand-off
  parks the head as the opening row and only the player's
  `started()` confirmation consumes it; `stop()` clears the
  opening; an edit restarts while an item is opening. Named
  saved queues (`saved_queues` / `save_named` /
  `saved_queues_json` / `replace_pending`) with Python's name
  and entry validation and its four error strings.
- `providers` — Stalker clients are `Sync` (token behind a
  Mutex) and published to the table only after their fetches,
  so loads never hold the table lock; two resolve lanes share
  one queue, a Play lookup superseded by a newer selection
  returns "The stream lookup was superseded by a newer
  selection." without a portal call, and catch-up lookups are
  never superseded.
- `background::LatestTask::with_parallelism` for the lanes.

App (iptv-app):

- Recordings > Recording Notifications... — the
  `recording_success_notification` config (dialog / speech /
  history, default dialog) routes the manual-start box, the
  scheduled-start report and the recording-saved report.
- Scheduled Recordings filter row — "Filter by title or
  channel" search (130 ms debounce) and the "Schedule view"
  choice, over a dialog-scoped `ScheduledUi` (query, view,
  shown ids) threaded through render and every selection-based
  action; Ctrl+F focuses the search with its text selected;
  the list takes the focus at open.
- Queue hooks — `PlayerShared` carries the recording queue;
  the VLC and fallback Playing polls confirm the opening row,
  advancing confirms first (Python's `ended`), and an engine
  Error on the opening item stops the queue at that title with
  a "Playback Error" box before any restart recovery, the row
  kept for a retry. The queue editor gains Save Queue... /
  Load Saved Queue... / Delete Saved Queue... with Python's
  confirmations and enablement.
- Player menu — Save Subtitle Transcript... and Save
  Recording Excerpt... beside the PR #44 items; the subtitle
  find results go multi-select with Go to Time... (exactly one
  selection jumps, anything else is announced) and Save
  Selected Subtitles... (the dialog stays open). The shared
  text export mirrors Python's `export_file`: extension
  append, replace confirmation, atomic write, outcome boxed
  under the export's title. The excerpt flow takes its points
  from the start, the named bookmarks strictly inside the
  media, and the end; the encode runs in the background and
  is canceled by a stop or a new load.
- Series scans are requested where Python requests them:
  after a guide import, after a pause period change, and when
  a duplicate job becomes a series rule.

i18n and help: the 42 msgids Python gained in #46 are appended
to all 13 catalogues with Python's post-#46 translations
(Hungarian verified 42/42 identical; `msgfmt --check` clean).
The help changes (transcript paragraph; queue / notifications /
excerpt paragraphs — three in Hungarian; the schedule-filter
paragraph; the replaced stop/finalize paragraph) are applied
passage-by-passage in all 14 languages and verified present
with the old text gone.

## Structural equivalences (no code change, by design)

- Series-scan request retention: Python's scan thread consumed
  requests, so #46 made a request raised during an active scan
  survive it. Rust has no scan thread — the 10 s DVR tick
  re-evaluates `series_last_scan` and a scan snapshots the
  rules when it runs — so a request cannot be consumed early.
  The request *sites* were the real gap and are ported above.
- Recording-start isolation: Python starts recordings on
  worker threads and #46 isolates each start's destination
  check so a stuck drive cannot block the connection-limit
  accounting of another. Rust starts recordings synchronously
  on the UI thread and the engine registers a start before it
  returns, so there is no cross-thread gate to isolate; the
  stuck-drive half of the fix is the background space probes.
- Exit drain: Python #46 waits for in-flight start threads at
  exit. Rust's PR #44 exit flow already drains active
  recordings through `begin_shutdown`, and there are no
  in-flight start threads to wait for.
- Python's `tools/release.py` and `docs/learnings` changes in
  the PR are Python-project tooling with no Rust counterpart.

## Gate

Recorded 2026-11-27 on the final tree: `cargo fmt --all --check`
clean; `cargo clippy --locked --workspace --all-targets -- -D warnings`
clean; `cargo test --locked --workspace` 565 passed / 0 failed
(iptv-core 520, iptv-app 25, iptv-vlc 9, the remaining suites 11);
playlist parity PASS against the pinned Python checkout
(a29ad9a3); `git diff --check` clean; `msgfmt --check` clean on
all 13 catalogues.
