# Validation 0021: GUI parity G2 — the full menu system

Date: 2026-11-01. Issue: #56 (program #54). Spec:
docs/gui-parity-audit.md; Python client_ui.py menu construction.

## Landed

- Menu bar is now File / Player / View / Options / Help with the
  Python items, order, radio/check kinds, and shortcut text taken
  from the effective shortcuts table.
- Working items: Exit; Import EPG to DB (file pick, remembered in
  epg_urls, loaded immediately); What Is Playing; Cast To... and
  Cast / Connect... (shared open_cast_window with the old button);
  Show Built-in Player; Play/Pause and Stop against the player
  window's media; Live TV radio; Video on Demand radio (shared
  open_vod_window); Add to Favorites (shared
  toggle_selected_favorite with the button); Go to Favorites;
  Previous Channel (the adapter now tracks the previously played
  channel); Go to Channel Number... (text entry, selects the row);
  Media Player to Use radios + Custom Player... (writes
  media_player / custom_player_path); Language radios (writes
  language; restart note announced); Minimize to System Tray check
  (the close handler now hides to the tray and vetoes the close
  when set); Show Player on Enter check (play_builtin honors it);
  Show Stream URL check (toggles the panel and the setting);
  Auto-check for Updates check (startup check wired);
  Export/Import Settings (config file copy out; validated JSON
  object copy in with a restart note); Automatic Announcements
  radios (announcement_level); Subtitle Speech: Speak subtitles
  check (the player timer reads it) and Read current subtitle;
  Check for Updates; User Guide; Open Logs Folder; Copy Log and
  Debug Information (clipboard); About.
- Present but disabled until their slices land, per the audit:
  Playlist Manager + EPG Manager (G5), What's on Now + Account
  Info + Continue Watching + Recently Watched + Show Downloads
  (G6), Preferred Audio Track + Keyboard Shortcuts + What's New
  (G7).
- The VOD, Cast, and User Guide buttons left the main window —
  their commands live in the menus now, as in Python. Open/Cancel
  stay until the Playlist Manager (G5) absorbs them; Record stays
  until the channel context menu (G3) lands.
- Deviations recorded: one menu bar on all platforms (Python uses
  a Menu-button popup on Linux for GTK reasons; the wx menu bar is
  functional and accessible under GTK). The announcement level is
  stored; per-level announcement gating in the adapter is a
  follow-up inside G7's scope.

## Automated evidence

- cargo fmt --check, clippy zero warnings, 112 workspace tests
  pass, playlist parity PASS, git diff --check clean.

## Owner-heard evidence still owed

NVDA pass over the five menus (names, radio/check states spoken),
the tray minimize behavior on Windows, and Go to Channel Number.
Menu construction is compile-verified here; no GUI runs on this VM.
