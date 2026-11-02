# 0044 — Tray menu parity (issue #103)

Date: 2026-11-02.

The tray popup is now built fresh on every right-click,
mirroring Python's CreatePopupMenu instead of the old static
two-item menu:

- Restore.
- Player Controls submenu: Show Player, Play/Pause, Stop,
  separator, Cast / Connect... — all four items disabled
  when nothing is loaded in the built-in player (Python's
  _internal_player_has_media rule; ours reads the player's
  current channel).
- Stop Recording(s), present only while a recording is
  active; choosing it stops every active recording.
- Separator, then Exit.
- Left click (up) and double-click on the icon both restore
  the main window, as Python binds both for NVDA/JAWS tray
  navigation. Selections act through the same helpers as the
  menus (toggle_pause_state, menu_player_transport,
  show_cast_for), so tray and menu behavior cannot drift.

Evidence class: automated — fmt clean, clippy zero warnings,
workspace tests pass, playlist parity PASS. Hearing the tray
menu with NVDA remains part of #86.
