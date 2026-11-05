# 0088 — Second-round batch 16: player details (audit N23)

## Scope

- Audit finding N23 in `docs/feature-parity-audit.md`,
  against Python `internal_player.py` in the pinned oracle:
  `_update_status_label` (:2041), `_select_audio_track`
  (:2472), `_cycle_audio_track` (:2518), the recovery paths
  (:1203, :1546, :1567), the subtitle load (:2624, :2630),
  and `_on_mouse_wheel` (:2233).

## What changed

- Core `playback.rs`:
  - `player_status_line(prefix, buffer_seconds,
    bitrate_mbps, volume, audio_track)` builds Python's
    exact status line: `{prefix} {buffer:.1}s buffer`, plus
    ` | ~{mbps:.1} Mbps` when a nonzero bitrate is known,
    the always-present ` | Vol {vol}%`, and
    ` | Audio: {track}` when a track label is known.
  - `bitrate_hint_from_url` ports
    `_extract_bitrate_from_query`: the first query key
    containing `bitrate` or `bandwidth` wins, scaled by
    magnitude (bits/s above 10,000, kbit/s above 500, Mbps
    as-is).
- App (`main.rs`):
  - PlayerState carries the line's parts (remembered prefix,
    buffer seconds from the cache the current load actually
    carries, the URL bitrate hint, the current audio track
    label). A new play resets prefix and track label and
    re-reads the hint; `engine_load` records the load's
    cache in seconds for both engines.
  - `update_player_status` rebuilds the line, sets it
    silently, and — when a prefix was given — speaks the
    whole line while the announcement level allows the
    priority, exactly Python's speak-the-label rule. A
    prefix-free call only refreshes the line.
  - Track changes: `choose_audio_track` (the funnel for the
    Choice, the menu and the cycle key) now reports through
    the line with prefix "Audio: {name}" at priority 2 —
    replacing the Rust-only "Audio track preference:"
    wording — and an engine refusal reports "Audio track
    unavailable" at priority 1 without saving or relabeling,
    like Python's failed set. The cycle key's empty and
    single-track cases name themselves on the line at the
    default priority 3. When the preference machinery
    applies a track on publication, the line's Audio segment
    follows it.
  - Recovery: scheduling a reconnect announces
    "Reconnecting..." at priority 2 (the rollover path keeps
    its own "Refreshing stream…" at priority 3). Giving up
    puts "Stream lost" on the line at priority 1 and shows
    the core's `lost_report()` — already Python's wording,
    retry count and Last error included — in a Stream Lost
    box with OK | Warning through the N18 notification
    queue, instead of a direct modal with OK only.
  - Subtitle file load: a successful VLC attach announces
    "Subtitle file loaded: {name}" at priority 2; a file
    whose suffix is outside Python's speakable set
    (.srt/.vtt/.ass/.ssa) with subtitle speech on then
    announces "Subtitle speech needs a supported text
    subtitle file." at priority 1. The attach failure
    message ("Could not load subtitle file.") already
    matched Python.
  - Volume: the key-driven change now refreshes the line
    silently (Python speaks nothing for volume), replacing
    the Rust-only "Volume {value} percent." text. The mouse
    wheel is bound on the video surface and the controls
    panel: one notch is 2%, clamped 0–100, same silent line
    refresh.

## Evidence

- Core tests (2 new): the bitrate hint's scaling, skipping
  and first-match rules; the status line's format with and
  without prefix, bitrate and track segments.
- Gates: `cargo fmt --all` clean, `cargo clippy --workspace
  --all-targets -- -D warnings` clean, `cargo test
  --workspace` (380 tests), playlist parity check PASS,
  `git diff --check` clean.

## Residuals

- The Mbps segment appears when the stream URL carries a
  bitrate/bandwidth hint — Python's query-hint path. Python
  also seeds it from the resolved HLS variant's bandwidth;
  the Rust variant resolver returns only the URL, so that
  source is not surfaced (noted, not wired through the
  resolver for this low-severity item).
- Python's segment-rollover refresh cap has its own shorter
  "Stream disconnected." box; in Rust an exhausted rollover
  falls into the session's ordinary recovery and ends in the
  same lost report as any other give-up.
- The wheel's per-notch step follows Python's handler
  (2% per event); high-resolution wheels send more events,
  as they do in Python.
- Brandon's NVDA pass on the spoken line and the wheel
  remains his, as with the other player work.
