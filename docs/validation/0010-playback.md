# Issue #22 (parity slice 3): playback

Status: **automated gates PASS**. Implemented 2026-11-01 on branch
`agent/playback`. Python remains the shipping product and the behavior
oracle.

## Implemented behavior

- `iptv-core::playback` is the wx-free seam. `StreamHeaders` ports
  `http_headers.py`: attribute headers (`http-user-agent`,
  `http-referrer`, origin, cookie, authorization) form the base, and the
  URL's pipe modifiers (`url|User-Agent=...|Referer=...`) are split off
  and fill gaps with the Python alias canonicalization — the pipe tail
  never reaches an engine.
- External players port `external_player.py`: argv shapes (VLC
  one-instance replace flags, MPV IPC/idle flags, MPC and others bare
  URL), the well-known executable locations for the headline players,
  custom-path player detection, and the `LaunchGate` (0.75 s same-stream
  debounce, generation supersede).
- `PlaybackSession` owns state and policy: play/pause/stop, engine
  events, live-stall reconnect up to 3 attempts (VOD never reconnects),
  volume clamped 0–100, and announcement text. The buffer law is
  represented by `start_cache`: 500 ms initial cache growing to the
  saved buffer from the slice-1 config settings. Playback rate is never
  touched, per agents.md.
- Resume ports `playback_resume.py` checkpoint rules: positions under
  30 s are not kept, positions inside the end window (30 s or the last
  5%) remove the entry, and the store caps at the newest 200 entries.
  Entries persist in the config under `resume_positions`, keyed by the
  stable favorite channel key.
- Adapter: Enter on the Channels list plays the selected channel.
  External choices spawn the resolved executable. The built-in choice
  opens a player window (wx `MediaCtrl` behind the `media-ctrl` feature)
  with named Pause or resume / Stop controls; media events feed the
  session (loaded → started + resume seek; stop near end → ended; stop
  mid-stream → stall/reconnect), and a 1 s timer checkpoints resume for
  seekable content.

## Evidence

- 61 workspace tests pass (7 new playback tests: modifier splitting and
  canonicalization, attribute-over-pipe precedence, argv shapes, launch
  debounce, session reconnect policy, resume rules, start cache).
- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets`
  (zero warnings), `tools/check-playlist-parity.py` PASS against pinned
  Python 9aa89e0, `git diff --check`.

## Evidence boundary

- The built-in engine is wx `MediaCtrl`, not libVLC: it cannot apply the
  saved network cache, so the buffer law is honored at the seam and by
  external players, while libVLC remains the recorded parity endgame
  (the same stance BlindRSS-Rust took). Linux builds need the
  `gstreamer-player-1.0` library at link time; `crates/iptv-app/build.rs`
  links it because wxdragon-sys 0.9.24 probes the wrong pkg-config
  module (`gstreamer-video-1.0`) — an upstream gap, documented in place.
- MPV IPC reuse, the subtitle session, and the long tail of niche
  external players from the Python tables are not ported; unknown named
  players fall back to a same-named command on POSIX, as the reference
  reports them on other platforms.
- No owner-heard NVDA pass is claimed for the player window; playback
  starts, announcements, and the resume flow are Brandon's ears to judge.
