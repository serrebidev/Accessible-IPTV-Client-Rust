# Validation 0022: GUI parity G3 (scope, context menu, Channel EPG)

Date: 2026-11-01. Issue: #57 (program #54). Spec:
docs/gui-parity-audit.md; Python client_ui.py and the channel
context menu in main.py.

## Landed

- Playlist view scope: a "Playlist view" Choice before Categories
  (All playlists + one entry per configured source/account). Core
  BrowserWorker gained a playlist scope: rows and the category
  list are filtered by the channel's playlist-id, with a core test
  covering row counts and category filtering across two sources.
- Channel context menu on the Channels list, built at popup time
  with state-aware labels (Add/Remove from Favorites, Record/Stop
  Recording): Play (shared activate_channel with Enter), Favorite,
  Record toggle, Set Reminder... (current programme, 5-minute
  lead), Schedule Recording... (current programme into the DVR
  schedule with the configured format and default padding), View
  EPG..., Catch-up... (resolve_show_url; generic/Xtream URLs play
  directly, Stalker resolves through the provider worker).
- Channel EPG window: the channel's programmes (from an hour ago,
  up to 200) with Set Reminder and Schedule Recording on the
  selected programme — the base the full EPG viewer (G6) grows.
- Enter-to-play factored into activate_channel, shared by the
  list, the context menu, and Previous Channel.

## Still open in this slice

- Categories as a tree control (the remaining G3 item) lands as a
  follow-up PR on this issue.

## Automated evidence

- cargo fmt --check, clippy zero warnings, 113 workspace tests
  pass (one new core scope test), playlist parity PASS,
  git diff --check clean.

## Owner-heard evidence still owed

NVDA pass: scope picker announcements, context menu opening and
labels, Channel EPG window flow. Compile-verified here.
