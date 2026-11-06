# 0097 — Escape closes windows (v0.4.21)

Brandon's report (2026-11-06): "There's a lot of
accessibility issues with windows not being able to use
escape to close windows."

## Audit

Python gives every secondary window an Escape path:
dialogs carry EVT_CHAR_HOOK handlers ending the dialog
with ID_CANCEL (dialogs_misc.py:541, main.py:9359,
dialogs_misc.py:949), the downloads dialog hides on
Escape (main.py:8908 hide_window), Scheduled Recordings
closes (main.py:9065), and wx's own dialogs cancel on
Escape natively. The player exits fullscreen on Escape
(internal_player.py:2294).

The Rust port turned most of those dialogs into modeless
frames and hand-built dialogs whose buttons are closures,
not the standard ids wx looks for — so Escape did
nothing at all in:

- every window built on list_dialog (What's on Now,
  Continue Watching, Recently Watched, Reminders,
  Account Info, Recent Subtitles, Audio Output Device,
  the programme guide — whose own label promises
  "Press Escape to close" — and Downloads, which had a
  bespoke KEY_DOWN pair from N15);
- the Playlist Manager, EPG Manager, Account form,
  Scheduled Recordings, Recording Conflict, Reminder,
  VOD, Cast, Preferred Audio Track, Shutdown Countdown
  and User Guide frames;
- the hand-built modal dialogs: slider_dialog (volume,
  seek, padding and friends), Recording Problem Times,
  About, Select Custom Player, and Resume.

Already correct, untouched: the player's unbound-Escape
fullscreen exit (dispatch_player_key's Esc special
case), the playlist scope combo's Escape revert, and
the built-in wx dialogs (message, text entry, file,
choice, directory), which cancel on Escape natively.

## Fix

Two shared bindings in main.rs, mirroring Python's
mechanism (CHAR_HOOK on the window itself, so Escape
works whatever control holds focus):

- bind_escape_hide(frame): Escape hides the frame —
  the verb every one of these windows' own Close button
  already uses.
- bind_escape_cancel(dialog): Escape ends the dialog
  with ID_CANCEL. For the Resume dialog that lands in
  its existing not-ID_OK path, i.e. Cancel, without
  touching the chosen cell.

list_dialog binds its own frame, which covers all nine
list windows at once; Downloads' bespoke N15 KEY_DOWN
pair is removed as superseded. Each standalone frame
and custom dialog binds at construction, right after
its accessibility label is set.

## Verification

Local gates (fmt, clippy -D warnings, workspace tests,
playlist parity, diff check) plus the Release run's
cross-platform Gates step. Brandon's hands-on pass —
open any of the named windows and press Escape — is
the acceptance check.
