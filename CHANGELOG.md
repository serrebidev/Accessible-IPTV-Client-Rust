# Changelog

Readable release history for Accessible IPTV Client, newest first. When
adjacent releases were part of the same fix stream, they are combined with a
version range such as `0.2.2-0.2.3`. Each release's section below is its
Forgejo release note, taken from this file by the release workflow.

## 0.4.37

- The player gains Mute/Unmute and an optional per-channel volume memory, plus Back/Forward 10 and 60 second seeks, Go to Time, and a Playback Time announcement on on-demand media.
- You can save named positions inside a recording and jump back to them, and choose which events the announcements speak for: Playback, Subtitles and Guide.
- Notification History lists the session's boxes with Details, Refresh, Copy and Clear. Reminders gain per-event switches, the Recording Library gains Export List to CSV, and Recordings gains a Connection Limits dialog.
- Favourite Lists manage up to 50 named lists of 2000 channels with portable export, import and relinking of missing channels.
- Programme Guide by Time shows what airs at any local time, with Programme Details, scheduling, reminders and calendar export. Recently Watched now resolves channels per playlist source, and Go to Channel Number understands the channels' own numbers.

## 0.4.36

- Scheduled Recordings now offers Weekday repeats, skipping one occurrence, extra time before and after, finding another airing, and checking what changed in the guide. Series rules skip episodes you already have, and recordings say when they were partly recorded or stopped by you.
- Backups now keep the whole setup: settings, scheduled recordings, reminders and searches, in the same file the Python client writes, so you can swap between them.
- View gains Assign Programme Guide: pin a channel to a guide entry, or go back to automatic matching, and every guide view follows the assignment. Guide entries that clash between sources stay separate instead of overwriting each other.
- A missed recording can be recovered from the channel archive. The app can keep the computer awake while recording and wake it for a scheduled recording, warn when the drive is running out of space, and save what you just watched as a clip.
- The player can now read subtitles built into a recording and step through its chapters. Recordings also gain a playback queue that starts at a problem time, and series can roll straight into the next episode after a short countdown.
- Reminders gain Remind Again in 5 Minutes, Remind at Programme Start, and Export to Calendar.

## 0.4.35

- Updated the desktop runtime and media-library dependencies. Release builds reuse compatible cached artifacts and use current packaging tools.

## 0.4.33

- When a stream cannot be recovered, the Stream Lost message now appears once. Before, closing it could open another one, over and over.

## 0.4.32

- The built-in player can now boost volume up to 200 percent for quiet streams: the slider, volume keys and mouse wheel all run to 200, matching the Python client.

## 0.4.31

- Help > About now has a separate link for each of GitHub, Forgejo, the projects site, Telegram and the full credits: Tab reaches each one and Enter or Space opens it.

## 0.4.30

- Help > About now lists the author, the GitHub and Forgejo pages, the projects site and the Telegram channel, and points to CREDITS.md for full credits.
- The README follows the shared layout and credits the libraries and upstream code the app uses.

## 0.4.29

- No more console windows. Every tool the app runs in the background (ffmpeg, the proxy lookup, the firewall rule, shutdown) now starts hidden. Before, a terminal window could open at startup and again every time you paused on a channel, taking the focus away from the app, which made moving around and switching back with Alt+Tab feel frozen.
- Search now waits for you. Typing in Search channels no longer refilters the whole channel list on every key. Press Enter to search and move to the channel list, or just leave the field with Tab, Shift+Tab or a click. Clear the field and leave it to see every channel again.
- Moving through the channel list does less work: the Episode description and Stream URL fields are only rewritten when their text actually changes, so your screen reader has fewer updates to process.

## 0.4.28

- Recordings > Recording Library lists finished recordings from up to 20 folders: search, filter by completion status, sort by date, title, duration or file size, play or resume, rename titles, move recordings to the recording trash and restore them. Active recordings are never listed, and every finished recording is indexed as it completes.
- The built-in player gains Playback > Previous Episode and Next Episode (Ctrl+PageUp and Ctrl+PageDown, configurable) and Playback > Subtitle Timing (Ctrl+Shift+T): the subtitle delay, up to ten minutes either way in milliseconds, applies to display and built-in subtitle speech for the current media and resets when new media starts.
- Scheduled Recordings gains Edit Times and Recording Conflicts. Overlaps are computed with schedule padding against the per-account recording connection limits; a new recording that would exceed a known account limit asks for confirmation with the overlapping recordings listed, and Recording Conflicts shows each overlap's channel, account and real recording window.
- Continue Watching gains a Type choice between Video on Demand and Recordings. Recording positions live in their own store, are checked against the recording library by exact file signature, and saves run in the background: a failed save leaves the saved positions untouched and says so.
- An Xtream account whose server rejects the saved credentials now reports an authentication failure instead of presenting an empty catalogue.
- Copied text now survives closing the app: every copy (stream URLs, account details, diagnostic information, recording problem times, EPG source URLs) goes through one helper that flushes the clipboard before closing it, the pattern already shipped in BlindRadio and BlindRSS-Rust.

## 0.4.26

- The About box links and the user guide's bug-report address now point to the project's Forgejo home, as the Python client's do; the old GitHub addresses no longer open. All three packages (Windows, Linux and macOS) are now built and published by the Forgejo release workflow.

## 0.4.25

- Updates now come from the project's Forgejo home instead of GitHub: Check for Updates, and the optional launch check in Options, ask the Forgejo release page for the newest version, and releases are published there from now on. The previous version still checked GitHub, so this one must be installed by hand; afterwards updates resume automatically from Forgejo. This release carries the Windows package.

## 0.4.24

- Category names are spoken again in the Categories tree. NVDA announced only “Categories, tree view” for every item and never the category itself, because naming the tree the way the newer interface library prefers had replaced the tree’s own accessibility with a stub that knows no items. The tree now takes its name the way the Python client does, so All Channels, Favorites and every playlist category are read aloud with their position and level as you move through them. The tree also gains the expand buttons and root lines the Python tree shows.

## 0.4.23

- Translations now run on patois, the gettext library that translates Paperback: the catalogues for all 13 languages are compiled into the program itself instead of travelling as a folder of files beside it, and the standard dialogs’ own strings now follow the app’s language too. Same translations, same language menu, same best-fit choice for your system language.

## 0.4.22

- Release notes are now written for people, in this file, and the GitHub release page shows them. Every past release's notes have been filled in the same way, back to the first one. The release process now refuses to publish a version whose changelog section is missing, so an empty release page cannot ship again.

## 0.4.21

- Escape now closes windows, as it always did in the Python app: every list window (What’s on Now, Continue Watching, Recently Watched, Reminders, Account Info, Downloads, the programme guide and more), the Playlist and EPG managers, the account form, Scheduled Recordings, Recording Conflicts, the Reminder, VOD, Cast, Audio Preferences, Shutdown Countdown and User Guide windows all hide on Escape, and the hand-built dialogs (sliders, Recording Problem Times, About, Custom Player, Resume) cancel on Escape. Before, Escape did nothing in any of them.

## 0.4.20

- This release stops the Windows console window from opening alongside the app: the program is now a proper Windows GUI application, like the Python client. Nothing else changes from 0.4.19.

## 0.4.19

- This release removes the Windows debugging-check dialogs for good. They were never app errors: the interface library was being built with its developer consistency checks switched on, so an internal check that a shipped build is supposed to skip could stop the app with a dialog (the menu check and the tree check in earlier versions were the same thing). The library is now built the way release builds are meant to be, with those checks compiled out, matching the Python client. Also fixed: switching the player into or out of fullscreen now settles its layout and focus just after the switch, as the Python client defers them.

## 0.4.18

- This release completes the second parity pass against the Python client. Chromecast casting works now: devices are found on your network, the channel plays through the cast proxy, the app asks before disconnecting, saved positions and local buffering are refused while casting, and a casting session counts against one-connection providers. Translations ship with the app: the Python client's catalogues and help guides in 14 languages are bundled, so a non-English system language now actually translates the interface and the guides. A new Account Info window finds your provider accounts, including accounts detected from playlist and channel addresses, and shows each account's status and limits, with Copy Details. Guide imports match Python: gzipped guides resume after an interruption, imports lock across app copies, an old guide database offers Python's repair prompt, and the import's start and finish are announced. Dialogs carry Python's explanations as readable text, the About box gains the community links, What Is Playing and Go to Channel answer the way Python's do, the category tree branches per playlist with an Other playlists branch and channel counts, Favorites messages match Python's, the player status line shows buffer, bitrate and volume in Python's format, audio track changes are spoken, and the mouse wheel over the player changes the volume. Recording formats now follow the selected channel's stream type, failed catch-up downloads retry and explain themselves, and message boxes that arrive while another is open now queue instead of stacking. The guide import lock also clears promptly on Windows and macOS when the copy of the app that held it has closed.

## 0.4.14

- This release keeps the window responsive while the saved guide loads, and brings more of the Python client's behaviour across. Channel details, series scans, guide search and What's on Now now read the saved guide on a background thread instead of freezing the window, and results that arrive after you have moved on are dropped instead of replacing what you are looking at. Guide matching now uses the Python client's scored matcher, so channels find their correct guide listings more reliably. Recordings gain the Python client's per-recording logs, outcome reports and Recording Problem Times view, and a failed download now explains the provider error in plain words instead of leaving a raw code.

## 0.4.13

- This release makes the built-in player's timers actually run, and stops decoding video when the player window is hidden. Reconnect, resume, subtitle speech and the sleep timer now work in the real app; they were silently dead in every earlier build. Hidden playback uses far less processing power (about 1.5 percent of one core instead of about 12), and pressing Enter while the player engine is still starting no longer freezes the window; playback begins when the engine is ready. Also fixed: two libVLC data layouts that could corrupt memory, and the release packages now bundle the newest VLC 3 release.

## 0.4.12

- This release adds VOD search and item details, and stops the window freezing during playback. In the VOD window, the new Search VOD field searches movies and series across every group: type a title and press Enter; each result row names its group and where it comes from, the status line says how many items matched, and clearing the search brings back the group you were browsing. The new Item details view shows the selected item's title, type, group, source, its guide description when one is available, and its stream address. Opening a series from the results and going back returns you to the same results with that series still selected. Also fixed: the guide information on channel rows now refreshes on a background thread instead of on the window itself, so alt-tabbing back into the app during playback answers right away instead of taking five to ten seconds, and a refreshed guide merges in much faster.

## 0.4.11

- This release makes startup and channel changes feel instant with a large guide. The channel rows used to freeze for several seconds after each playlist load, because refreshing the guide information on each row scanned the whole saved guide; it now looks at just the hours around the current time, the same way the Python client does. On a guide with about 800,000 programmes that step drops from about five seconds to a fraction of a second, so new channels are ready to use as soon as they appear.

## 0.4.10

- This release speeds up the programme guide and fixes a recording check. The guide now loads and refreshes as fast as the Python client or faster: on a large test guide, a first load dropped from about thirteen minutes to about two and a half, a refresh from about three minutes to about one, and opening the app with a guide already saved is instant instead of taking most of a minute. Recordings work again on non-English Windows systems: the free-space check before recording no longer depends on localized command output (thanks to Farhan Khan for that fix). And the player no longer logs a warning about a plugin-path option that current libVLC removed; it uses the mechanism libVLC supports now.

## 0.4.9

- This release refreshes everything under the hood: the cryptography, the guide parser, the database and the platform libraries are all at their newest released versions. Nothing changes on screen, and settings backups stay fully compatible; backups written by earlier versions still restore.

## 0.4.8

- Casting works with header-protected streams again. The built-in cast proxy now answers the HEAD requests casting devices send before playing, follows redirects so playlists resolve against the address they actually live at, and passes range and duration headers through, so casting devices can probe a stream properly. Streams that need a specific player identity now get the default one automatically. Thanks to Farhan Khan for the fix.

## 0.4.7

- Faster with big playlists and guides, and the last parity gaps closed. Large playlists now keep a background index, so switching playlists or categories and searching stay quick even with hundreds of thousands of channels, and the guide shown on each row refreshes almost instantly instead of re-checking every channel against the guide every minute. Scheduled Recordings has the right-click menu (Refresh, Cancel, Delete) like the Python client. The preferred audio track check now waits as long as the Python client does, so a slow stream is less likely to lose your saved track. Closing the app no longer waits on the player engine. The log file lives where the Python client puts it. And pressing F1 while a menu is open now opens the help for the menu item you are on.

## 0.4.6

- Fix: single-instance handover now works on Windows (the lock file is opened with the access Windows requires). Plus the Python parity catch-up. Guide sources saved by the Python client now load, the saved guide is there again after a restart, and search finds programmes as well as channels. Provider playlists keep their own channels separate, replace cleanly on refresh, and a failed refresh keeps the channels you had. Resume positions now read the Python client's bookmarks, so Continue Watching carries across. Recordings get unique file names, retries, destination space checks, and honest empty-recording reports; reminders and series recording match the Python dialogs. The app is single-instance (a second launch hands over to the running one), keeps a real log file under Help, checks for updates hourly, and F1 opens the help topic for the window you are in.

## 0.4.4

- Less talk, working keys. Controls no longer read out instructional sentences when you land on them — Categories, Search, and Channels announce their names, like the Python app, and the channel list no longer repeats the playlist status message every time you tab to it. Ctrl+Up and Ctrl+Down now only change the volume; they no longer move the channel or category selection at the same time. And the search field selects your whole query when you enter it, so typing replaces it, with Ctrl+A selecting all.

## 0.4.3

- Recording conflicts and safer imports. When a scheduled recording would take a provider connection your watching is using, the recording waits with its reason shown in Scheduled Recordings instead of cutting in; you choose there: keep watching and let it wait, stop playback and record, or cancel the recording. Waiting recordings start fairly, earliest first, when a connection frees. And importing settings now first saves a verified encrypted recovery copy of your current settings where you choose, under its own password, and refuses to run while recordings or downloads are active. The basic player (when libVLC is unavailable) also reports its state honestly now, and its menus no longer offer full-player-only controls.

## 0.4.2

- Reliability and subtitle speech. Playback now confirms before it counts as playing, and a failing stream retries inside one 30-second budget with a clear Stream lost message instead of spinning forever. Recordings that start late say so honestly, and cancelling a scheduled recording now stops its capture. Settings backups interchange with the Python client, verified both ways. Subtitles can now speak through an application voice on Windows (SAPI), not only through your screen reader: pick the backend, output device, volume, and speed under Options, Subtitle Speech, and if the chosen output device disappears, speech pauses until it returns or falls back to the system default, per the device policy.

## 0.4.1

- Less talk. Channel rows now carry the guide the way the Python app does — channel name first, then the current and next programmes after it — and moving through the list no longer speaks the guide out loud on every step; the description panel still fills in silently.

## 0.4.0

- Signed updates. Check for Updates now uses the ship-shape updater, the same signed updater as BlindRadio: update packages are cryptographically signed and verified before anything installs, then install and relaunch on their own (Windows and Linux; macOS opens the verified disk image for a manual install). Releases before this one can still update forward onto it.

## 0.3.4

- Fix: large playlists load promptly at startup. The playlist now loads first and the programme guide follows once it settles, instead of the two downloads racing; the guide database write no longer freezes the window; and the channel list says Loading playlist while a load is in flight instead of claiming no playlist is loaded.

## 0.3.3

- Fix: streams play again. The packages now bundle libVLC (the built-in player engine) beside the app on Windows, Linux, and macOS, so playback works with no separate VLC install. If libVLC still cannot load, pressing play now says so instead of failing silently.

## 0.3.2

- Fix: large playlists and guides now load. The HTTP library silently capped downloads at 10 MB, so a big provider playlist (for example a 28 MB, 115,000-channel list) failed to add and its EPG could never load. Playlist, EPG, Xtream, and Stalker fetches now take the full body.

## 0.3.1

- Fix: the Windows build no longer shows a wxWidgets debug dialog ("Can't expand/collapse hidden root node!") at startup or when the category list rebuilds. Nothing else changes from 0.3.0.

## 0.3.0

- Main window: the layout now matches the Python client exactly (Playlist view and Categories on the left; Search, Channels, description and Stream URL on the right), and the menu-less shortcuts from the Python client work here: Ctrl+Up/Down volume, Ctrl+Shift+R record, Ctrl+D favorite and the rest of the documented set.
- Player: a Playback menu in Python's order, a Sleep Timer with an optional volume fade-out, and an Audio Output Device dialog that lists the real libVLC devices (System default first) and applies the saved choice at playback start. Announcement levels now gate player speech exactly as Python's do (None, Errors only, Important events, Detailed status).
- Local Live Buffer: live TV in the built-in player can be buffered locally (off by default; set it up from the player's Playback menu), which enables pausing live TV, Rewind Live TV and Return to Live. A recording of a buffered channel reads the buffer instead of opening a second provider connection.
- Dialogs: What's on Now has its filter box with a program count; Continue Watching gains Check Availability, which re-resolves each saved position against the current playlists and reports Available or Unavailable per row. Settings backup and restore now use Python's password-protected encrypted format, so backups interchange with the Python client both ways.
- Menus and shell: the Media Player menu lists all 32 players the Python client offers, with its exact per-platform program lookup. The tray icon builds Python's popup fresh on every right-click (Restore, Player Controls, Stop Recording(s) while recording, Exit) and left-click restores the window. The Recordings menu's minimum-free-space reserve and schedule padding use Python's values.

## 0.2.0

- The window and menus move closer to the Python client: the Recordings menu, Help order and speech submenu match it, the shutdown countdown runs its full 60 seconds, and Stalker recordings resolve their streams and retry failures more reliably.

## 0.1.0

- First public release of the Rust client.
