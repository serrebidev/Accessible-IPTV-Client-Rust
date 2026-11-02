# GUI parity audit: Python client vs Rust client (2026-11-01)

Brandon's direction: the Rust GUI should be the Python GUI, UI-wise.
The Python app's `client_ui.py` (`_build_ui`), `main.py`, and
`internal_player.py` are the spec. This audit inventories both sides
and defines the alignment program (issues G1–G7).

## The Python main window (spec)

- Frame titled "Accessible IPTV Client" with a **status bar**; no
  button row. Commands live in menus and shortcuts.
- Two-column layout. Left column: a "Playlist view" Choice (scope:
  one playlist or All playlists), then the "Categories" **tree**.
  Right column: "Search" text field (Enter submits), the "Channels"
  **virtual list**, an "Episode description" read-only multiline
  field, and a "Stream URL" read-only multiline field (visibility
  follows the `show_channel_url` setting, default shown).
- Tab order after the channel list is episode description, then
  stream URL — the description is what a viewer wants after a row;
  the URL is a diagnostic.
- Highlighting a channel refreshes the EPG description and URL
  panels. F1 opens the User Guide at the section for the focused
  control (per-control help topics).
- Menus (menu bar on Windows/macOS; on Linux a single "Menu"
  button whose popup carries the same content):
  - File: Playlist Manager (Ctrl+M), EPG Manager (Ctrl+E), Import
    EPG to DB (Ctrl+I), What's on Now (Ctrl+W), What Is Playing
    (Ctrl+Shift+I), Account Info (Ctrl+Shift+A), Cast To..., Exit
    (Ctrl+Q).
  - Player: Show Built-in Player (Ctrl+Shift+J), Play/Pause
    (Ctrl+Shift+P), Stop (Ctrl+Shift+S), Cast / Connect...
    (Ctrl+Shift+C).
  - View: Live TV & Catch-up / Video on Demand (radio pair),
    Continue Watching..., Add to Favorites (Ctrl+D, label follows
    the selected channel), Go to Favorites, Previous Channel
    (Ctrl+0), Recently Watched... (Ctrl+H), Go to Channel
    Number... (Ctrl+G), Show Downloads (Ctrl+Shift+D).
  - Options: Media Player to Use (radio list + Custom Player...),
    Preferred Audio Track..., Language (radio list), Minimize to
    System Tray (check), Show Player on Enter (check), Show Stream
    URL (check), Auto-check for Updates (check), Export
    Settings..., Import Settings..., Automatic Announcements
    (radio: Off / Minimal / Important events / Detailed status),
    Subtitle Speech (engine radios, Speak subtitles check, Read
    current/previous subtitle, Review recent subtitles..., voice
    volume/output/speed, Subtitle capabilities...), Keyboard
    Shortcuts..., Check for Updates.
  - Help: User Guide (F1), What's New, Open Logs Folder, Copy Log
    and Debug Information, About...
- Channel context menu: Play, Add/Remove Favorite (dynamic),
  Record / Stop Recording, Set Reminder..., Schedule Recording...,
  View EPG..., Catch-up...
- The built-in player window: Play/Pause toggle, Stop, Record,
  Cast, Full Screen buttons; a Volume slider; a "Choose Audio
  Track" Choice; a status label; a subtitle label; the PLAYER
  shortcut table dispatched live.
- Dialogs: Playlist Manager (source list; Add File / Add URL / Add
  Xtream Codes / Add Stalker Portal; edit/remove; OK/Cancel), EPG
  Manager, Scheduled Recordings, Channel EPG, Catch-up, Catch-up
  Download + Downloads window, Cast Discovery, Account Info,
  What's on Now, Reminders, Continue Watching, Audio Track
  Preference, Recording Padding, Shutdown Countdown, Recording
  Problem Times, Audio Device, About, What's New (changelog),
  Keyboard Shortcuts editor, User Guide.

## The Rust GUI today

- Main window: a button row (Open playlist…, Cancel loading,
  Record, VOD, Cast, User Guide), a Category **list box**, Toggle
  favorite button, Search field, Channels list, and a status
  label + details text. Menu bar has only File (Exit) and Help
  (User Guide, Check for Updates). Tray icon present.
- Separate windows: Player (Pause/Stop, Load subtitles — no volume,
  no audio-track choice, no seek, no Full Screen, no Record/Cast),
  VOD (group/item lists), Cast (device list, Play/Stop/Refresh),
  User Guide (contents, text, Find — close to the Python dialog).
- Missing entirely at the UI level: Playlist view scope, Categories
  tree, episode description and stream URL panels, status bar,
  channel context menu, per-control F1 topics, Playlist/EPG
  managers, Scheduled Recordings dialog, Channel EPG dialog,
  reminders/continue-watching/what's-on dialogs, account info,
  downloads window, options (player choice, language, checks,
  announcement levels, subtitle speech), shortcuts editor,
  About/What's New, shutdown countdown dialog.

## Alignment program

- G1: main-window information surfaces — status bar mirroring the
  spoken status, Episode description + Stream URL panels wired to
  the highlighted channel (EPG now programme description; URL per
  `show_channel_url`), tab order list → description → URL.
- G2: the full menu system (File/Player/View/Options/Help as
  above) with labels and shortcut text from the shortcuts engine;
  commands factored out of the button closures; the button row
  retires as its commands land in menus; Linux gets the Menu
  button popup form.
- G3: channel context menu (Play, Favorite, Record, Set
  Reminder..., Schedule Recording..., View EPG..., Catch-up...)
  and the Playlist view scope + Categories tree.
- G4: player window parity — volume slider, audio-track Choice,
  seek/live-buffer controls, Full Screen, Record/Cast buttons,
  subtitle label, live PLAYER shortcut dispatch (engine landed in
  slice 12; track selection completes with the libVLC endgame
  #49).
- G5: Playlist Manager + EPG Manager dialogs (absorbs #51
  accounts; Open playlist retires into the manager).
- G6: programme and library dialogs — Channel EPG (absorbs #50),
  Catch-up, Reminders, Scheduled Recordings (unblocks DVR job
  creation deferred since slice 7), Continue Watching, What's on
  Now, Account Info, Downloads window.
- G7: options and system dialogs — Keyboard Shortcuts editor,
  Preferred Audio Track, Export/Import Settings, About + What's
  New, Shutdown Countdown, per-control F1 help topics.

Slices land in order; each is its own issue, PR, gates, and
validation note, per the repo flow.
