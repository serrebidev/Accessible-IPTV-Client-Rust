# 0108 — C7: Recording Playback Queue and play from Problem Times (round 3)

Port target: Python v1.151.6 (`a29ad9a3`) `recording_queue.py`
and `recording_library_ui.py` (queue buttons, problem-times
play), plus `main.py` `RecordingProblemTimesDialog` in seek
mode and `_choose_recording_position`. Plan:
`docs/superpowers/specs/2026-11-23-python-parity-round3-design.md` (C7).

## What landed

- Core (`recording_queue.rs`, new): the session-only queue —
  add deduped by normalised path, edit/move/remove with
  head-change restart, start/stop, begin/finish hand-off with
  ticket, the end-of-item ticket+generation gate, and Python's
  three sentences verbatim (`{position} of {count}: {title}`,
  `Recordings in the playback queue: {count}.`,
  `Playback queue stopped at {title}. {reason}`).
- Core (`recording_log.rs`): `ProblemTimes.positions`
  (media time where known, clock fallback marked estimated,
  deduped) and `seek_rows`/`clock_to_ms` for the seek dialog,
  with Python's `"{time} (estimated) – {message}"` labels.
- Core (`recording_library.rs`): `problem_log` — Python's
  footer-checked log lookup (summary scan backwards in
  blocks, `# File:` identity, exact `# Media signature:`
  when present, legacy started/ended window otherwise, symlink
  and missing-log refusals with Python's message).
- App (`main.rs`): the queue engine (`QueueInner` in
  `Wiring`, hand-off validation on its own thread delivered
  on the 50 ms main timer, delivery plays from the start with
  no resume offer, failures shown as Python's
  `Recording Playback Queue` information box), end-of-item
  advance on the `Ended` session state (a manual stop leaves
  Idle, so it never advances), the `Recording Playback Queue`
  editor (Move Up/Down, Ctrl+Up/Down, Delete, Play Queue, Stop
  after Current Recording, help topic `recordings`), the
  library's `Add to Playback Queue`, `Recording Playback
  Queue...` and seek-mode `Recording Problem Times...`
  buttons with Python's enablement, and `play_channel_at` for
  explicit starts.
- English help (`docs/help/en.md`, Recordings): the queue,
  editor keys and play-from-problem-time sentences. Dialog
  and message strings go through `tr`/`trf` and fall back to
  English; per the translator-sync design in validation 0091
  the help sentence joins English only.

## Evidence

- Core tests beside the code: `recording_queue.rs` (add
  dedupe, edit/clamp/restart, start/stop, finish/stale-ticket,
  advance gate, verbatim wording), `recording_log.rs` (seek
  row labels and `positions`), `recording_library.rs`
  (`problem_log` footer, signature and refusal cases).
- `cargo test -p iptv-core` and
  `cargo clippy -p iptv-core -p iptv-app --all-targets -- -D warnings`
  green on Windows (exact commands in the task return).

## Brandon's NVDA check

1. Recording Library: select two recordings, Add to Playback
   Queue — "Recordings in the playback queue: 2." announced;
   Recording Playback Queue shows "1 of 2" / "2 of 2".
2. Ctrl+Up/Ctrl+Down reorder, Delete removes; Play Queue
   starts the first; when it finishes the next starts by
   itself; Stop after Current Recording stops at the end.
3. Add a recording, delete its file, Play Queue — "Playback
   queue stopped at {title}." with the reason.
4. Select a recording with timed problems, Recording Problem
   Times, Enter on a row — playback starts at that time.

## Residuals

- Queue validation re-checks at hand-off on a thread, as
  Python does, but the seek-mode log read runs
  synchronously (one tail read, no scan).
- A queue hand-off landing while the library is open plays
  behind it; the library hides only on Play Queue, as Python's
  `open_queue` closes it.
- Python's worker validation is latest-wins per `LatestTask`;
  here a replaced head starts a fresh validation, which is
  the same observable outcome for the editor's restart rule.
