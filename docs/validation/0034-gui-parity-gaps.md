# 0034 — GUI parity gaps pack (audit-driven)

Date: 2026-11-01. Branch: agent/gui-parity-gaps.

## How this pack was scoped

Two independent inventory audits (Python GUI, Rust GUI) were
diffed, and every claimed gap was then verified against the
shipping Python source (`client_ui.py`, `main.py`,
`internal_player.py`) before any code was written. Several
audit claims did not survive verification: the Python tree
contains dialog classes (standalone Search, Preferences,
Create Playlist, Channel Groups, Volume Normalization) that no
code instantiates — dead code, not parity targets. The real
gaps were in the Recordings menu area and details:

## Shipped

- **Recordings menu** (was missing entirely; Python has it as
  a top-level menu): Start Recording, Stop Recording, Stop All
  Recordings, Programme Reminders, Check Recording Storage
  (folder, free space, reserve), Minimum Free Space (writes
  `recording_min_free_mib`; the recording start gate now uses
  Python's `normalize_reserve` semantics — integer 0..=65536
  stands, anything else falls back to 256 MiB — instead of a
  fixed 500 MB), Scheduled Recordings, Recording Format
  submenu (the 10 presets as radios over the existing
  `recording_format` config), Schedule Padding (Python's
  fields, 0..=180, config keys `recording_pre_padding_minutes`
  default 0 / `recording_post_padding_minutes` default 2;
  `add_dvr_job` now reads them instead of hardcoding 2/5),
  Open Recordings Folder, Set Download Folder (DirDialog →
  `recordings_dir`), and the Shut Down the Computer When
  Recordings Finish check over the existing shutdown poll.
- **Check for Updates** moved from Options to Help, in
  Python's order (User Guide, What's New, separator, Check
  for Updates, Open Logs Folder, Copy Log and Debug
  Information, separator, About).
- **Automatic Announcements labels** corrected to Python's:
  None, Errors only, Important events, Detailed status.
- **Subtitle Speech submenu** completed over the
  screen-reader path: Read previous subtitle, Review recent
  subtitles (last 20 cues), Subtitle capabilities. The
  application-voice TTS backend remains unported (recorded in
  the parity audit update).
- **Shutdown countdown** corrected to Python's 60 seconds,
  title and message text, with Cancel Shutdown and Shut Down
  Now buttons.
- **Scheduled Recordings dialog**: Refresh and Delete
  (finished jobs only, `DvrSchedule::remove_job`) added
  beside Cancel.
- **What's New text** rewritten to describe 0.2.0 accurately.
- **Guide**: Recordings storage/reserve lines and the
  Keyboard Shortcuts editor details added to docs/help/en.md;
  docs/gui-parity-audit.md gained a verification update
  closing its sections 3 and 4.

## Core additions

- `recording::normalize_reserve_mib` (+ unit test against
  Python's boundary values), `RecordingEngine::active_keys`,
  `DvrSchedule::remove_job`.

## Automated evidence

- fmt clean, clippy zero warnings, 124 workspace tests pass,
  playlist parity PASS, `git diff --check` clean.

## Manual evidence still owed (owner)

- NVDA pass over the Recordings menu and the new dialogs
  (storage, padding prompts, subtitle review) — part of the
  gate-pack checklist, issue #86.
