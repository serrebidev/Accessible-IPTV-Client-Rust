# 0068 — Startup load audit: m3u and EPG time-to-new-channels

Brandon (2026-11-04): "When I start the program, it takes a long
time to see the new channels. I would like this done as quickly
as possible without destroying the user experience." This audit
measured the whole startup path at his real scale — a 115,651
channel playlist and an 813,600-programme guide (2,400 guide
channels) — fixed what was actually slow, and verified the rest
was already fast. Benches live in
`crates/iptv-core/tests/startup_load_bench.rs` (ignored tests,
release mode, this loaded dev box; absolute numbers run faster
on a quiet machine, the ratios are the finding).

## The playlist path is already fast

Worker side, per phase at 115,651 channels / 120 groups:

- Read + parse a local playlist file: 0.52 s.
- Full merged load, cold (hash, parse, parsed-cache write,
  dedupe): 1.18 s; warm parsed-cache hit: 0.47 s; prefill model
  (what paints the channels first): 0.32 s.
- Model builders: `categories()` 5.7 ms, `category_rows` 0.36 ms,
  `NavIndex::build` 10.4 ms. The channel list is virtual; there
  is no per-row widget cost.
- The remaining time before those numbers start is the network
  fetch itself, outside the app's control. Verified from the
  vendored ureq 3.5.1 manifest: gzip is in its default features,
  so playlist downloads already negotiate compression where the
  server offers it. The 15-minute download cache means restarts
  inside that window skip the fetch entirely.

## The freeze: the guide suffix refresh on the UI tick

After new channels land, the app refreshes the now/next suffixes
on the channel rows synchronously on the UI tick. At startup
(with no guide imported yet this session) that takes the store
path: `EpgStore::all_now_next`. The old implementation scanned
every programme in a 31-day window — essentially the whole
813,600-row guide, with descriptions — and reduced it in Rust:

- `all_now_next`, old: 4.70 s, every suffix refresh.
- Two per-channel index dives measured in between: 2.97 s (the
  random-page cost on a cold cache eats the theoretical win on
  this hardware).

## The fix: port Python's `get_all_now_next` exactly

Python's bulk snapshot — the parity oracle — never scanned 31
days. It range-scans a start-leading covering index
(`idx_programmes_start_cover (start, end, channel_id, title)`)
bounded to start within [now − 24 h, now + 6 h] and end after
now ("a programme airing now cannot have started more than ~a
day ago"), reduces in memory (current: greatest start not after
now, first scanned wins a tie; next: first future row), and
fetches descriptions afterwards for the winning rows only. The
Rust store now does the same:

- New index `idx_programmes_start_cover (start, stop, channel,
  title)`, created in `migrate` alongside the existing indexes.
- `all_now_next` scans that index inside Python's window,
  reduces identically, then fetches each winning row's
  description by its unique (channel, start, stop) slot.
- `EpgGuide::bulk_now_next` (the in-memory twin) applies the
  same 24 h floor and 6 h horizon, so both guide surfaces answer
  identically instead of drifting by which path served them.

Result at 813,600 programmes: **`all_now_next` 4.70 s →
65.6 ms**, with an identical snapshot (2,400 channels with
current and next, spot-checked rows equal).

Behavior deltas vs the previous Rust store scan, all toward
Python: a programme that started more than 24 h ago no longer
counts as current even if still airing; "next" reaches 6 h out,
not 31 days; on an exact start tie the first (shortest) row
wins, as in Python, instead of the last. The row suffixes and
What's on Now — the two consumers — are exactly Python's
`get_all_now_next` consumers.

Regression coverage: `all_now_next_matches_python_window_semantics`
checks the store against an independently restated Python
oracle across 13 probe times plus crafted edge channels —
past-only, future-only, beyond-horizon, start ties for both
current and next, a 120-programme bulk channel, and the named
window edges (next at +5 h counts, +7 h does not; started
23 h ago is current, 25 h ago is not).

## Measured and left alone

- `GuideIndexes::build` over the store's 2,400 channels: ~0.8 s
  on this loaded box. It is cached in the app (rebuilt only when
  the guide's channel set changes), so it is paid once per guide
  change, not per refresh. The cost is string normalization
  (word-number expansion plus four canonical forms per channel);
  rewriting it risks matching behavior for a one-time sub-second
  cost. Not the complaint; left as is.
- A routine startup does not re-download the guide:
  `should_auto_import_epg` skips the import when the store has
  usable data, the sources hash is unchanged, and the last
  import is younger than the configured interval (default 6 h).
  The 0067 import numbers stand (fresh 155 s, refresh 65 s at
  672k programmes) for when an import does run.
- Cached row-guide formatting rides the E12 match cache
  (measured fast in the parity program); no change.

## Net effect at startup

Channels paint from the prefill in ~0.3–0.5 s of worker time
after the fetch, and the suffix refresh that used to freeze the
window for ~5 s now costs ~66 ms for the snapshot (plus the
one-time-per-guide index build). Nothing about the user
experience changes except that the stall is gone.
