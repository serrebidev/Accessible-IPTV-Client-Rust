# 0095 — The wxWidgets debugging-check dialogs, root cause and class fix

Date: 2026-11-06. Branch `agent/fix-fullscreen-layout` from main
b998edc (v0.4.18). Ships as v0.4.19.

## Report

Brandon, on the published v0.4.18 Windows build, while just
streaming (no fullscreen toggle, no dialog interaction):

```
wxWidgets\src\msw\window.cpp(6027):
assert "m_hDWP" failed in EndRepositioningChildren(): Shouldn't be called
```

## Root cause of the whole dialog class

wxdragon's DEFAULT Cargo features include `wx-asserts`
(wxdragon 0.9.24, `[features] default = [..., "wx-asserts",
...]`). With it, wxdragon-sys builds wxWidgets with its
internal consistency checks live; without it, the build sets
`wxBUILD_DEBUG_LEVEL=0` / `wxDEBUG_LEVEL=0`. The app took the
defaults, so checks that a shipped build is supposed to
compile out have been stopping the app with a modal dialog
all along. Every debugging-check dialog Brandon has hit in
this app is this one configuration fact plus a different
minor wx-internal gripe each time:

- v0.3.1 era: `!IsHiddenRoot(item)` in the tree control.
- v0.4.14 era: `wxMenu::Delete` on an absent item.
- v0.4.18: this `EndRepositioningChildren` assert.

wxPython ships release wx (checks compiled out), which is
why the Python client never shows these. wx's own guidance
says the same: these checks are for the program's developers,
not its users, and production builds should not have them.

## The fix

The workspace root `Cargo.toml` now defines wxdragon with
`default-features = false` (Cargo only honors
`default-features` at the workspace definition — a member
crate's setting is ignored, which a cargo warning caught
during this fix), and `crates/iptv-app/Cargo.toml`
re-enables every other default explicitly (media-ctrl,
wx-printing, wx-tiff, wx-jpeg, wx-tango-icons,
wx-extra-image-formats, wx-regex, appindicator). The only
build change is `wx-asserts` off.
With checks compiled out, a check like this one degrades to
what pressing Continue always did — the operation completes
as a no-op and playback is never interrupted. The individual
gripes that remain reachable in code (the menu-delete guard
from PR #191) stay fixed in code as before; this change
removes the dialog, not the correctness.

Note for the other wxdragon lanes (BlindRadio, BlindRSS-Rust,
BlindFB): they take wxdragon's defaults too unless changed;
same one-line class fix applies there.

## This instance's mechanism (for the record)

wxMSW positions children in deferred sessions
(`BeginDeferWindowPos`); `EndRepositioningChildren`
(msw/window.cpp:6025 in wx 3.3.4) asserts a session is open.
The handle can be lost mid-session inside wx itself:
`wxWindowMSW::DoMoveWindow` writes a failed `DeferWindowPos`
result (null) back over the parent's live session handle
(window.cpp ~2123), and the `SetScrollbar` flush documented
in wx's own comment consumes a session out from under its
guard. During playback the embedded libVLC video window is
created and resized by libVLC's own thread when stream
dimensions change — foreign window activity of exactly the
kind that interleaves with these sessions. The app itself
issues no window moves during streaming (verified: no
set-size/move calls outside construction).

## Also in this release: fullscreen layout deferral

The audit for app-forced layout found exactly one manual
`Layout()` call: `set_fullscreen` ran layout and a focus
move synchronously inside the fullscreen mode switch, where
Python's `_set_fullscreen` defers the focus with
`wx.CallAfter`. The toggle now only changes state and
records `fullscreen_layout_pending`; `drain_fullscreen_layout`
runs the layout + focus on the next player tick (150 ms),
after the switch settles. End state unchanged. Audit
details: no `.fit(` calls anywhere; other dynamic show/hide
(the stream URL field) forces no layout or focus; dialog
construction uses `set_sizer` before first show throughout.

## Verification

Local gates (fmt, clippy `-D warnings`, workspace tests,
playlist parity) on the branch — this also rebuilds wx
without asserts and runs the full suite against that build.
The Release run's three-platform Gates for v0.4.19. The
dialog class cannot recur with the checks compiled out;
Brandon's streaming and fullscreen use on v0.4.19 is the
acceptance step.
