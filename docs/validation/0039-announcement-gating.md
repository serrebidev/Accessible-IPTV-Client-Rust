# 0039 — Announcement-level gating (issue #102)

Date: 2026-11-02.

Implemented from the line-referenced Python spec
(live_announce.py + internal_player.py call sites):

- Python's law: the player's status text always updates
  visually; speech fires only when the message is non-empty
  AND config announcement_level (0-3, default 2) >= the
  message priority. Main-window announcements are never gated;
  subtitle cue speech is never gated.
- The adapter now routes every player-origin announcement
  through player_status(status, persist, message, priority):
  visual surfaces via Status::set_silent, speech via the live
  region only when the level reaches the priority.
- Priority mapping, mirroring Python's call sites: subtitle
  load failure = 1 (errors); Stopped, audio-track selection,
  subtitle file loaded, subtitle speech on/off, the sleep
  fade notice = 2 (state changes / confirmations); session
  announcements from play start, pause/resume, the media
  loaded/stopped events and the libVLC state poll = 3
  (Python's timer state-machine default).
- Player volume keys now update the status text silently —
  Python's player never speaks volume changes (its
  _update_status_label call for volume carries no prefix).
  The main window's volume hotkeys keep announcing (main
  window speech is ungated in Python).
- The one inconsistency in Python (the same "Stream lost" /
  "Reconnecting..." strings at priority 1/2 from restart code
  but 3 from the timer handler) collapses here: the Rust
  session machine has a single announcement path, gated at 3.

Evidence class: automated — fmt clean, clippy zero warnings,
workspace tests pass, playlist parity PASS. Owner-heard NVDA
confirmation at each level remains #86.
