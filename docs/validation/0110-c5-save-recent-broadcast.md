# 0110 — C5: Save Recent Broadcast (round 3)

Port target: Python v1.151.6 (`a29ad9a3`) `buffer_clip.py`
(`select_segments`, `_stage`, `save`) and `_save_recent_broadcast`
in `client_timeshift.py`, menu item "Save Recent Broadcast..."
in `internal_player.py` (Playback menu).
Plan: `docs/superpowers/specs/2026-11-23-python-parity-round3-design.md` (C5).

## What landed

- Core (`timeshift.rs`): `ClipError` (Python's exact sentences),
  `select_clip_segments` (complete segments only, `end > floor`,
  24-hour cap), `clip_manifest` (Python's staged playlist bytes),
  `clip_ffmpeg_args` (the stream-copy remux command),
  `clip_space_sufficient` (reserve on both volumes, doubled
  staged copy on one device), `MAX_CLIP_SECONDS`,
  `CLIP_COPY_CHUNK`.
- App (`buffer.rs`): `BufferSession::dir` and `snapshot` accessors.
- App (`main.rs`): the Playback menu item "Save Recent &Broadcast..."
  after Return to Live (Python's order), the history prompt
  ("Available history: {seconds} seconds.", title
  "Duration (seconds)", default 300 clamped to the span), the
  save dialog (recordings folder, sanitized channel name with a
  `-%Y%m%d-%H%M%S` stamp, `.ts` appended), the exists refusal
  ("A file with this name already exists in the selected folder."),
  the buffer lease with Python's error box, the reserve from
  `recording_min_free_mib` raised to the stop threshold when the
  space monitor auto-stops, the cancellable progress dialog
  ("Saving the broadcast excerpt...", Cancel focused), the worker
  (hard link then copy staging, ffmpeg remux, fsync, exclusive
  publish, "Saved to {path}." / error box on completion), and
  raising the running export's dialog on a second invocation.
  Help topic `built-in-player` carries Python's paragraph
  verbatim in English help.
- Strings: every user-facing msgid already had catalogue entries
  in all shipped languages, so no new translations were needed.

## Evidence

- Core tests beside the code: `timeshift.rs`
  (`clip_selection_matches_python_buffer_clip`,
  `clip_manifest_matches_python_stage_output`,
  `clip_space_and_ffmpeg_args_match_python`).
- `cargo test -p iptv-core` (441 lib tests) green, including a
  rerun after `rustfmt`; `cargo check -p iptv-app` green.
- `cargo clippy -p iptv-core -p iptv-app --all-targets -- -D warnings`
  fails on pre-existing dirty-tree warnings outside this group
  (`recording_queue.rs`, `recording_library.rs`, and main.rs
  lines well outside this group's edits — HEAD alone is
  clippy-clean by a stash check); this group's files report no
  findings.

## Brandon's NVDA check

1. Enable the local live buffer, play a live channel for a few
   seconds, then Playback > Save Recent Broadcast: the duration
   field offers the available history, defaulting to 300.
2. Choose 10 seconds and the offered name; the progress box
   opens with Cancel focused. Cancel leaves no file. Run it
   again: "Saved to {path}." and a playable `.ts` begins at a
   segment boundary.
3. Pick an existing file name: the provider box refuses with
   "A file with this name already exists in the selected folder."
4. With no buffer (external player or buffer off), the item says
   "No buffered audio or video is available."

## Residuals

- The progress box is modal, so the main window cannot be used
  during the export; Python's is modeless with its own window
  dialog. Cancel is focused in both.
- Non-English help sections for this paragraph are not yet
  translated (English only, as in C8).
- The `recording_power` backend test is flaky under parallel
  test runs (fails in a full-suite run, passes alone); it does
  not involve this group's code.
