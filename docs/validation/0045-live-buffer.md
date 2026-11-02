# 0045 — Local Live Buffer, Rewind, Return to Live (issue #96)

Date: 2026-11-02.

Python's local live buffer is ported: when enabled, live TV in
the built-in player is ingested once by ffmpeg (stream copy)
into a rolling window of 2-second MPEG-TS segments on a
loopback server, and the player plays a view into that window
instead of opening the provider stream directly. That makes
pause, Rewind Live TV and Return to Live possible on live
channels, and lets a recording read the same buffered view
instead of opening a second provider connection.

Core (`iptv-core::timeshift`, from the spec pass against
`client_timeshift.py` / `live_buffer.py`):

- BufferSettings mirrors Python's config rules: only a JSON
  true enables the buffer; seconds clamp 60..=1800 (default
  600), MiB clamp 64..=4096 (default 512); the configure
  dialogs work in minutes 0..=30 and write through
  apply_dialog_choice.
- BufferWindow mirrors DiskWindow: segment bookkeeping,
  eviction by seconds and bytes, full manifest validation on
  publish (timeline seeding, GOP latch), retained span,
  rewind range, expiry checks.
- LiveBuffer mirrors the session registry: leases
  (reference-counted; the last release closes the session),
  per-view cursors that land on the containing segment, the
  one-shot initial view (first playlist render trimmed to 3
  rows), view caps of 32 (16 protected), and the
  disable-while-a-recording-reads refusal.
- TimeshiftError carries Python's exact message strings.

Adapter (`iptv-app::buffer` plus the main-window wiring):

- BufferSession owns the spool directory, the ffmpeg ingest
  process and the loopback HTTP server. The ingest argv is
  Python's verbatim (reconnect flags for http(s), TCP for
  RTSP, the shared header input args from recording.rs, the
  segment muxer uploading playlist and segments back to the
  server). Python's two stderr codec lines set the window
  error "input codec cannot be transparently buffered".
- HLS sources are resolved before ingest with Python's rules
  (separate renditions refused, max-bandwidth variant pick
  with the optional Mbps cap, up to 4 master levels, the same
  error strings).
- The server accepts the muxer's uploads (validating each
  manifest through the core window before it becomes
  visible), renders view playlists byte-for-byte in Python's
  format, answers 410 for evicted segments, and renders the
  same trimmed initial view for HEAD without consuming the
  one-shot flag that the first GET consumes.
- Playback interception: a live built-in-player request with
  the buffer enabled starts (or reuses) the session for the
  channel's source key and plays the live-edge view; the
  failure path shows Python's "could not start" box and does
  not fall back to direct play. The 1-second player tick
  delivers finished starts, tracks the playback position
  against the engine clock, and speaks Python's priority-1
  poll messages (storage/input failure, expired position,
  live input ended). Pause/resume runs the paused-position
  expiry check.
- Player Playback menu gains Local Live Buffer..., Rewind
  Live TV... and Return to Live between Sleep Timer and
  Record, with Python's dialog texts, ranges and error
  strings; configuring the buffer relaunches the live channel
  being watched, as Python does. Return to Live falls back to
  relaunching the last buffered channel, then to Python's
  "No local live history is available." box.
- Recordings lease a protected view: starting a recording of
  a buffered channel reads the view URL with no headers
  instead of opening the provider URL; the lease is released
  when the recording stops or finishes, and disabling the
  buffer is refused while such a lease is held.
- Closing the main window closes every session; player Stop
  releases the current session's playback lease.

Recorded deviations from Python:

- The engage test's casting input is always false: the Rust
  cast model keeps no persistent connection state (casting is
  fire-and-forward through the cast worker), so there is no
  "casting now" flag to consult.
- Python's provider single-connection gating
  (single_stream_provider_key, has_active_connection,
  wait_provider_idle) is not implemented: the Rust provider
  layer has no equivalent connection tracking. The recording
  lease above removes the common second-connection case.
- The engage test's channel-kind input is always false: the
  Rust Channel carries no kind field, and VOD never takes the
  live path (is_vod_url), so the movie exclusion cannot
  trigger differently in practice.
- HLS resolution follows redirects through ureq itself; the
  base URL for relative variants is taken from the response's
  Location header when present, else the request URL, where
  Python reads the final response URL directly.
- After configuring, Python relaunches through its full
  launch path including cast checks; the Rust port relaunches
  through the built-in player path only.

Evidence class: automated — fmt clean, clippy zero warnings,
150 workspace tests pass (the timeshift core carries its own
suite mirroring Python's cases), playlist parity PASS.
Playing a buffered view through libVLC with NVDA remains
part of #86/#87; this VM has no libVLC.
