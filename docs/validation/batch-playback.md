# Playback batch — parity audit P01–P21

Implements the "Playback, audio, subtitles and history"
section of `docs/feature-parity-audit.md` against the pinned
Python client (v1.150.58). Each item below lists what
changed, the regression evidence, and any blocker or
deliberate divergence. Automated evidence only: nothing here
is owner-heard NVDA evidence, and this batch does not touch
the #86–#89 gate pack or ADR 0004 — the Python client
remains the shipping product.

## Gates (all pass)

- `cargo fmt --all` and `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace` — 208 tests, 0 failed (iptv-core
  184, iptv-app 10, iptv-vlc 4, integration 10)
- `python3 tools/check-playlist-parity.py
  ~/workspace/pyclone-pinned` — PASS
- `git diff --check`

## P01 — cache plan and recovery growth

`CachePlan` in `iptv-core/src/playback.rs`: the base cache
resets to the saved opening value on each new channel
choice, grows by 1 s per recovery restart up to the saved
maximum, and never grows for Dispatcharr live streams.
Loads take 500 ms on all four caches for live quick starts
(initial play and TS rollovers); recovery retries use the
current grown base. `iptv-vlc` `MediaOptions` maps the plan
onto `:network-caching`, `:live-caching`, `:file-caching`
and `:disc-caching`, plus `:http-reconnect=true`,
`:rtsp-tcp`, `:adaptive-use-access=1` when the adaptive
setting is on, `:clock-jitter=1500` for live and
`:clock-jitter=0`, `:clock-synchro=0`,
`:drop-late-frames=0`, `:skip-frames=0` for VOD.

Tests: `cache_plan_quick_starts_live_and_grows_the_base_on_recovery`,
`media_options_carry_the_cache_plan_to_all_four_caches`,
`buffer_bounds_match_python_including_the_18_migration`
(settings clamps, including the legacy 18 s maximum
migrating to 30 s).

## P02 — player close teardown

The player window's close handler now follows Python's
order: force a resume checkpoint, release the local live
buffer lease and any pending buffer start, cancel the
sleep timer, drop a pending reconnect, stop application
voice, stop the session machine, queue the VLC stop on the
transport worker (P18), stop the fallback control, and
hide the window.

Evidence: adapter wiring on the close path; no unit test
(a window lifecycle path). Live close behaviour is part of
Brandon's NVDA pass, not claimed here.

## P03 — freeze watchdog

`FreezeWatchdog` in core: while the session reports
Playing, VLC's decoded/displayed counters are sampled; any
counter change (including a reset or wrap) is progress.
Video arms only when video is visible and the stream is
not audio-only, after 2 progress samples; audio arms the
same way. No verdict inside the startup grace (12 s plus
the cache plan's base) or before 10 s without progress.
When libVLC reports no counters, a position-clock fallback
applies the same grace and threshold, rebaselining on
backwards timestamp jumps instead of calling them stalls.
A stall feeds the session's ordinary `Stalled` recovery —
it does not touch the rollover path (P04). The adapter
builds the watchdog per channel with
`audio_only_hint` (the playlist `radio` attribute wins;
media-type style attributes are the fallback).

Tests: `freeze_watchdog_detects_frozen_counters_after_grace`,
`freeze_watchdog_never_arms_video_for_radio_or_hidden_video`,
`freeze_watchdog_clock_fallback_rebaselines_on_timestamp_resets`,
`audio_only_hint_reads_the_radio_attribute_first`.

## P04 — Xtream TS segment rollover

`looks_like_live_ts` ports Python's detection (`.ts` path
with a numeric final stem or numeric segment, the
extensionless content patterns, and the
timeshift/movie/series/archive/catchup/recording
exclusions). On an ended or errored live TS stream that
has played at least once, `PlaybackSession::note_rollover`
takes the rollover path: a 200 ms refresh on a budget of 6
that is separate from the #128 reconnect budget (attempts
stay at 0), refilled whenever playback reaches Playing
again. A stream that never played is a failed open and
uses ordinary recovery. At the budget's end the session
gives up as `Lost`. Cache on rollover is the 500 ms quick
start (P01), and the adapter announces "Refreshing
stream…" once, from the rollover arm.

Tests: `live_ts_detection_matches_python`,
`rollovers_use_their_own_budget_and_refill_on_playing`.
#128 semantics are untouched: Playing only on engine
confirmation, the 30 s startup deadline is never extended
by retries, and the existing session tests
(`playing_is_reported_only_after_engine_confirmation`,
`startup_deadline_is_shared_and_retries_do_not_extend_it`,
`no_media_timeout_reconnects_then_the_deadline_loses`) all
still pass.

## P05 — HLS variant cap in the player path

`internal_player_variant_max_mbps` (default off; positive
values clamp to [0.25, 500]) is now applied on the
built-in play path: when a cap is set and the URL is an
HLS master, the adapter resolves it through the existing
buffer-path `resolve_hls_input` on a worker thread,
waiting at most 400 ms before falling back to the master
URL, exactly as Python probes. Buffer-owned URLs bypass
the probe (the buffer path applies its own resolution).
The parse/select law was extracted from `resolve_hls_input`
into the pure `select_hls_variant`: AVERAGE-BANDWIDTH
beats BANDWIDTH; the pick is the highest bandwidth at or
under the cap, else the smallest overshoot, else (no cap)
the highest overall.

Tests: `variant_cap_picks_the_highest_at_or_under_it`,
`variant_cap_falls_back_to_the_smallest_overshoot`,
`no_variant_cap_picks_the_highest_overall`,
`master_without_variants_is_an_error`, plus the settings
clamp in `buffer_bounds_match_python_including_the_18_migration`.
Note: the resolver itself is the pre-existing buffer-slice
implementation, reused unchanged in behaviour.

## P06 — per-channel audio track memory

Core `audiotracks.rs`: `audio_memory_key` is the first
non-empty of name, tvg-name, tvg-id, trimmed and
casefolded (never URL or provider data, so memory survives
URL reissue and is shared across playlists).
`remember_channel_track` writes the remembered name and
slot together, validates the slot against 0..64, and caps
each map at 500 entries. Only manual picks persist: the
adapter's manual choice path writes both maps and
`last_audio_track` in one config save; automatic preference
application never writes memory. Each play supplies the
remembered name and slot for the new channel's key (empty
when none, so the previous channel cannot leak), and the
existing preference order (channel memory, then audio
description when preferred, then global last track, then
saved keywords) plus the slot fallback for renamed tracks
applies them.

Tests: `memory_key_is_first_nonempty_name_casefolded`,
`remembered_tracks_write_name_and_slot_together`
(eviction is by key rather than strict insertion order —
the one approximation; the maps are keyed identically, so
only which entry is evicted at the 500 cap can differ).

## P07 — late-published audio tracks

The adapter no longer treats the first empty track list as
final: while Playing, track publication is retried on
every poll, counting attempts in 500 ms units against the
budget of 20 (≈10 s, Python's `_max_audio_preference_attempts`),
settling early when a preference or slot match applies.
The choice and menu republish whenever the published id
list changes, including after settling, so controls fill
in whenever the list appears.

Evidence: adapter wiring over the P06 core selection
(`selection_prefers_keywords_then_ad_last` covers the
match law). No dedicated unit test — the budget lives in
the poll loop.

## P08 — embedded subtitle tracks and sidecar slaves

`iptv-vlc` gains the libVLC subtitle surface, all optional
symbols (absent libVLC builds still load): track
descriptions in order, the current SPU, `set_spu` (-1 is
Off), and `add_slave` for subtitle sidecars with
select=true. The adapter adds a Subtitles submenu to the
Playback menu, rebuilt on every refresh: radio items
`Off` plus each embedded track with id >= 0 (blank names
show as "Track {id}"), checked at the live SPU. Selecting
a track sets it in the engine; selecting an embedded track
discards an external file's cues (Python's rule); selecting
Off keeps shown review text. The `S` shortcut cycles
Off → embedded tracks → Off. Embedded tracks are
render-only, as in Python — libVLC never returns their
text, so they are never spoken or reviewable; sidecars are
both rendered (slave) and parsed for speech (P10).

Tests: `engine_load_is_graceful_without_libvlc` (the API
surface loads with no libVLC present); menu/cycle wiring
is adapter code, exercised by the gates above.

## P09 — subtitle load guards

Loading a sidecar while the local live buffer owns the
stream is refused up front with Python's message ("External
subtitle files are not synchronized with the local live
buffer. Use the stream's embedded subtitles.") — no dialog,
no slave, no parse. Otherwise the slave attaches first;
parsing runs on a worker thread and delivers through a
generation slot: every play, stop, embedded-track select,
and new file load bumps the generation, and stale results
are dropped silently. Empty or unparseable files install
empty cues under the new generation and report "No subtitle
text found in this file." at priority 1.

Tests: the cue parsers are pinned by `srt_and_vtt_blocks_parse`
and `ass_dialogue_uses_declared_format`; the generation
guard is adapter wiring.

## P10 — subtitle sampling at 150 ms

A dedicated 150 ms timer on the player frame samples cues
(the 500 ms/1 s status polling is unchanged). Effective
cue time is the media position minus the engine's SPU
delay (`iptv-vlc::subtitle_delay_ms`, `libvlc_video_get_spu_delay`
in microseconds, converted to ms) — media clock only,
never wall-clock extrapolation. The sidecar's VLC track is
adopted once `add_slave`'s asynchronous selection binds;
if the user then switches to a different track, the
file's cues are discarded. Label and speech fire only on
cue-index change, so a cue is never re-spoken.

Tests: `active_lookup_prefers_latest_start` (the sampling
lookup), `srt_and_vtt_blocks_parse`; the timer and delay
wiring are adapter/vlc code.

## P11 — subtitle review cursor and recent list

`ReviewCursor` in core `subtitles.rs`: sampling maintains
the active cue and the latest-started cue; the review
cursor follows new active cues, clamps to the latest shown
cue, and survives gaps. Review reads, with Python's exact
strings: no cues → "No subtitle text to read. Load a
supported subtitle file."; nothing shown yet → "No subtitle
has been shown yet."; current in a gap → "Last subtitle:
{text}"; stepping past either end of *shown* history →
"No earlier subtitle." / "No later subtitle." with the
cursor unmoved. Review never reads file cues beyond the
latest shown one. The Recent Subtitles dialog lists the
cues actually delivered (newest first, numbered, last 20
of the 50-cue shown history); empty history reports "No
recent subtitles."

Tests: `review_cursor_walks_shown_history_only`,
`review_cursor_retains_the_last_shown_cue_through_gaps`.

## P12 — review speech through the shared route

All player review speech (Read Current Subtitle and the
Previous/Next commands, from both the menu and the keys)
goes through `review_subtitle`, which speaks via
`speak_cue_text` — the #131 shared route with exactly one
backend (screen reader or app voice per the subtitle
speech settings). Nothing in the review path speaks to the
live region directly, and review is not gated by the
automatic-speech toggle or pause state, matching Python's
`speak_review`.

Tests: the route's law is pinned by the existing #131 core
tests, notably `exactly_one_backend_per_cue_and_preview_suppresses`
and `backend_parsing_defaults_to_screen_reader`.

## P13 — player shortcut parity

Python's matching order is restored in
`dispatch_player_key`: a configured binding (the
`shortcuts` config's `player.*` overrides over the core
defaults) matches first. Only when nothing matches do the
fallbacks run — Ctrl+Up/Ctrl+Down synthesise the volume
action when its binding is still the plain arrow key, and
Esc leaves fullscreen (P19). Volume steps are 2, or 5 when
the event carries Ctrl, for both the synthesis path and a
direct Ctrl+arrow binding. The `A` action cycles audio
tracks (wrapping) instead of focusing the choice control;
`S` cycles embedded subtitle tracks (P08); no Space
binding exists, as in Python. Arrows inside the focused
track Choice keep native Choice behaviour because the
frame's key handler only sees keys the focused control
does not consume (wx behaviour; stated here, not unit
tested).

Tests: core shortcut law — `parse_canonicalizes_and_validates`,
`validity_law_and_overrides`, `set_shortcut_errors_and_reset`.

## P14 — manual stop cancels the sleep timer

`player_stop` (the manual stop path) cancels the sleep
timer first: the deadline clears, any fade in progress is
undone by restoring the applied volume from the stored
user volume (the fade never changes the stored value), and
the buffer lease is released (Python's manual-stop buffer
close). The sleep deadline's own expiry path already
cleared its state before stopping; the two paths do not
double-restore.

Evidence: adapter wiring; the fade arithmetic is unchanged
from the sleep-timer slice.

## P15 — VOD resume dialog

Before a non-live channel launches, the outgoing stream is
force-checkpointed, then the channel's `playback_bookmarks`
record (P21) is looked up. With an entry, `choose_resume`
asks "Resume playback from H:MM:SS?" (hours unpadded,
minutes/seconds two-digit) with Resume, Play from
Beginning (the default, as Python's `NO_DEFAULT` makes
it), and Cancel. Resume starts at the saved position;
Beginning deletes the record and saves before playback;
Cancel aborts the launch entirely — the current stream
keeps playing and the bookmark is untouched.

Tests: the record side is pinned by the P21 bookmark
tests; the dialog is adapter wiring.

## P16 — resume seek protection

`ResumeGuard` in core: armed at play time from the
dialog-approved resume position. While the engine's
position is more than 2 s short of the target, checkpoints
return without writing — the saved bookmark is neither
overwritten nor deleted. Reaching within 2 s of the target
(or past it) confirms the seek and checkpointing proceeds
in the same call. If 60 s pass unconfirmed, the guard
reports "The saved playback position could not be
restored." once, re-anchors the 30 s checkpoint throttle,
and normal checkpoint rules resume. Checkpoints only run
for VOD with a reached position, are throttled to one per
30 s unforced, and are forced on stop, prepare, and close.

Test: `resume_guard_protects_until_the_seek_is_confirmed`.

## P17 — recent channels history

History is now the persisted `recent_channels` key list
(not a session-only list): `remember_recent` in core
`favorites.rs` dedupes by the stable `channel_key`
(provider fields, then tvg-id, then display name — never
the URL), promotes the key to the front, and caps at 20,
so A/B/A yields exactly [A, B]. Recording happens only for
live channels and only on a confirmed start: the built-in
route records once when the session reaches Playing, the
external route only when the process actually spawned.
Stored keys resolve against the current catalogue for the
Recent dialog and Previous Channel, so renewed URLs play
the fresh entry; unmatched keys stay persisted but are
skipped. Previous Channel plays the second resolved entry
or reports "No previous channel is available."

Test: `recent_history_is_a_deduped_twenty_entry_mru`.

## P18 — serialized VLC transport

All VLC loads and stops run on one background worker
(`VlcOp` channel): a load executes off the UI thread and
reports through a generation-tagged reply slot; stops are
ordered after queued loads, so a superseded stream's
connection closes before the new one opens. Superseded
generations are dropped on delivery. An initial load that
fails falls back to the basic player with an explanatory
status, as before; a failed retry feeds the session's
error path. `VlcEngine` is a shared `Arc` handle with
`&self` transport methods, which is what lets the worker
and the UI share one engine safely.

Tests: `engine_load_is_graceful_without_libvlc`; the
ordering is adapter wiring over the channel.

## P19 — menu shortcut labels and fullscreen

Playback-menu labels now carry the effective (remapped)
shortcut for Play/Pause, Stop, Record, the Audio Track and
Subtitles submenus, Speak Subtitles, Read Current
Subtitle, Previous/Next Subtitle, Cast…, Toggle Full
Screen, What Is Playing, Hide Window, and Exit Player;
items Python leaves unlabelled (Sleep Timer…, Local Live
Buffer…, Rewind Live TV…, Return to Live, Load Subtitle
File…) stay unlabelled, and Audio Output Device… keeps
its literal `D`. Fullscreen goes through one
`set_fullscreen` helper: no-op when already in the target
state, the whole controls panel hides (the window was
restructured into a video panel plus a controls panel to
make this possible), the button's label and accessible
name follow ("Full Screen" / "Exit Full Screen"; the
construction name is "Toggle Full Screen", as in Python),
and focus moves to the video surface. F11 toggles both
ways through the shortcut map; Esc exits only when no
binding matched (P13).

Evidence: adapter wiring; label text follows the same
effective-bindings source as P13.

## P20 — monotonic elapsed time

`now_ms()` is monotonic milliseconds since process start
(a `OnceLock<Instant>`), so the launch debounce, session
deadlines, reconnect due times, watchdog grace, and sleep
timer all measure elapsed time that a wall-clock step
cannot wedge or skip. The sleep timer's deadline, fade
window, and remaining-minutes display were converted to
the same clock. The one wall-clock value kept is the
bookmarks `updated` stamp (`now_epoch()`), which Python
also stores as epoch seconds for display/sort only — it
drives no deadline.

Tests: the session timing law is pinned by
`startup_deadline_is_shared_and_retries_do_not_extend_it`,
`reconnect_backoff_matches_python`, and
`launch_gate_debounces_same_stream_only`.

## P21 — Python `playback_bookmarks` consumption

New core module `bookmarks.rs`, a line-for-line port of
Python's `playback_resume.py`. Keys are SHA-256 digests of
Python `json.dumps` output, reproduced byte-exactly
(separators, key order, and string escaping, including raw
UTF-8 for non-ASCII): Xtream items key on the account-scoped
locator (source hash from provider/playlist id, type,
numeric id, series for episodes), plain-playlist items on
playlist scope plus exact URL, with the legacy
provider-plus-URL key migrated to the stable key on lookup
when safe (the newer `updated` wins when both exist). The
Rust client's old `resume_positions` key is no longer read
or written; it is left untouched in existing configs.
Records normalize to Python's law (strict integer types;
30 s <= position < length <= 7 days; remaining beyond
max(30 s, length/20) or the record counts as finished;
200-entry cap ordered by recency) and checkpoint writes
delete inside the same bounds. Metadata persists only a
cleaned title (no URLs, no control characters, 240 chars)
and the validated locator — never a stream URL or
credentials. Xtream VOD channels now carry Python's
`provider-type` / `provider-id` / `resume-type` /
`resume-id` attributes (from `vod.rs`), which is what
makes locator keying possible. Continue Watching resolves
rows through `find_current`: exactly one unambiguous
current item, duplicates with differing identity make the
row unavailable, and no saved URL is ever used.

Tests (digests generated from the Python oracle itself):
`keys_match_the_python_digests`,
`live_channels_have_no_bookmark_key`,
`normalize_drops_finished_short_and_malformed_records`,
`checkpoint_writes_and_removes_by_the_python_bounds`,
`lookup_migrates_a_legacy_record_to_the_stable_key`,
`find_current_requires_one_unambiguous_item`.

Divergence note: whether a channel counts as a movie for
legacy keying is decided by the request classifier (URL
shape), where Python uses its catalogue `kind`. The two
agree on the catalogue shapes both clients produce; the
stable Xtream and playlist keys — the ones current Python
writes — do not depend on it.

## Blockers and leftovers

- None of P01–P21 is blocked. Everything above is
  implemented and covered by the gates listed.
- Owner-heard verification (NVDA behaviour of the player
  window, menus, dialogs, and speech) remains Brandon's
  pass, together with the #86–#89 gate pack; this document
  claims automated evidence only.
- The engine's final release on application exit still
  happens on the UI thread (the engine handle is created
  once and never replaced), where Python defers frame
  destruction until its background teardown joins. Loads
  and stops — the operations that can stall — are all on
  the worker (P18).
