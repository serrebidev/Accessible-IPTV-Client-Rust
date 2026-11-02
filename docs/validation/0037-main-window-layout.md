# 0037 — Main window layout (issue #101)

Date: 2026-11-02.

The main window now matches the Python client's layout:

- Frame 800×600 (was 720×480).
- Two columns: left (proportion 1) Playlist view + Categories
  tree; right (proportion 2) Search, Channels, Episode
  description, Stream URL.
- The Toggle favorite button is gone (Python has no button row;
  favorites toggle via the View menu, Ctrl+D and the channel
  context menu, as before).
- The orphaned Record button (created and wired but never laid
  out) and its binding are removed; recording starts via the
  Recordings menu, the context menu, the player and Ctrl+Shift+R.
- The visible "Playlist status" label and read-only details
  field are gone from the layout. The Status carriers still
  exist but are hidden — status text rides the frame status bar
  and the hidden live-region control, exactly Python's model
  (its announcer is a hidden control; its status bar is the
  visible surface). Dialog status fields are unchanged: Python
  dialogs do show status text.

Evidence class: automated — fmt clean, clippy zero warnings,
132 workspace tests pass, playlist parity PASS. Owner-heard
NVDA evidence remains issue #86 (tab order follows creation
order: Playlist view, Categories, Search, Channels, Episode
description, Stream URL — the hidden carriers are skipped).
