# 0072: Second-round audit batch 1 — N07, N08, N10

The first three open findings of the second-round audit
(docs/feature-parity-audit.md), taken in the audit's suggested
order. All decision logic lives in wx-free core code with unit
tests; the adapter only gathers live state and shows the boxes.

## N07: exit warns about work it would lose

Python asks before exiting while reminders are pending
(`planning_ui.py` `_confirm_reminder_exit`), while DVR jobs are
scheduled (`main.py` `on_close`, listing each title with its
local window, with different wording when a recording is in
progress), and while a download runs — each box can veto the
exit. The tray-hide path asks nothing, because the app and its
work stay alive; an explicit Exit (File menu, tray Exit) runs
the checks even with Minimize to System Tray on (Python's
`_exit_requested`).

Rust: new core module `iptv-core/src/exit_guard.rs` —
`exit_prompts(reminders, jobs, recording_active,
download_active, now)` returns the prompts in Python's order
(reminders, recordings, download) with Python's wording; the
reminder box is marked `no_default` (Python's `wx.NO_DEFAULT`).
The upcoming-jobs set is Python's `scheduled` plus the
Rust-only `waiting` state (issue #134: a job held by a
connection conflict has not started and would be lost the same
way). The window label is Python's `_schedule_window_label`
(local `YYYY-MM-DD HH:MM - YYYY-MM-DD HH:MM`).

Adapter: `Wiring.exit_requested` is set by the File > Exit and
tray Exit handlers before `frame.close(false)`; the close
handler hides to the tray only for a plain close with the
minimize option on, otherwise shows each prompt and vetoes on
the first non-Yes answer (resetting `exit_requested`, as
Python does).

Tests: 4 core tests — pending-reminder counting (only
scheduled/ready and not past end), prompt order and wording,
the active-recording wording variant, and the empty case
(a running recording alone prompts nothing, as in Python).

## N08: playlists refresh on their own every 3 hours

Python's refresh timer reloads the playlists every 3 hours
(`main.py:2218`, `start_refresh_timer` / `on_timer_refresh` →
`start_playlist_load`). The Rust housekeeping tick (1 s,
always-on) now tracks the last playlist refresh and calls the
existing `reload_sources` full cycle when 3 hours have elapsed
since startup or the last refresh — the same path a manual
refresh takes, so focus and selection behave exactly as they
do for a manual refresh.

## N10: stream headers and options reach libVLC

Python's `_apply_stream_headers` sends more than the user
agent and referrer the Rust engine sent before: the cookie via
`:http-cookie`, and Origin, Authorization, Accept,
X-Forwarded-For, Range and custom headers via `:http-header=`
lines — with Python's browser-like default user agent
(Chrome/118 string) and default Accept when the stream names
neither. `_apply_cache_options` also always sends `:rtsp-tcp`
and `:http-proxy` when a system proxy is detected
(`_detect_system_http_proxy`, urllib `getproxies`), and a VOD
resume passes `:start-time` at load instead of only seeking
after Playing.

Rust: `iptv-vlc::header_option_strings` builds the option list
in Python's order (agent, referrer, cookie, Origin,
Authorization, Accept, X-Forwarded-For, Range, extras) and
`play_url` applies it; the crate doc comment that claimed
libVLC has no generic header option is corrected.
`MediaOptions` gains `proxy` and `start_time_seconds`, and
always emits `:rtsp-tcp`. Core gains
`DEFAULT_PLAYER_USER_AGENT` / `DEFAULT_PLAYER_ACCEPT` (the
exact Python strings) and `system_http_proxy()`: environment
variables first (all platforms), then the Windows Internet
Settings registry (`ProxyEnable` + `ProxyServer`, parsed by
the tested `parse_registry_proxy`, including per-protocol
`http=` lists), detected once per process. The adapter's
`vlc_media_options` fills both fields; `:start-time` applies
to non-live loads with a resume position, and the existing
post-Playing resume seek still runs as before (same target).

One deliberate difference: the post-Playing seek is kept, not
removed — it also covers the fallback engine, which has no
`:start-time`. On the VLC path both mechanisms aim at the same
position.

Tests: vlc crate — header option strings for bare and full
header sets (defaults filled, order exact), proxy/start-time/
rtsp-tcp option emission and absence. Core — registry proxy
parsing (plain, per-protocol, already-schemed, absent).

## Gates

`cargo fmt --all -- --check`, `cargo clippy --workspace
--all-targets -- -D warnings`, `cargo test --workspace`
(330 tests: core 292, app 18, vlc 9, integration 11) and the
playlist parity check against the pinned Python clone all
pass.
Not run: a live capture against a header-protected provider
channel (the audit's check) — no such channel is available to
this machine; the option strings are unit-tested against
Python's construction instead. Owner-heard NVDA acceptance of
the exit boxes remains Brandon's, as with every round.
