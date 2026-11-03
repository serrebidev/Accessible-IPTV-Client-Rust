# 0048 — Built-in player could not play: packages shipped no libVLC

Date: 2026-11-02.

The owner, on the v0.3.2 Windows package (verified: he is
running the v0.3.2 portable build, the app reports itself up
to date), could not play any stream. His stream URLs serve
fine (probed directly: HTTP 200, video/mp2t flowing).

Root cause: the release packages contained only the
executable and docs — no libVLC. The built-in player is
libVLC (dlopen'd); the Windows loader tried only the bare
name libvlc.dll, which resolves in the executable's folder
or PATH. With no bundled copy and no VLC install in either
place, engine creation failed, the code silently fell back
to wxMediaCtrl, and the MPEG-TS streams failed there
without any message. (This is the #87 live-seam gap biting
exactly as the gate pack predicted: the libVLC-present path
had never run on Windows.)

Fixes:
- iptv-vlc: the loader now tries the copy beside the
  executable first on every platform (then the bare name,
  then the existing macOS system paths), and passes
  --plugin-path pointing at a bundled plugins/ folder when
  one sits beside the executable.
- Release workflow: each platform job now bundles the
  libVLC runtime into the package — Windows takes
  libvlc.dll, libvlccore.dll and plugins/ from the latest
  official VLC win64 zip; Linux installs libvlc5,
  libvlccore9 and vlc-plugin-base and ships the libraries
  plus plugins/ (the Linux binary also gains an $ORIGIN
  rpath so the bundled libraries resolve); macOS takes the
  dylibs and plugins/ from the latest official arm64 dmg.
- When libVLC still cannot load (source builds, damaged
  package), pressing play now says so on the status line —
  "libVLC is not available (...); the basic player is being
  used..." — instead of failing silently.

Also from the owner's session (same report): the playlist
did load once given time. Startup downloads the 28 MB
playlist and the 263 MB guide simultaneously and the
channel list's initial text reads "No playlist loaded"
while the load is in flight. The sequencing and
UI-freeze parts of that are a separate slice (0049):
defer the guide until the playlist settles, mirror the
guide to SQLite on the worker thread instead of the UI
tick, and show "Loading playlist..." while a load runs.

Evidence class: automated — fmt clean, clippy zero
warnings (-D warnings), workspace tests pass (incl. a new
loader-order test), playlist parity PASS. The owner's
Windows playback retest closes it; ships as v0.3.3.
