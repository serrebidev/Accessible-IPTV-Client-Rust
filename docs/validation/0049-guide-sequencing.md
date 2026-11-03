# 0049 — Startup load sequencing: playlist first, guide off the UI thread

Date: 2026-11-02. Follows 0048.

The owner's NVDA log from his v0.3.2 session showed the app
up, the playlist entry present in the Playlist Manager, and
the channel list announcing "No playlist loaded. Channel
list is empty." with 0 items. Given time, the playlist did
load. Three causes, all about ordering and where work ran:

1. At startup the playlist (28 MB) and the guide (263 MB,
   813k programmes) downloaded simultaneously on separate
   workers, racing for the same bandwidth and CPU, so
   time-to-channels stretched to minutes.
2. When the guide finally landed, poll_epg mirrored it into
   the SQLite store synchronously on the UI tick — a
   seconds-long freeze of the whole window at that scale.
3. While any load was in flight, the channel list kept its
   initial "No playlist loaded" accessibility text, which
   reads as failure when it is really just slowness.

Fixes:
- Startup order swapped (sources before guide) and
  reload_epg now defers whenever the browser is busy: it
  marks epg_deferred and the completion tick dispatches the
  guide load the moment the browser settles. Provider guide
  URLs and Import EPG go through the same dispatch helper.
- The EpgWorker takes an optional store path and mirrors
  the merged guide into SQLite on its own thread before
  delivering the result (the failure-note strings are
  unchanged and still reach the status line). The adapter
  no longer mirrors on the tick.
- While a load is in flight and the shown model is still
  empty, the channel list's accessibility description says
  "Loading playlist..." until results publish.

Regression evidence: a new core test drives EpgWorker with
a store path and asserts the guide arrives with an empty
note and the store holds the channel and programme
(worker_mirrors_the_guide_to_the_store_on_its_own_thread).
The deferral itself is adapter wiring (no widget tests in
this workspace, per the established pattern).

Evidence class: automated — fmt clean, clippy zero
warnings (-D warnings), workspace tests pass, playlist
parity PASS. Owner-side timing on his 115k-channel setup
is the closing check; ships as v0.3.4.
