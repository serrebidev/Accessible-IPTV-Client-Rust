# 0076 — Second-round batch 5: N12 recording logs, outcome reports, problem times

Date: 2026-11-04. Scope: audit finding N12 — when a recording
ended, Python could say exactly what ffmpeg did; Rust threw
ffmpeg's output away and reported a bare success/failure in
the status bar.

## What was wrong

Rust's recording engine spawned ffmpeg with stderr discarded
(`Stdio::null()`), never passed `log_datetime`, and kept no
record of a capture beyond its path and whether the exit was
clean. Python runs every capture with
`-loglevel level+datetime+info` (probed once per ffmpeg: the
flag arrived in ffmpeg 7 and an older binary refuses to start
with it), stderr into `<recordings>/logs/<name>.log`, and at
exit reads the problems back out, appends a grouped summary
to the log, and reports the outcome in one of four wordings
(saved / nothing recorded / stopped warning / error with the
ffmpeg detail and the log's path). A Recordings menu item,
Recording Problem Times, places each kind of problem at its
times in the newest log for a listener checking a recording.

## What changed

- New core module `recording_log`: the log-line parser
  (optional datetime stamp, component prefixes, level), the
  problem summary (problems grouped by digit-folded message,
  split into opening vs recording phases at the `Output #`
  line, folded `Last message repeated` counts, per-occurrence
  offsets), problem counts, the tail problem reader, the
  `time=` progress reader, the closing summary formatter,
  `recording_problem_times` (clock times plus media-clock
  jump times), and the cached `ffmpeg_supports_log_datetime`
  probe. Clock formatting rounds half-to-even, like Python's
  `round()`, so a listed time reads the same in both clients.
- Engine: `start`/`start_shared` take a `RecordingStartMeta`
  (title, format, planned seconds). stderr is redirected into
  the per-recording log, whose header records the date and the
  full command line unmasked, as Python's does; when the log
  cannot be opened, stderr is piped and drained for problem
  lines instead. Stops record `stopped_by_user` (and `killed`
  when escalation had to kill). Every finish — natural,
  stopped, or escalated — builds the full report: exit code,
  problem tail, problem counts, written seconds from the
  log's `time=` lines, and the summary appended to the log.
- App: recordings and downloads pass `log_datetime` from the
  probe; DVR jobs pass their span as the planned length,
  downloads their padded window. Finish reporting is Python's:
  the DVR job message carries the failure detail, and every
  non-download finish shows its outcome dialog (saved /
  nothing recorded / stopped warning with detail and file /
  error with detail, including the provider-MKA wording). The
  downloads window's truncation check now reads the written
  seconds from the log like Python, falling back to probing
  the file only when the log carried no stats.
- Recordings menu gains Recording Problem Times... in
  Python's position: the newest log's problems as
  "times: message" lines, the VLC Ctrl+T hint, Copy Details
  and Close.

## Verification

- Core: parser/summary/counts/problem-times unit tests over a
  synthetic timestamped log, plus an engine test running a
  script that speaks ffmpeg's log format — the finish report
  carries exit code 3, counts (1 warning, 1 error), written
  seconds from the stats line, and the log on disk holds the
  header and the appended summary with the planned length.
- Full gates green locally: `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`, playlist parity against the
  pinned Python oracle, `git diff --check`.
- The dialogs themselves are shell wiring, verified by
  construction against client_recording.py and the Python
  dialog; hearing one fire at a real recording's end is
  Brandon's hands-on pass.

## Residuals

- DVR recordings finishing while the app exits keep the
  existing exit-path handling ("Stopped because the app
  exited."); Python's notification suppression flag has no
  separate Rust counterpart because finishes are only polled
  while the app runs.
- The modal-queue behavior for these reports (Python queues
  a finish dialog behind an open modal) is N18, next round.
