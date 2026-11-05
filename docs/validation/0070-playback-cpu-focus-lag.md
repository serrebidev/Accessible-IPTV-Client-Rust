# 0070 — Playback high CPU and 5–10 s alt-tab focus lag

Brandon (2026-11-04): "When I'm playing something it is high
CPU. When I alt tab into the window it takes 5-10 seconds to
focus and work." Audit at his real scale (115,651 playlist
channels, 813,600 programmes in 2,400 guide channels; bench in
`crates/iptv-core/tests/startup_load_bench.rs`, release mode,
this loaded dev box — absolute numbers run faster on a quiet
machine, the ratios and the thread each cost lands on are the
finding).

## Where the time went before the suffix-worker fix

The synchronous costs below were paid by the old
`refresh_epg_suffixes` path on the UI thread, so the window
could not process focus, paint, or input until each one
finished:

- `GuideMatchCache::build` over the full playlist —
  **25.3 s** worst case (guide ids/names disjoint from the
  playlist, so every one of the 115,651 channels pays the full
  fuzzy match: tvg-id candidate expansion, word-number
  replacement, four name canonicalizations per name source).
  This runs inside the suffix refresh whenever the playlist
  pointer changed since the last refresh, and after every
  guide load (the delivery clears the match cache), so it
  recurs during ordinary playback sessions: a provider
  delivery, a search, or a favorite toggle republishes the
  model, and the next 50 ms tick pays the rebuild before the
  window answers again.
- `EpgStore::all_now_next` mid-window — **3.20 s** in a cold
  process (OS page cache cold on the 178 MB guide database),
  against 65.6 ms warm in validation 0068. The variance is
  itself the bug on the UI thread: right after alt-tabbing
  back is exactly when caches are cold.
- The steady-state part was never the problem: bulk now/next
  for the in-memory guide is 7 ms and the row build is
  sub-millisecond to a few ms. The per-minute timer also
  repainted the whole channel list (`refresh_items` over up to
  115k rows) even when no suffix had changed.

## The parity gap

Python — the behavior oracle — solved this exact problem in
its issue #109 (`responsiveness.WorkGate`): `_refresh_now_
playing_labels` runs on a background thread, throttled to
30 s, reads the SQLite guide database read-only, defers while
the user is interacting (retrying 1 s later), drops superseded
or stale results at gate checkpoints, and publishes only when
the label map actually changed. The Rust port kept the data
but dropped the architecture: the same rebuild ran on the UI
thread, every minute, plus after every model republish, with
an unconditional full-list repaint.

## The fix

The architecture now matches Python's: the suffix rebuild
runs on a worker thread, and the UI thread only swaps a
finished map in. Guide loading already uses `EpgWorker` for
the merge and `GuideIndexes::build`; those operations finish
before the load result reaches `poll_epg`.

- Core `SuffixWorker` (`epg.rs`): a background thread that
  owns the whole rebuild — open the durable EPG store (WAL
  mode, so a second connection is safe), build `GuideIndexes`
  and the playlist-wide `GuideMatchCache`, take the bulk
  now/next snapshot from the store, and produce the row
  suffix map. It caches the store connection, the indexes
  (keyed by a guide generation counter), and the match cache
  (keyed by playlist pointer + guide generation), so the
  steady-state job skips every rebuild whose inputs did not
  change. Store-only reads, exactly like the old refresh —
  suffix content is unchanged.
- App side (`main.rs`): `refresh_epg_suffixes` and
  `cached_row_guide` are gone. `request_suffix_refresh`
  dispatches a job unless one is already pending or the
  browser reports the user is busy (the WorkGate deferral —
  the 50 ms / 60 s timers simply retry on their next tick);
  `drain_suffix_results` applies a result only when its
  generation and playlist pointer are still current
  (superseded results are dropped, as in Python), and
  repaints the channel list only when the suffix map actually
  changed. The per-minute timer is now just drain + request.
  The `epg_match_cache` field is deleted; a guide delivery
  bumps the guide generation and zeroes the suffix stamp
  instead of clearing a cache the UI thread would rebuild.
- `EpgGuide::merge` (`epg.rs`): channel positions are
  resolved through one id map instead of a linear find per
  channel, and the per-channel programme merge
  (`merge_programmes`) takes a slot-map fast path when no
  incoming row overlaps a stored or sibling row outside its
  exact slot — exact slots absorb with the established field
  rule (changed title takes the new title and description;
  same title keeps the longer description), the rest append,
  one final sort. Overlap detection is exact (a prefix-max
  stop scan for spanning stored rows, plus rows starting
  inside the probe's span, exact partner exempt; a filtered
  later-row scan within the incoming list). Any real crossing
  — a corrected slot — falls back to the sequential
  `upsert_programme` path for that channel, so crossing
  corrections keep their exact reference behavior. A unit
  test pins the fast path against a reference copy of the
  sequential upserts on eight crafted cases, including both
  orders of exact partner + crossing sibling, a spanning
  stored row, an incoming-internal crossing, and a same-slot
  duplicate chain.
- `EpgWorker` builds the merged guide and its `GuideIndexes`
  before sending the load result. `poll_epg` swaps the finished
  snapshot and updates status; it does not merge programmes or
  build those indexes on the UI thread.

## Remaining cold-store path

Before a loaded in-memory snapshot is available, the app still
has synchronous SQLite fallback calls. In particular,
`store_indexes` reads guide channels and builds indexes on the
UI thread; `guide_now_next`, `guide_snapshot`, programme
queries, and EPG search also read the store from the adapter.
The cold persisted-store path is tracked separately in
`docs/specs/0007-cold-epg-reads.md`.

## Measurements after the fix

Same bench, same loaded box:

- The 25.3 s match-cache build, the cold 3.2 s `all_now_next`,
  and the suffix row build now run on the `SuffixWorker`
  thread. The UI-thread cost of a suffix refresh is the map
  swap plus, only when a suffix changed, one list repaint —
  the measured warm row build the swap replaces was 7.9 ms,
  and the repaint is skipped entirely when nothing changed.
- Single-channel diagnostic (200,000 contiguous slots merged
  into themselves): 0.20 s on the slot-map path. The
  sequential upsert is O(n²) retain scans on the same input
  (~40 billion row visits — minutes), so the fast path is
  confirmed active, not silently falling back.
- Full-guide bench (813,600 programmes into an existing
  guide): 1.38 s wall on this box while it was running
  several other builds (every other bench figure in the same
  run came in 2–5× slower than the quiet-box numbers in
  0068); the merge no longer scales quadratically, so a
  real-world delivery merge is proportional to the incoming
  guide size.
- A `SuffixWorker` unit test builds suffixes from a temp
  store end to end (BBC Two fixture: "News Hour" now,
  "Next: Quiz") and echoes the job generations.

## Gates

`cargo fmt --all -- --check`, `cargo clippy --workspace
--all-targets -- -D warnings`, `cargo test --workspace`
(287 core lib tests, including the two new ones), the
playlist parity check against the pinned Python oracle, and
`git diff --check` — all green.
