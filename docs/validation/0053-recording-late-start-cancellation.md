# 0053 — Late-start and cancellation outcomes (issue #130)

Against the pinned Python reference (`dvr.py` at 9aa89e05) and the
ticket's criteria.

## Verified already true (no change)

- Buffered recordings start at the live edge: the recording input
  takes the buffer session's cursor at `behind = 0.0` (protected
  view), never from retained history; earlier-buffer recovery
  stays out of scope, as the ticket says.
- Late starts stop at the saved stop time including padding:
  `stop_ts` is programme stop + post-padding and the tick's Stop
  fires there (covered by the new late-start test at exactly the
  padded stop).
- Expired unstarted jobs are Missed: the tick marks a Scheduled
  job past its stop as Missed with Python's message, matching
  `dvr.py` (existing test). The Rust-only async-resolution expiry
  stays Failed, the faithful analog of Python's start-failure
  path (`_start_job_worker` marks Failed with the error) — the
  reference has no resolving state to expire from.

## Gaps closed

- **Missed-beginning warning.** Python has none; the ticket
  requires one. `DvrJob.started_late` (serde-defaulted, persisted
  with the schedule, reset for repeat occurrences) is set when a
  recording actually begins after the programme's own start —
  padded start + pre-padding — on all three start paths (tick
  start, `mark_recording`, `resolution_succeeded`). A successful
  finish then completes with "Started late; the beginning of the
  programme was missed." in the job message (appended to any
  finish message), which survives reload. A start inside the
  padding window is not late and finishes clean.
- **Cancel must stop the capture.** The scheduled-recordings
  dialog's Cancel Recording set the job Canceled but never stopped
  the recording engine: a canceled job leaves the Recording
  state, so the scheduler would never emit its Stop action and
  the capture ran on. The handler now stops `dvr:{id}` in the
  engine; the finish flows through the normal poll, releases the
  buffer lease, and `mark_finished` keeps the job Canceled with
  the user's reason (Python's rule, now under test). The delayed-
  resolution side was already guarded (`resolution_succeeded`
  refuses canceled jobs; resolve completions drop canceled jobs)
  and stays under its existing regression tests.

## Evidence

- Automated: fmt clean; clippy `-D warnings` clean; 163 workspace
  tests pass — three new DVR tests (late start completes with the
  persistent warning and stops at the padded stop; in-padding
  start is not late; finish after cancel keeps Canceled and the
  user's message) on top of the existing missed/cancel/resolution
  race suite; playlist parity PASS.
- Kept distinct for #137: controlled-media and real-provider
  recording checks, and accessible job-status checks with NVDA.
