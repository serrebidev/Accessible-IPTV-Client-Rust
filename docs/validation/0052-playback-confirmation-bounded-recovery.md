# 0052 — Playback confirmation and bounded recovery (issue #128)

Farhan's reliability ticket, against the pinned Python internal
player (`internal_player.py` at 9aa89e05): `_max_reconnect_attempts
= 6`, `_first_play_deadline_seconds = 30.0` anchored at the channel
choice and never extended by retries, `_first_start_buffer_timeout
= 25.0` for the no-media restart, backoff 0.4/2/4/8/8/8 s, a
two-minute quiet window resetting the budget, a generation guard on
every delayed restart, and give-up surfacing "Stream lost" (status,
priority 1) plus the long dialog text.

## What was wrong

- The VLC poll never fed `Started` to the session: during libVLC
  playback the session sat in Opening forever, so "Playing" was
  never confirmed on the primary engine (the fallback's on_loaded
  did confirm). Playing is now reported only on engine
  confirmation in both paths; a stale Started after Stop is
  dropped (the session ignores events with no current request).
- Recovery was three reconnects, live-stall only; an engine Error
  failed immediately, and nothing ever gave up on a stream that
  never started (Opening could hang forever).
- Reconnects executed immediately, with no backoff and no
  stale-work guard; a pending buffer start could still deliver
  after Stop (its delivery checked only the channel URL, which
  Stop does not change).

## Changes

- Core `PlaybackSession` rebuilt on injected time: `play(request,
  now_ms)` anchors the deadline; `on_event(event, now_ms)` routes
  Stalled/Error/live-Ended through one bounded `recover` (six
  attempts, deadline checked first, budget reset only after
  120 s quiet); `on_tick(now_ms)` runs the 30 s deadline, the
  25 s no-media restart, and the 10 s prolonged-buffering
  restart (fed by `note_buffering`); give-up lands in
  `SessionState::Lost` — announcement "Stream lost", and
  `lost_report()` returns Python's dialog text with the attempt
  count and last error. `play`/`stop` bump a generation the
  adapter checks before firing any delayed work.
- Adapter: reconnects are scheduled (`pending_reconnect` with
  generation + due time) and fired by `drive_session` on the
  player tick through `engine_load` (the engine recorded at load
  time); a failed reload feeds back as an Error event. The VLC
  poll feeds Started on the Playing transition and buffering
  on/off; the fallback's stop event schedules instead of loading
  inline. Give-up settles through `settle_session`: engines stop,
  "Stream lost" at priority 1, long report in a Stream Lost
  dialog on the player window (Python's two surfaces).
- Buffer starts record the session generation; delivery after a
  Stop or a newer choice drops the outcome (releasing the lease)
  with no playback and no failure dialog. Stop also clears any
  pending buffer slot. The buffer-failure path already preserved
  its error without silently switching to direct playback —
  verified unchanged.
- One deliberate change from the slice-3 session test: a VOD
  stoppage mid-file now earns bounded recovery, matching the
  Python internal player (its error/stopped restarts do not check
  liveness); a VOD Ended still ends.

## Evidence

- Automated: fmt clean; clippy `-D warnings` clean; 160 workspace
  tests pass (six new session tests: confirmation-only Playing,
  shared deadline not extended by retries, no-media timeout then
  deadline, generation bumps on stop/supersede, Python backoff
  table, prolonged buffering + budget reset; the old session test
  rewritten to the six-attempt/Lost semantics); playlist parity
  PASS.
- Not covered here (kept distinct, per the ticket): controlled-
  media playback and owner-heard NVDA status checks in a live
  environment — those belong to the #137 evidence pack and the
  gate pack (#86/#87).
