# 0033 — Replacement-gate pack for v0.2.0 readiness (#79)

Date: 2026-11-01. This pack is the evidence ledger Farhan's #79
spec asks for. Every gate records a status: **pass**,
**fail**, **blocked**, or **not-run**, with its environment.
Automated evidence never stands in for owner-heard evidence.

## Part 1 — Owner-heard NVDA and keyboard acceptance

Status: **not-run** (owner's setup is the only authority).
Checklist, to be worked through with NVDA, results recorded
per item as pass/fail with notes:

1. Launch: focus lands predictably; window title read.
2. Browse: scope Choice, search field, Categories tree, channel
   list — Tab order and names all read correctly.
3. Channel list: arrow navigation announces channel names;
   Favorites toggle (context menu and shortcut) confirms by
   announcement, not just visually.
4. Playback: Enter plays; player window opens with seek slider,
   time, subtitle readout, volume, tracks list all named;
   player shortcuts (Ctrl+P, Ctrl+S, Up/Down volume, F11,
   Ctrl+W, Ctrl+Q) operate without focus loss.
5. Recording: manual record start/stop announcements;
   Scheduled Recordings dialog — schedule, cancel, and a
   Stalker job in "resolving" state all read correctly.
6. Dialogs: Playlist Manager, EPG Manager, Channel EPG,
   What's on Now, Reminders, Catch-up, Downloads, Keyboard
   Shortcuts editor, Audio Preferences — open, navigate,
   dismiss with Escape/Close, focus returns to the list.
7. Menus: every menu opens by Alt keys; check/radio states read.
8. Failure recovery: play a dead URL — error is announced and
   focus stays usable; quit with an active recording — the
   countdown dialog is readable and Cancel works.
9. Close-to-tray and reopen from tray; Exit from tray quits.
10. Updates: Check for Updates announces both "up to date" and
    "update available" states.

## Part 2 — Fixture-backed seams (automated)

Status: **pass** on this Linux VM unless noted.

- Playlist/provider/EPG parsing: golden fixtures vs the pinned
  Python tree (parity tool PASS) plus core unit tests.
- libVLC **absent** path: engine returns a graceful error and the
  app falls back to MediaCtrl; covered by iptv-vlc tests on this
  VM, which has no libVLC. Startup does not depend on it.
- libVLC **present** path: **blocked** here (no libVLC); child
  ticket for the Windows VM.
- Recording lifecycle: ffmpeg command construction unit-tested;
  live seam check on this VM 2026-11-01 — ffmpeg 8.1.2 recorded
  3.11 s from a sanitized locally generated source with stream
  copy, output probed and valid. No credentials or private URLs.
- Stalker DVR resolution: correlation ids round-trip in order
  (core test, #68); stale/canceled results dropped by the
  adapter pending map (#70); retry/expiry covered in
  docs/validation/0032-stalker-dvr-hardening.md.
- Manifest selection and update download integrity: updater
  unit tests cover version compare, manifest parsing and
  SHA-256 verification failure handling.

## Part 3 — Casting evidence

- Protocol level: **pass** — UPnP AVTransport and Roku ECP
  request construction, DIDL metadata (including the Sonos
  variant) are unit-tested in cast.rs.
- Physical renderer: **blocked** — no renderer on this network.
  Protocol confidence is explicitly NOT device acceptance;
  child ticket tracks a physical session (discover, play,
  spoken status, stop) when a renderer is available.

## Part 4 — Packaging and update layout

Status: **pass** for the v0.1.0 layout (release workflow run
green on Windows/Linux/macOS; 3 platform packages plus
update-manifest.json whose SHA-256 matched the Windows asset,
verified by direct API read). The same layout ships v0.2.0.
A controlled Windows-VM install/update rehearsal remains a
child ticket; no workflow change is made by this pack.

## Ledger summary

- Automated gates (fmt, clippy, tests, parity): pass.
- Fixture seams: pass, except libVLC-present (blocked, Windows).
- Owner-heard NVDA/keyboard: not-run — Brandon's pass.
- Physical casting: blocked — no renderer available.
- Packaging: layout pass; Windows install rehearsal open.

Python remains the shipping product; nothing in this pack
authorizes a swap.
