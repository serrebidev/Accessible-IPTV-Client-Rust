# 0038 — Player Playback menu + Sleep Timer (issue #94)

Date: 2026-11-02.

The built-in player window now has Python's Playback menu bar:
Play/Pause (with the effective binding shown), Stop, Sleep
Timer..., Record, Audio Track submenu, Load Subtitle File...,
Speak Subtitles (check, mirrors the setting both ways), Read
Current Subtitle, Previous Subtitle, Next Subtitle, Cast...,
Toggle Full Screen, What Is Playing, Hide Window, Exit Player.
Every item reuses the existing button/shortcut helpers.

- The Audio Track submenu starts with Python's disabled "No
  audio tracks available" item; when libVLC publishes tracks it
  is rebuilt as radio items mirroring the track Choice, kept in
  sync both ways (menu selection drives the Choice and the
  engine; track application checks the matching radio). wx
  menus are owning wrappers, so the code fetches borrowed
  handles from the frame's menu bar (position 5 in the Playback
  menu) instead of storing them.
- Sleep Timer (#94) follows Python's spec exactly: minutes
  prompt 0-1440 with Python's text (0 cancels), then the
  fade-out seconds prompt (0-min(300, minutes*60), default from
  the sleep_fade_seconds setting, 5, persisted). The 1-second
  player tick drives it: at the fade window the announcement
  "Playback volume is fading before the sleep timer stops it."
  fires once, the volume ramps linearly to zero, and the
  deadline stops playback and restores the pre-fade volume for
  the next play. Cancelling mid-fade restores the volume too.

Staged for their own issues: Local Live Buffer / Rewind /
Return to Live menu items (#96), Audio Output Device (#95),
and the announcement-level gate on the fade message (#102 —
Python priority 2).

Evidence class: automated — fmt clean, clippy zero warnings,
workspace tests pass, playlist parity PASS. Owner-heard NVDA
evidence remains #86; libVLC-present track-menu behavior
remains #87.
