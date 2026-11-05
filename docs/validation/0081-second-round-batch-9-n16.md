# Second-round batch 9 — N16: stream type probing

Date: 2026-11-04. Item: audit N16 (medium) — Python probes a
channel's streams to tell radio from TV, caches the result,
says "Checking stream type", and reports an audio-only
recording (`media_type.py`; `main.py:3719`, `:4128`,
`:4281`). Rust decided from playlist metadata only.

## What was built

Core (`crates/iptv-core/src/media_type.rs`, new):

- `parse_media_streams` / `classify_media_streams` /
  `classify_probe_report`: a hand-written port of Python's
  ffmpeg-report parsing (no regex crate in the workspace, as
  with the earlier scanners). A video track flagged
  `attached pic` (cover art on a radio stream) never counts
  as video — the exact rule that keeps radio with a static
  image classified as audio.
- `MediaTypeCache`: session cache keyed by stable channel
  identity, valid only while the entry's URL is unchanged;
  only classified results are stored, so an unanswered probe
  retries next time (Python's `MediaTypeCache` semantics).
- `formats_for_media`: audio-only streams offer the audio
  presets only; everything else keeps the full list.
- `channel_record_key`: Python's `_channel_record_key` over
  the `Channel` struct (provider type/id, stream id, URL,
  provider data, name).

App (`crates/iptv-app/src/main.rs`):

- `classify_channel_media`: metadata statement first
  (`channel_media_is_audio`), then the probe cache — the one
  classification every recording path now shares.
  `recording_format_for_channel` and
  `remap_format_for_media` take the cache, so manual,
  scheduled, shared and catch-up captures all classify the
  same way, including probe results.
- Probing (Python's `_schedule_media_probe` /
  `_probe_channel_media_now` / `_probe_channel_media_worker`
  / `_terminate_media_probe`): highlighting a channel arms a
  1.5 s debounce (token + deadline, fired by the
  housekeeping tick — arrowing through the list opens no
  streams). The probe runs one `ffmpeg -i` on a worker
  thread (20 s budget, headers honored, http(s) only) and
  stores only a classified answer. Guards, all Python's: a
  channel feeding a live buffer is skipped, nothing probes
  while the player has media, and a busy one-stream provider
  is never stolen from (`single_stream_busy`). The ffmpeg
  child handle is registered per identity; playback,
  manual/scheduled recording and catch-up download starts
  preempt every in-flight probe (`terminate_media_probes`),
  and a probe preempted before registering kills itself.
  Registered probes release the provider settle slot on
  completion (`note_media_released`), as Python's do.
- Recording Format submenu: refilled on menu open (frame
  `MENU_OPEN`; the refill is a no-op while the state it
  shows is unchanged) with the presets for the selected
  channel's classification and the checked item set to the
  channel's effective format; while a probe is classifying
  an unknown channel, a disabled "Checking stream type…"
  item (id 5225) says so. Choosing a format now saves to
  the audio or video preference by the SELECTED CHANNEL's
  classification (Python `_set_recording_format`), replacing
  the earlier save-by-format-kind approximation.
- Recording start report: a manual recording that starts
  now shows Python's box — "Recording started ({fmt}):\n
  {path}", or "Audio-only stream detected. Recording
  started ({fmt}):\n{path}" for an audio-only stream —
  with the resolved preset's label, alongside the existing
  status line.

## Residuals

- Python's probe worker re-checks the provider slot on the
  worker thread immediately before opening the stream; the
  Rust worker registers its child under the same identity
  discipline instead (a preempted probe kills itself at
  registration). The UI-thread guards run at probe start in
  both.
- No live one-connection provider was available to verify
  the preemption against a real refusal; the guards are
  covered by construction and the core tests below.

## Tests

- Five core tests in `media_type::tests`: TV/radio report
  classification incl. the attached-pic rule, stream-line
  parsing, cache URL scoping and classified-only storage,
  per-media format lists, record-key shape.
- Full gates green locally on 2026-11-04: `cargo fmt --all
  --check`, `cargo clippy --workspace --all-targets -- -D
  warnings`, `cargo test --workspace` (373 tests),
  playlist parity PASS against the pinned oracle, `git diff
  --check`.
