# 0043 — Audio Output Device (issue #95)

Date: 2026-11-02.

The player Playback menu now has Python's "Audio Output
Device..." item (with its hard-coded D accelerator), backed
by real libVLC bindings:

- iptv-vlc gained audio_output_devices() and
  set_audio_output_device() over
  libvlc_audio_output_device_enum / _list_release / _set
  (optional symbols; absence degrades to an empty list).
  The device list is walked via next pointers and released
  before returning — Python's own note records that walking
  it any other way reads off the end and crashes; the empty
  id (libVLC's system-default entry) is skipped because the
  dialog offers its own, exactly as Python filters.
- The dialog mirrors Python's AudioDeviceDialog: hint text,
  "Audio output devices" list, "System default" first, then
  deduped devices by description (or id), the saved choice
  preselected, OK/Cancel, double-click accepts. No devices
  (or no libVLC engine) → Python's information box, "No
  audio output devices were found."
- A changed choice persists to the audio_output_device
  config key and applies to the live engine immediately;
  the saved device is re-applied at every play start
  (play_builtin), as Python applies it in its play path.
  Selecting System default stores the empty id and, like
  Python, makes no libVLC call — a fresh player starts on
  the default anyway.
- The guide's Audio Output Device section now describes
  the shipped dialog (it previously documented the gap).

Evidence class: automated — fmt clean, clippy zero warnings,
139 workspace tests pass, playlist parity PASS. The device
list itself can only be exercised with libVLC present —
that stays with issue #87 (this VM has no libVLC; the
absence path is what runs here). Owner-heard NVDA evidence
remains #86.
