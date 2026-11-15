# 0101 — Python PR #36 ported: recording tools, playback navigation, data recovery (v0.4.27)

Brandon's order (2026-11-15, via the main chat): "put the pr's
on both" — Tibor's (htibcsike) Python PR #36, "Improve
recording tools, playback navigation and data recovery",
merged into the Python client as bf4e8c8e and shipped there
as v1.150.66, is ported whole to the Rust client. The port
target is the post-#36 Python state of every touched
subsystem, including the base pieces #36 builds on that the
Rust client never had (the recording library itself, episode
continuation state, the recording-limits module).

## What landed

Core (crates/iptv-core):

- `background.rs`: `LatestTask`, the latest-wins single
  worker with cancellation tokens — superseded or closed
  work is dropped at delivery time, as Python's does.
- `recording_library.rs`: the per-folder recording index
  (`.recording-library.json`) with corrupt-index preservation,
  scan and trash scan, checked paths that refuse active or
  playing files, trash/restore/permanent delete (hard-link
  first, exclusive copy with mtime restored), title rename,
  the 20-folder library list, `file_uri`, playback channel
  construction, resume resolution by exact file signature,
  row filter/sort queries, outcome classification, and
  `remember` — every finished recording is indexed on the
  single finish path in `poll_recordings`.
- `recording_limits.rs`: per-account connection limits,
  peak-demand computation over padded windows (with #36's
  fix: empty limits short-circuit before any key math),
  overlap warnings, and the conflict rows with account
  aliases (configured playlist names only, never URLs).
- `episode_continuation.rs`: adjacent/following episode by
  bookmark identity plus provider/playlist/series equality.
- `providers.rs`: Xtream catalogue calls now distinguish an
  authentication rejection (`auth` 0 in `user_info`) and
  other non-list responses from an empty catalogue, instead
  of silently returning no items.
- `bookmarks.rs`: `is_movie` now also matches
  `local-recording:` provider ids, so local recordings get
  bookmark keys (the Python legacy key carries an explicit
  movie kind; the Rust URL heuristic missed .ts recordings).
- `shortcuts.rs`: player defaults for Previous Episode
  (Ctrl+PageUp), Next Episode (Ctrl+PageDown) and Subtitle
  Timing (Ctrl+Shift+T), with #36's anti-steal pass — a saved
  binding that already uses one of these keys is kept, and
  the displaced action moves to a free Ctrl+Alt+Shift letter.
- `recording.rs` / `dvr.rs`: finished recordings carry the
  planned length and channel name into the library index;
  `validate_window` and `DvrScheduler::edit_timing` port
  Python's timing-edit rules and messages exactly.
- `iptv-vlc`: `libvlc_video_set_spu_delay` is bound;
  `set_subtitle_delay_us` drives Subtitle Timing.

App (crates/iptv-app):

- Recordings > Recording Library: the full dialog — search,
  folder scope (plus All Recording Folders), trash view,
  status filter, sort with Descending, Play / Resume, Rename
  Title, Move to Recording Trash / Delete Recording File,
  Restore, Add/Remove Folder persisted as
  `recording_library_folders`. Scans run on the LatestTask;
  file actions run on their own thread and are not cancelled
  by closing the window.
- Playback > Previous/Next Episode: the current media keeps
  playing during lookup; a stale token or a changed playback
  generation drops the reply; no adjacent episode and fetch
  failures are announced.
- Playback > Subtitle Timing: the delay (−600000..600000 ms)
  applies to the engine and to subtitle display/speech for
  the current media only; a load owes its restore at Playing;
  new media resets it.
- Recording Conflicts: scheduling a recording that overlaps
  now opens the conflicts dialog (details with channel,
  account alias and the real padded window; Schedule Anyway)
  instead of the old count-only prompt; Scheduled Recordings
  gains Edit Times... (the timing editor, with conflict
  re-check) and Recording Conflicts... for one selected job.
- Continue Watching: a Type choice switches between Video on
  Demand (`playback_bookmarks`) and Recordings
  (`recording_playback_bookmarks`). Recording rows resolve
  through the library by exact file signature. Remove and
  Clear All write through the ResumeHistoryWriter thread
  against a fresh on-disk config; only success applies in
  memory — failure shows "Could not save playback history
  changes." and leaves the saved positions untouched. While
  a write for the playing item's store is in flight, resume
  checkpointing pauses, as Python's pending guard requires.
- i18n: the 25 new PR #36 strings (plus the session's other
  new dialog strings) in all 13 catalogues, translations
  taken from the Python catalogues at bf4e8c8e. Help: the
  four PR #36 passages (episode navigation, Continue
  Watching Type, Subtitle Timing, Recording Library,
  Recording Conflicts) added to docs/help in all 14
  languages from the Python guides.

## Deliberate divergences and named gaps

- Conflicts analysis is synchronous: the Rust computation
  is pure and local over snapshots, so it is observably
  identical to Python's worker round-trip (Schedule Anyway
  enables only after a successful analysis). Python's
  worker-failure branch is unreachable here.
- The library dialog filters synchronously on delivery and
  on control changes instead of routing filtering through
  the LatestTask; filtering is instant, and enablement is
  gated on scan/action busyness exactly as in Python.
- "Schedule by Time..." manual job creation is NOT ported:
  the Rust client never had schedule-by-time at all (a
  pre-existing gap, not part of #36's delta). Only the Edit
  Times form #36's call sites need was ported.
- Natural-end episode autoplay (the countdown after an
  episode finishes) is NOT ported: it predates #36 in the
  Python base and Rust never had it. The minimal episode
  snapshot + token/busy state `navigate()` needs is here.
- `recording_limits` runtime budget enforcement at record
  start (`check_start`) is unchanged by #36 and not ported;
  the limits are used for scheduling warnings and conflicts.
- A repeated (ambiguous) local clock time in Edit Times
  resolves to its first occurrence; Python raises and
  offers a choice dialog.
- The library index is a BTreeMap (sorted keys) where
  Python's dict keeps insertion order; this only affects
  `remember`'s 10,000-entry trim order in a folder with
  more than 10,000 recordings.
- Python's `json.loads` accepts NaN and its cleaner then
  sanitizes it; serde_json rejects NaN, so an index
  containing NaN is treated as corrupt (preserved, rows
  default). Arguably safer; recorded here.
- A `waiting` DVR job (held for a connection) maps to
  "scheduled" in conflict/budget views, matching Python's
  scheduler state. Its Edit Times button enables from the
  view, but `edit_timing` re-reads the live job and refuses
  non-scheduled/recording statuses, so a waiting job's times
  cannot be edited from that path — a known minor wrinkle.
- Library Play uses the normal play path (Python forces the
  internal player); a user configured with a named external
  player gets that player instead.

## Gates

fmt clean; clippy `--workspace --all-targets -- -D warnings`
clean; playlist parity PASS against the pinned Python tree;
`git diff --check` clean. Tests: iptv-app 22 passed,
iptv-vlc 9 passed, iptv-core 374 passed with 2 failures —
`epg::tests::worker_mirrors_the_guide_to_the_store_on_its_own_thread`
and `epg::tests::worker_partial_failure_keeps_loaded_sources_and_reports`
fail identically on pristine Forgejo main 770e1ce6 in this
environment (verified in a clean worktree before the port
was applied), so they are pre-existing and not caused by
this change.

## Acceptance still Brandon's

NVDA pass over the Recording Library dialog, the conflicts
dialog and Edit Times, Subtitle Timing, episode navigation,
and the Continue Watching Type switch, in English and one
non-English language.
