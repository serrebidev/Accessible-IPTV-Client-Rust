# Validation 0025: GUI parity G6 — programme and library dialogs

Date: 2026-11-01. Issues: #60 and #50 (program #54).

## Landed

- File > What's on Now: all channels' current programmes, Play
  starts the channel.
- File > Account Info: account picker + Fetch Info; the provider
  report fills the dialog text (the poll also keeps the main
  status line behavior).
- View > Continue Watching: resume entries with watched/total
  times, Play (position applied by the player), Remove (writes
  back through Persist::save_resume).
- View > Recently Watched: the session playback history
  (PlayerState now records the last 50 played channels).
- View > Show Downloads: catch-up downloads tracked with live
  progress from the DownloadWorker (polled on the completion
  tick), plus cancel.
- Catch-up dialog per channel (from the Channel EPG window):
  in-window past programmes with Play and Download.
- Reminders dialog and Scheduled Recordings dialog (from the
  Channel EPG window): delete reminders, cancel DVR jobs.
- The Channel EPG window (G3) is the full EPG viewer now: per-
  programme reminder/schedule plus entry points to Reminders,
  Scheduled Recordings, and Catch-up — closing #50's viewer and
  reminder-creation scope (lead time is the context default in
  this pass).

## Automated evidence

- cargo fmt --check, clippy zero warnings, 113 workspace tests
  pass, playlist parity PASS, git diff --check clean.

## Owner-heard evidence still owed

NVDA pass over the new dialogs (list labels, button order,
download progress announcements). Compile-verified here.
