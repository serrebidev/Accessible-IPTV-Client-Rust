# 0064 — Finish-all: the parity audit's remaining partials closed

Date: 2026-11-03. Branch: `agent/finish-parity`. Closes the items
validation 0063 recorded as partial or bounded: B19, E12, E10,
R10, R19, P18, S05, S10. Ordered by Brandon in the Rust lane:
"finish all the work."

## B19 — Background navigation index (built)

- New core module `crates/iptv-core/src/navindex.rs`: a direct
  port of Python's `playlist_index.py`. Stores positions, never
  channel copies: lowercased search names by position, channel
  positions per non-empty `playlist-id` scope, and group positions
  per scope in first-appearance order. Unscoped channels are not
  stored, exactly as Python rules (the no-scope accessors read
  the catalogue directly).
- `BrowserWorker` warms the index on a background thread after a
  model is applied (Python's `_warm_playlist_index_background`),
  installs it only for the exact model it was built from
  (`Arc::ptr_eq` against the published playlist or the pending
  replacement), and discards builds for superseded models.
  Projections use it when installed and fall back to the scan
  path while it is absent — Python's optional-index law.
- Consumers: `scoped_rows` (binary-search membership per row),
  `categories()` (the index's group table for the active scope),
  `favorite_count()` (index membership for favorites), and the
  search worker, which now receives the index alongside the
  model and scans its precomputed lowercase names instead of
  re-lowercasing every name per keystroke
  (`matching_scoped_indexed`).
- Regression tests: `navindex::tests::*` (scope positions and
  group tables equal a linear scan; normalized names) and
  `browser::tests::nav_index_projections_match_the_scan_path`
  (scoped categories, favorite count and search rows identical
  before and after the index installs).
- Scale measurement (500,000 channels, release build,
  `crates/iptv-core/tests/scale_bench.rs`, ignored by default):
  index build 0.28 s on its background thread; worker load of
  ten 50,000-channel sources plus first results 4.4–12.0 s
  across runs (parse-bound, varies with page cache); set-scope
  to a 50,000-row scope 166 ms end to end; category switch
  83 ms; a rare-match search 85 ms and an all-match search over
  the full catalogue 125 ms. A raw full-catalogue membership
  sweep is 37 ms through the index versus 16 ms for a pure
  linear scan — the index's value is the maintained per-scope
  and per-group structures (and matching Python), not a
  one-shot sweep.

## E12 — Scale benchmark (measured) + the match cache

Bulk guide presentation was algorithmic after the EPG batch
(one `bulk_now_next` snapshot + `build_row_guide`); what was
missing was the measurement — and the measurement exposed a
real defect: `refresh_epg_suffixes` re-matched **every**
channel against the guide on every refresh, which at this
scale costs ~40 s of matching on the UI thread, every minute.
Python does not do that: it caches channel→guide matches
(the program-match cache) and invalidates on playlist/guide
replacement. Ported: `epg::GuideMatchCache` (one match pass
per playlist+guide pair) + `row_guide_from_matches` (suffix
assembly from the fresh snapshot); the app keeps the cache
keyed by the model's playlist pointer and clears it whenever
a guide load lands. A core test proves the cached path's
output identical to the uncached build.

Measured (500,000 playlist channels, 30,000-channel /
240,000-programme guide, release build): guide indexes build
1.2 s; the bulk now/next snapshot 40–300 ms across runs;
one-time match-cache build ~40 s (the matcher is the ported
Python algorithm — tvg candidates, word-number expansion,
canonical name buckets — and this dataset's 470,000 unmatched
channels pay its full fallback price); the recurring refresh
after the cache exists: **68 ms**, down from ~38–47 s
uncached. Residual, recorded honestly: the one-time cache
build still runs on the refresh path (once per playlist or
guide change, not per tick); moving it to a background thread
is future work if a real 500k catalogue ever makes it felt.

## E10 — Exact Python panel behavior

A channel with no guide match now leaves the information panel
empty, which is what Python's description cache does; the
previous placeholder text ("No description available for this
programme.") is gone from that path. Channels matched to the
guide keep the full `epg_info_message` now/next rendering.

## R10 — Python's probe budget

The preferred-audio probe wait is now Python's 20 s (was a
deliberate 6 s bound). The probe's ffmpeg carries its own 10 s
`rw_timeout`, so a stalled probe thread stays bounded; the wait
only runs when a remembered audio preference exists with no
heard witness — the case where a short probe silently recorded
the wrong track.

## R19 — Scheduled Recordings context menu

The dialog's list now carries Python's right-click / Apps-key
menu: Refresh always; a separator, Cancel and Delete (with
Python's `Delete\tDel` label) when the list has rows. Selections
dispatch on the dialog frame to the same functions the buttons
and keyboard use — the Cancel button's logic was extracted to
`cancel_first_selected_job` so button and menu share one path.

## P18 — Engine release off the UI thread at exit

`release_engine_off_thread` runs in the main window's close
handler: it takes the state's engine and ops sender and drops
them on a detached thread. Dropping the sender hangs the ops
channel up, so the worker drains its queued ops, drops its own
engine clones on its thread and exits; the state's clone drops
on the cleanup thread. libvlc's final player/instance release
no longer runs on the UI thread while the app closes.

## S05 — Python's log-folder law

`applog::logs_dir` now implements Python's `get_logs_dir`
exactly: the config directory's `logs` folder on Windows, the
system temp directory's `logs` folder on every other platform.
Regression test: `applog::tests::logs_dir_follows_pythons_platform_law`.

## S10 — Menu-highlight F1 routing

The platform batch recorded that an open menu's highlighted item
could not be identified because wxdragon had no API for it.
That was wrong: wxdragon 0.9.24 exposes `on_menu_highlighted`
(wxEVT_MENU_HIGHLIGHT) with the item id. The main frame now
tracks highlights: when the highlighted item has a User Guide
topic (`menu_item_topic` covers the playlist/EPG managers,
What's on Now, Continue Watching, VOD, favorites, casting,
player, recordings, scheduled recordings, reminders, subtitle
speech, updates, keyboard shortcuts and the channel context
menu), it becomes the current help topic — the same slot focus
tracking uses — so F1 / the User Guide command opens at the
item the user is on instead of the last focused control.

## Gates

All green on the branch before the PR: `cargo fmt --all
--check`; `cargo clippy --workspace --all-targets -- -D
warnings` (exit 0); `cargo test --workspace` — 304 passed,
0 failed (299 at v0.4.6 + navindex unit tests + the browser
index-equivalence test + the log-folder law test; the E12
cache-equivalence assertion rides the existing bulk-guide
test); `python3 tools/check-playlist-parity.py` against the
pinned Python clone — PASS; `git diff --check` clean. The
scale benches are `#[ignore]`d so the normal suite stays
fast; their measured numbers are recorded above.

## Remaining owner evidence

Issues #86–#89 and #137 were closed on Brandon's approval of
2026-11-03 ("I approve finish all the work"), recorded on each
issue. No automated result in this document stands in for the
owner-heard NVDA evidence those issues describe; the closure
basis is the owner's decision, stated on the issues themselves.
