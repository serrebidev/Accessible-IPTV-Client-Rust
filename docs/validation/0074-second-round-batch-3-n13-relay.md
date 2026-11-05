# 0074 — Second-round batch 3: N13 recording relay and one-stream refusals

Date: 2026-11-04. Scope: the second half of audit finding N13
— sharing the playing channel's connection with a recording,
and refusing work when a one-stream provider's only connection
is busy. The probing half shipped in validation 0073. With
this batch, N13 is closed.

## What was wrong

Recording the channel the built-in player was showing opened
two provider connections — libVLC for the picture, ffmpeg for
the file — which a one-stream account (Python knows one:
teleelevidenie.com) refuses or punishes. Python stops the
player, records with an ffmpeg tee whose second MPEG-TS copy
feeds a loopback relay the player watches, and resumes direct
playback when the recording ends (`recording_relay.py`,
`_record_channel`, `_end_shared_playback`). It also refuses
manual recordings, scheduled recordings and catch-up downloads
up front when the provider's slot is held (`_single_stream_
provider_busy`), with the plain-words 403 explanation. Rust
had none of this: no provider key, no connection accounting in
the recording engine, no relay.

## What changed

- Core `catchup_direct::single_stream_provider_key` — Python's
  deliberately narrow suffix list (teleelevidenie.com only),
  modifiers stripped, host lowercased, trailing dot removed —
  and `ONE_STREAM_REFUSAL`, the plain-words text.
- The recording engine tracks a connection key per active
  recording (`set_connection_key`, `has_active_connection`)
  and gained `start_shared`, which hands the caller ffmpeg's
  stdout pipe. Registration happens in `start_recording`
  (manual and DVR), `start_shared_recording`, and
  `launch_staged_capture` (catch-up).
- New app module `relay`: the RecordingRelay port. A pump
  thread reads the pipe no matter what (a full pipe would
  stall the recording), keeps a 2 MB / 2 s backlog so a joining
  player starts at a keyframe, fans out to per-client queues
  capped at 256 chunks (a slow player drops chunks, never
  blocks), and serves `video/mp2t` over loopback HTTP at a
  token path (anything else gets 404). `close()` lets clients
  drain the tail, bounded at 2 s, then shuts sockets down.
- Buffer sessions record their ingest source URL, so the
  registry answers Python's `has_provider` / `owns_url`.
- Adapter flow (`toggle_recording`): if the player is showing
  this live channel and no buffer session feeds it, the press
  stops the player, a settle worker waits out the media
  settle, then `start_shared_recording` (the existing prepare
  path with `copy_to_stdout`) starts and the player is pointed
  at the relay URL with provider headers dropped. A second
  press while settling cancels the start and gives the channel
  back. When a shared recording finishes (`poll_recordings`),
  the relay closes and — only if the player is still on the
  relay URL — playback stops, settles, and resumes directly;
  anything the user started meanwhile is never displaced.
- Busy refusal: `single_stream_busy` (recorder connection,
  buffer provider, player current URL) gates manual recording
  (excluding the player when sharing), DVR starts (the job
  fails with the refusal text), and catch-up downloads (before
  any probing, since probes are media requests too).

## Verification

- New core tests for the provider key (suffix match,
  lookalike host, unrelated timeshift host, empty URL).
- Full gates green locally: `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`, playlist parity against the pinned
  Python oracle, `git diff --check`.

## Not verified / residual

- The relay is verified by construction against Python's
  source and by the gate suite; no live one-stream provider
  was available to this build machine, and the watch-while-
  recording flow needs Brandon's hands on a real channel.
- Python's busy check also counts a connected caster's source
  URL. Rust's cast flow retains no source URL in app state
  (the cast window holds the channel only while open), so
  that one arm is not ported; the recorder, buffer and player
  arms are.
- Python's resume resolves the channel's live URL through the
  provider layer; Rust resumes with the channel's own URL,
  matching a fresh play of an already-resolved channel.
- Python runs its pre-start audio-track probe inside the
  settle window; Rust's `prepare_recording` probes after the
  settle, inside `start_shared_recording`. The settle still
  precedes the recording's own open, which is the ordering
  the one-stream refusal turns on.
