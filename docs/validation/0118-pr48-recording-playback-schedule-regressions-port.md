# 0118 — Python PR #48 port (recording, playback, schedule regressions)

Spec: Python PR #48 by htibcsike, "Fix recording, playback, and schedule
regressions" (merge c200cb30, head fe098a13, base acf27f5e), shipped as
Python v1.152.6. Per Brandon's parity rule (2026-11-27), each fix was
audited against the Rust client first; only the missing behaviour was
ported. Data compatibility both ways is unchanged: the only new
persisted state is none — the DVR failure state and the EPG
incomplete-attempt marker are session-only, exactly like Python's.

## Ported

- DVR save resilience: a failed schedule write leaves the in-memory
  schedule intact, warns once per outage ("The recording schedule could
  not be saved. Check available disk space and write permissions.",
  caption "Scheduled Recordings", shown after the DVR tick), and is
  retried by the tick — a quick first retry, then Python's 60-second
  recovery cadence — until it lands (`DvrSchedule::save_logged` now
  records the outage; `take_save_warning` drains it once).
- Backup validation: `settings_backup`'s stored-job check now also
  validates `pause_dates` with the loader's rule, matching Python's
  switch to the full `valid_stored_job`. A bundle whose job carries an
  invalid pause period is refused; a valid one passes.
- EPG completeness: core load results carry (loaded, total) source
  counts through `EpgWorker::try_recv`. The app writes the freshness
  stamp and source hash only when every source loaded; a partial load
  still feeds the guide, the reminder/recording guide-change reviews
  and the dependent scans. A partial or failed attempt records the
  source-set hash with a 300-second retry time, and the automatic
  importer defers the same sources until then. A hand-run import now
  reports three ways: complete, "Partial EPG Import" with the counts,
  or failed.
- Go to Channel Number: all channels carrying the entered number are
  collected; more than one opens a chooser ("Choose a channel to
  play:") with group-disambiguated labels and numeric prefixes when
  labels still collide; cancel selects nothing. Rust keeps its
  recorded divergence (validation 0090): the chosen channel is
  selected in the list rather than played.
- Recording Library: scans are per folder end to end — one worker per
  folder, each publishing its rows as it lands; the merged list
  dedupes by canonical path; warnings accumulate per folder; the
  controls stay live while scanning and the status line appends
  "Loading recordings..." to the counts; a filter re-render restores
  the focused row by path as well as the selection; starting a file
  operation cancels every scan worker and bumps the scan version.
- Catch-up downloads: a programme whose download is already being
  prepared (a resolve in flight for it, or a staged probe) refuses a
  duplicate request at request time and again at begin; the begin
  check also consults the staged probes. Settings import now treats
  download preparation as pending downloads.
- Media probes: termination drains the probe state and retires the
  children (kill + wait) on a detached thread, noting each release as
  the child actually exits — a stuck probe can no longer block the
  interface thread. No new classification probe starts while a
  playback resolve is outstanding.
- i18n: the 3 new strings harvested into all 13 catalogues from the
  Python post-#48 catalogues (all translated, placeholders verified,
  msgfmt clean). PR #48 has no help changes.

## Already correct in Rust (no change)

- `recording_library::resolve_resume` already skips a directory whose
  scan errors and continues with the others.
- Catch-up playback already resolves through the shared async play
  path with the play-generation guard (PR #44); its failure box keeps
  the Play path's wording rather than Python's Catch-up templates.
- Download completion ordering: the engine starts synchronously and
  finishes are polled, so an early completion cannot be delivered
  before its row exists.
- Manual recording admission is synchronous on the UI thread, and the
  shared-player path already implements the second-press cancel
  (N13); there are no per-key pending-start lanes to wedge.
- Per-source EPG failures were already tolerated with a note and no
  per-source modal dialog exists in Rust.
- DVR saves stay synchronous: Python's off-thread writer addresses
  its own thread model; the Rust schedule file is local, and the
  ported failure handling (warning + retry) covers the user-visible
  regression.

## No Rust counterpart

- Python's `LiveBufferRegistry` locking (the registry lock held
  during disk work): Rust has no multi-session buffer registry —
  timeshift is a single app-owned session with plain ownership
  checks.
- The pending-start cancel events and playback-probe cancellation
  plumbing in `recorder.py`/`main.py`: Rust recording starts are
  synchronous; the exit flow's `closing_application` guard already
  drops late deliveries, and exit cancels pending work by draining
  (PR #44).
- `docs/learnings` additions: documentation only.

## Gate

Recorded 2026-11-28 before shipping v0.4.43: `cargo fmt --all --
--check` clean; `cargo clippy --locked --workspace --all-targets --
-D warnings` clean; `cargo test --locked --workspace` 566 passed /
0 failed (iptv-core 521, iptv-app 25, iptv-vlc 9, remaining suites
11); playlist parity PASS against the pinned Python checkout
(a29ad9a3); `git diff --check` clean; `msgfmt --check` clean on all
13 catalogues. New regression coverage: the DVR save-failure test
(warns once per outage, retry armed, in-memory state intact), the
backup pause-period cases, and the EPG worker test now asserting
the (loaded, total) counts on a partial load.
