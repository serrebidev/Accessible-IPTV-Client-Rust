# 0111 — C6: Read Embedded Subtitle Track and Recording Chapters (round 3)

Port target: Python v1.151.x `recording_media.py`
(`chapters`, `subtitle_tracks`, `embedded_cues`), the Playback
menu item "Read Embedded Subtitle Track..." in
`internal_player.py` (`_read_embedded_track`), and
`media_navigation_ui.py`'s "Recording Chapters..." dialog
(`_recording_chapters`). Plan:
`docs/superpowers/specs/2026-11-23-python-parity-round3-design.md`
(C6).

## What landed

- Core (`recording_media.rs`): `EmbeddedTrack` (index,
  language, codec, title, readable), `parse_listing` (the
  `Stream #0:N` summary parse, `_TEXT` readability set, the
  `Input #0,` guard, 240-character titles, dict-position
  repeats), `subtitle_tracks` and `embedded_cues` (the
  ffmpeg listing and `pipe:1` extraction, 1 MiB listing /
  20 MiB cue caps, 20 s / 180 s deadlines, cancellation
  killing the child, `local-recording:{size}:{mtime_ns}`
  library-id check, signature re-checks, ASS copied /
  other tracks converted to SRT), `clock_text` (`HH:MM:SS`
  from `media_navigation.py`), and the chapter display name
  (blank names fall to the number). Every error sentence is
  Python's verbatim.
- VLC (`iptv-vlc`): `chapters()` (full chapter descriptions
  for the current title, copied before the native array is
  released, times clamped at zero), `set_chapter()`,
  `chapter()`, `is_seekable()`, plus the
  `libvlc_chapter_description_t` layout test.
- App (`main.rs`): the Playback menu item "Read Embedded
  Subtitle Track..." after Load Subtitle File (Python's
  order), the listing worker, the choice dialog
  ("Choose a subtitle track for speech:"), the extraction
  worker, and cue delivery as embedded speech ("The
  selected subtitle track is ready for speech." honouring
  the same speech/review machinery). The carried `Track
  {id} — language — title — codec — Unavailable for
  speech` label marks image tracks unusable for speech.
  "Recording Chapters..." follows Subtitle Timing:
  "Choose a chapter:", labels with `clock_text`, Enter
  jumps through the engine, non-seekable streams say so.
  Both dialogs carry Python's titles and messages verbatim
  and Python's help topics (`subtitle-speech`,
  `recordings`).
- Strings: every msgid already had catalogue entries, so no
  new translations were needed.
- Help (`docs/help/en.md`): the embedded-subtitle paragraph
  and the chapters sentence, verbatim from Python.

## Evidence

- Core tests beside the code: `recording_media.rs`
  (listing parse incl. unreadable image codec, titles not
  crossing streams, `Input #0,` guard, no-subtitles empty,
  `clock_text`, `local-recording:{size}:{mtime_ns}` id,
  `.mkv`/`.mka` and cancellation guards, chapter name
  fallback).
- `cargo test -p iptv-core` green (451 lib tests);
  `cargo test -p iptv-app` green (24 tests) including
  `embedded_track_labels_match_python_shape`.
- `cargo clippy -p iptv-core -p iptv-app --all-targets --
  -D warnings` still fails on pre-existing dirty-tree
  warnings outside this group (`recording_queue.rs`,
  `recording_library.rs`, and main.rs queue-editor
  lines); this group's files report no findings.

## Brandon's NVDA check

1. Play a local `.mkv` recording with two embedded tracks
   (one text, one image), then Playback > Read Embedded
   Subtitle Track: the choice names both tracks with
   language and codec; the image track says "Unavailable
   for speech". Choosing it says "This subtitle track
   cannot be read aloud."
2. Choose the text track: "The selected subtitle track is
   ready for speech." and the cues read in time. Choosing
   another track or stopping ends the reading.
3. On a chaptered recording, Playback > Recording
   Chapters: names and `HH:MM:SS` starts; Enter jumps. On
   a recording with none: "This recording has no chapters."
4. On a live stream: "Open a local recording to browse its
   chapters."

## Residuals

- The ffmpeg runner uses bare `ffmpeg`, as the probe code
  does; Python's bundled-ffmpeg resolution is not wired.
- The choice dialogs do not carry F1 help topics: the
  player dialog builders here have no guide wiring (the
  Subtitle Timing dialog has the same gap).
- Non-English help sections for these paragraphs are not
  yet translated (English only, as in C5/C8).
- The `recording_power` backend test is flaky under
  parallel test runs (existing; untouched here).
