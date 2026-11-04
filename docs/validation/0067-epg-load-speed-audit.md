# 0067 — EPG load-speed audit: Rust client vs Python client

Date: 2026-11-03. Branch: `agent/epg-load-speed`. Trigger: Brandon's
report that the Rust client's EPG takes a very long time to load
compared with the Python client, with his direction that everything
be Rust code except wxDragon-class bindings.

## Method

One generated fixture through both implementations, same machine,
runs serialized: 2,000 guide channels × 336 programmes = 672,000
programmes, ~128 MB of XMLTV, 30-minute slots over 7 days from
2026-11-02, varied realistic titles. Python side:
`EPGDatabase(db_path).import_epg_xml([file])` from the pinned
Python oracle. Rust side: `crates/iptv-core/tests/epg_load_bench.rs`
(parse via `parse_xmltv_checked`, store via
`EpgStore::import_source`).

## Results

Python (single figures; its import call parses and writes together):

- open fresh DB: 0.28 s
- import fresh (parse + write): 289.90 s
- open populated DB: 0.08 s
- import refresh (same data): 349.79 s

Rust before the fix:

- parse: 12.06 s
- store open fresh: 0.92 s
- import fresh: 773.29 s
- store open populated: 46.82 s
- import refresh: 180.51 s
- GuideIndexes::build: 1.77 s

Rust after the fix:

- parse: 12.28 s
- store open fresh: 0.25 s
- import fresh: 154.65 s (parse + import total 166.9 s vs Python 289.9 s)
- store open populated: 0.04 s
- import refresh: 65.18 s (vs Python 349.79 s)
- GuideIndexes::build: 0.40 s
- GuideMatchCache::build: 2,000 playlist channels all matched in
  under 1 ms; 50,000 channels in 95 ms — guide matching was never
  the problem at any realistic scale.

## Root causes found

1. The Rust store opened its SQLite database with near-default
   settings (about 2 MB page cache, rollback journal defaults)
   while Python's store applies an import pragma set
   (`PRAGMA_IMPORT` in the Python playlist module): WAL,
   synchronous NORMAL, 128 MB cache, 256 MB mmap, MEMORY temp
   store, autocheckpoint off, 20 s busy timeout. Under those
   settings Python writes the same rows several times faster.
2. `import_source` wrote all 672,000 programmes in ONE
   transaction. Python commits in batches of 15,000 programmes
   (`EPG_IMPORT_BATCH`) with a passive WAL checkpoint per batch.
3. `migrate()` ran an unconditional whole-table dedupe DELETE on
   EVERY store open — a full scan plus index rebuild over 672,000
   rows, which is where the 46.8 s populated-open went. Python
   gates its equivalent repairs behind version stamps for exactly
   this reason.

Per-programme write shape (ranged overlap DELETE plus the
title/description-aware UPSERT) is a faithful port of Python's and
was not the divergence; parsing was already 12 s.

## Fix (crates/iptv-core/src/epgstore.rs)

- `open()` applies Python's pragma list right after connect
  (auto_vacuum first, per-statement failures ignored, matching
  Python's tolerant application) and raises busy_timeout to 20 s.
- `migrate()` runs the legacy dedupe only when
  `idx_programmes_slot` is absent (the pre-index databases that
  need it); current databases skip straight to index creation.
- `import_source` inserts channels in their own transaction, then
  flushes programmes in batches of 15,000 with a commit and a
  passive WAL checkpoint per batch; the source cleanup and prune
  finish in a final transaction. Import semantics unchanged — all
  25 EPG unit/integration tests pass, including the dedupe,
  overlap-replacement, and refresh cases.

## Pure-Rust question (Brandon's direction)

Everything on the EPG hot path is Rust code: quick-xml parsing,
ureq fetch, flate2 gzip. The store engine is SQLite through
rusqlite's bundled build — C code in the same binding category as
wxDragon and libVLC, and it is Python's own storage design, now
tuned to Python's settings. With those settings the Rust client
beats the Python client on every phase measured, so no storage
swap is needed for speed. Replacing SQLite with a pure-Rust store
(redb/sled class) remains possible as its own project; it would
change the on-disk format and lose the Python file compatibility
the port has kept. Left as Brandon's call, not done here.

Known remaining delta: the Rust store keeps a title index
(`idx_programmes_title`, backing guide title search) that Python
does not have; its maintenance is part of the remaining write
cost and is paid for a feature Python lacks.
