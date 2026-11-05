# 0079 — Second-round batch 8: N15 Download window detail

Date: 2026-11-04. Scope: audit finding N15 — Python's
download dialog shows live progress figures, retries a
transient failure with a countdown, reports a kept partial
file as "ended early", asks before cancelling, and hides on
Escape while the download continues. Rust's Downloads window
showed rows only, cancelled instantly, and never retried.

## What was wrong

Python's `CatchupDownloadDialog` ticks once a second and
shows progress percent, elapsed time, time remaining and
size so far, all derived from the recording log's ffmpeg
progress. A transient failure (a retryable provider answer)
is retried up to three times with a doubling delay
(2 s, 4 s, 8 s), re-running the whole direct-file probe
flow each time; a download that arrives short but is not
worth retrying keeps its partial file and reports "Download
ended early" with the amount that arrived. Cancel asks
"Do you really want to cancel this download? It cannot be
resumed afterwards." and Escape only hides the window.
Rust had none of this: rows froze mid-word, failures were
final, and cancel was immediate and silent.

## What changed

- Core `recording_log::format_duration` ports Python's
  `format_duration` (banker's rounding included), and
  `first_error_line` ports the diagnostic-line pick used by
  the failure detail block.
- Core `catchup::catchup_failure_is_retryable` ports
  Python's rule: a user-stopped or cleanly finished capture
  never retries; otherwise the ffmpeg text decides — an
  HTTP 403/429/5xx status with a real word boundary
  ("error 4030" does not count), or the connection-reset /
  refused / closed / temporarily-unavailable / no-route /
  server-returned / timeout wordings. The retry budget is
  `CATCHUP_RETRY_MAX_ATTEMPTS` (3) with
  `CATCHUP_RETRY_BASE_DELAY_SECONDS` (2.0).
- App: a staged catch-up download now carries its attempt
  count, log path, expected seconds, start time and any
  frozen failure detail. `finish_staged_download` confirms
  a completed download with Python's "Download complete"
  box; schedules a retry for a retryable failure (partial
  file deleted, row shows Python's "Download failed -
  retrying (attempt N of 3) in S seconds..." line, failure
  detail frozen into the Downloads window's detail field);
  keeps the partial file and shows Python's "Download ended
  early" warning when a truncated download's budget is
  spent; and keeps the N14 failure box otherwise.
  `process_download_retries` runs on the 1-second
  housekeeping tick and re-stages the probe flow
  (`stage_direct_probe`, shared with the first attempt)
  when a retry delay elapses.
- The Downloads window's detail field now shows, for the
  selected capture, either the frozen failure detail or
  Python's four live lines: Progress percent (from the
  log's media position against the expected window),
  Elapsed, Time remaining and Downloaded size, refreshed
  every housekeeping tick.
- Cancel asks Python's confirmation first; cancelling a
  download that is waiting out a retry delay drops the
  staged entry and closes its row immediately. Escape on
  the Downloads window or its list hides the window while
  downloads continue (View > Show Downloads brings it
  back), matching the Close button's existing hide.

## Verification

- Core tests cover `format_duration` (including the
  half-to-even cases), `first_error_line` (skipping the
  log's own header), and the retryable table (word-boundary
  statuses, the connection wordings, stopped/clean
  exclusions).
- Full gates green locally: `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace` (364 tests), playlist
  parity against the pinned Python oracle,
  `git diff --check`.
- The window wiring is shell code, verified by construction
  against main.py's dialog; Brandon's hands-on pass on a
  real download is the live check, as with N13/N14.

## Residuals and deliberate divergences

- Rust keeps its one multi-row Downloads window (audit R06
  design); the figures live in that window's detail field
  rather than in a per-download dialog.
- The retry countdown is stated in the row text when the
  retry is scheduled; Python also re-renders the remaining
  seconds every tick inside its dialog. The relaunch itself
  is driven by the same 1-second housekeeping tick Python's
  dialog timer uses.
- The boxes show immediately, like the N12/N14 reports;
  queueing behind an open modal is N18.
