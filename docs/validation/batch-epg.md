# EPG batch validation (audit E01–E12)

Branch `agent/parity-epg`, against `docs/feature-parity-audit.md`
section "EPG and programme browsing". Python oracle:
`~/workspace/pyclone-pinned` (behaviour shape; the audit's line
evidence against Python v1.150.58 governs where they differ).

Automated evidence only. The GUI behaviours below were verified
by code inspection and core regression tests; nothing here is
owner-heard NVDA evidence, and issues #86–#89 stay exactly as
blocked as they were (ADR 0004 preserved).

## E01 — Python EPG source settings are read

- Core: `config::Config::normalize` coerces the `epgs` key to a
  string list like `epg_urls`, so an imported Python backup's
  guide list loads in shape and round-trips; `Config::f64_or`
  added for the freshness settings. New core helpers in
  `epg.rs`: `merged_epg_sources` (Python `reload_epg_sources`:
  own + provider + playlist-when-allowed, stripped-text dedupe),
  `source_key` and `hash_epg_sources` (SHA-1; hand-written
  `sha1_hex`, byte-compatible with Python's `hashlib.sha1`,
  because the shared config/database format depends on it).
- Adapter: new `effective_epg_sources` (own `epgs` ∪ legacy
  `epg_urls`, merged with provider-discovered sources);
  provider loads append to `epg_extra_sources` and load the
  merged list; the EPG manager and Import EPG read/write `epgs`
  and mirror to `epg_urls` so neither spelling resurrects a
  removed source.
- Tests: `config::tests::python_epgs_key_is_normalized_and_round_trips`,
  `epg::tests::sha1_matches_python_hashlib`,
  `epg::tests::merged_sources_dedupe_and_respect_playlist_opt_out`.

## E02 — Saved guide database is reopened for reading

- Adapter: `bind_browser` opens `epg.db` at startup into
  `Wiring.epg_store` (was initialised to `None` and never
  assigned). Store-only reads: `refresh_epg_suffixes` and What's
  on Now fall back to cached store indexes (`store_indexes`) and
  `EpgStore::all_now_next`; `store_programmes` window widened to
  seven days back through seven days ahead, uncapped (callers
  apply their own windows; the Catch-up dialog depends on the
  lookback).
- Core: `EpgStore::open` migrates older databases (adds
  `norm_name`/`last_seen`, backfills, dedupes slots, unique slot
  index) with WAL + busy timeout.
- Test: `epgstore::tests::usable_data_and_channels_with_show`
  (reopen-from-disk path is the same `EpgStore::open` the worker
  test round-trips through a real file).

## E03 — EPG enable and refresh settings drive behaviour

- Core: `should_auto_import_epg` ports Python's decision
  (`_should_auto_import_epg` + `_get_epg_auto_interval_seconds`):
  interval ≤ 0 always; source hash changed; guide not usable;
  last-import stamp missing or implausibly future; otherwise
  elapsed ≥ interval. Store gains `has_usable_data(now)`
  (programme joined to a channel ending after now) and
  `has_programmes`.
- Adapter: `dispatch_epg_load_mode` refuses loads when
  `epg_enabled` is false (forced loads included — disabling the
  guide means no guide work, matching Python's
  `start_epg_import_background` gate) and applies the freshness
  decision to automatic loads; successful loads persist
  `epg_last_import_epoch` and `epg_last_sources_hash`; a load
  requested while one runs sets `epg_recheck`, and completion
  re-dispatches when the effective source hash moved.
- Tests: `epg::tests::auto_import_decision_matches_python`,
  `epgstore::tests::usable_data_and_channels_with_show`.

## E04 — Partial imports preserve other sources

- Core: worker mirrors each successfully loaded source into the
  store separately under its own key (`EpgStore::import_source`,
  port of Python's per-source `import_epg_xml`): channel renames
  (normalised-name changes) drop that channel's programmes,
  stored channels are forgotten per source only when no source
  lists them, channels unseen for 14 days are retired and
  programmes older than 14 days pruned. The adapter merges each
  loaded guide into the in-memory guide instead of replacing it.
- Tests: `epgstore::tests::partial_imports_preserve_other_sources`,
  `epg::tests::worker_partial_failure_keeps_loaded_sources_and_reports`.

## E05 — Invalid feeds fail instead of replacing a good guide

- Core: `parse_xmltv_checked` errors on malformed XML
  (end-name checking on), on documents with no `<tv>` root
  (HTML error pages), and on truncation; `decode_epg_bytes`
  returns `Result` — a damaged gzip is an error, never read as
  text. `fetch_epg_source` retries transient errors up to three
  attempts with bounded backoff (`is_transient_epg_error` ports
  Python's hint list). A source counts only when it parsed and
  defined channels; a run with failures reports "{n} of {m}
  guide sources failed: {first}"; zero loaded sources is a
  failure event, and the previous stored guide is untouched.
- Tests: `epg::tests::checked_parse_rejects_non_guides_and_truncation`,
  `epg::tests::worker_partial_failure_keeps_loaded_sources_and_reports`.

## E06 — XMLTV variants retain programmes and text

- Core parse: namespace prefixes stripped (`local_name`),
  `Event::CData` captured, `end` accepted as `stop`, end derived
  from `<length>` (units minutes/seconds/hours) and `<duration>`
  (ISO 8601 or bare minutes) via `parse_duration_seconds` /
  `length_seconds`; `parse_xmltv_time` also accepts ISO `T`
  forms. First channel definition in a source wins.
- Golden fixture: `crates/iptv-core/tests/fixtures/xmltv-variants.xml`
  with integration test `crates/iptv-core/tests/epg_variants.rs`
  :: `xmltv_variants_retain_times_and_text` (prefixed namespace,
  CDATA title/description, `end`, minute/second lengths, ISO and
  bare durations — same times and text as Python).

## E07 — Programme upsert and deduplication

- Core: `EpgGuide::merge` is now Python's identity-based upsert
  — overlapping stored rows are replaced, an exact slot takes
  the incoming title and the incoming description when the title
  changed or the description is longer, channel renames drop
  the channel's programmes; the store applies the same rules per
  source in SQL.
- Tests: `epg::tests::merge_upserts_slots_and_enriches_descriptions`,
  `epgstore::tests::overlapping_slots_replace_and_descriptions_enrich`.

## E08 — EPG rows in main-window search

- Core: `should_run_epg_search` ports Python's guard exactly
  (`_SEARCH_EPG_MIN_CHARS` 3, `_SEARCH_EPG_BROAD_CHANNEL_LIMIT`
  75, result limit 100, All Channels only, not importing, guide
  enabled — no broad title search on this route);
  `EpgStore::channels_with_show` ports `get_channels_with_show`
  with `include_title_search=False` (normalised channel-name
  match, escaped LIKE wildcards, on-air first then future,
  deduplicated, capped); `epg_row_label` formats
  "{channel} - {title} ({start}–{end})" in local HH:MM.
- Adapter: after channel results publish, `append_epg_rows`
  appends the guide rows to the list (rows past the channel
  rows, rendered by `channel_text`); selection, highlight,
  activation (live vs catch-up via the programme window),
  favorites, and the reminder/schedule context actions resolve
  EPG rows to their channel (`row_playlist_index`); stale
  results are rejected because appends only happen for the
  just-published result set and are cleared on the next one.
- Tests: `epg::tests::epg_search_guard_keeps_broad_searches_out`,
  `epgstore::tests::usable_data_and_channels_with_show`,
  adapter `epg_search_rows_render_their_labels_past_the_channel_rows`.

## E09 — Channel EPG dialog completeness

- Adapter: the dialog now lists programmes from the one on air
  through seven days ahead, uncapped; the initial selection is
  the airing programme (`airing_index`); row labels use
  `epg_time_range` (clock range, `YYYY-MM-DD ` prefix for rows
  not starting today); a Tab-reachable read-only "Episode
  description" field shows the highlighted programme's
  description and follows the selection. The old details field
  no longer opens with action instructions.
- Tests: `epg::tests::channel_epg_selection_and_date_labels`.

## E10 — Information panel keeps now/next context

- Core: `epg_info_message` ports Python's `_epg_msg_from_tuple`
  exactly (Now line + description, gap → "Starts at {start}:
  {title}", then the Next line and, when a current programme
  exists, its description; both absent → "No program currently
  airing.").
- Adapter: the highlight handler fills the panel from
  `epg_info_message` whenever the channel matches the guide;
  EPG search rows show their own programme's description.
  Deliberate deviation, kept from the pre-batch behaviour: when
  a channel has no guide match at all the panel keeps the
  existing placeholder "No description available for this
  programme." (Python's description cache would leave it empty).
- Test: `epg::tests::info_message_carries_now_next_and_gap_context`.

## E11 — Independent 60-second freshness timer

- Adapter: `bind_epg_timer` starts a continuous 60 s timer at
  startup, independent of the completion/worker timer: hidden
  window (tray) → skip; `epg_enabled` false → skip; otherwise
  rebuild suffixes and refresh the list rows so an ended
  programme's suffix rolls over with no other activity. The
  completion tick's 300 s staleness backstop is unchanged.
- Covered by the core bulk-build tests (the timer only triggers
  `refresh_epg_suffixes`); the timer wiring itself is GUI code
  with no headless harness — verified by inspection.

## E12 — Bulk guide presentation off per-channel work

- Core: `EpgGuide::bulk_now_next` computes every channel's
  now/next in one pass over the sorted rows; `GuideIndexes`
  precomputes `known_ids` once at build time; `build_row_guide`
  turns a bulk snapshot into per-row suffixes/descriptions with
  one match per channel; `BrowserWorker::with_all_channels`
  borrows the catalogue instead of cloning it; the store's
  `all_now_next` answers the same shape with one range scan.
- Adapter: `refresh_epg_suffixes` and What's on Now build from
  one snapshot (`guide_snapshot`) instead of a lookup per
  channel.
- Found and fixed while wiring this: `BrowserWorker::all_channels`
  double-counted provider channels — extras are merged into the
  browse model by `apply_model` and promoted into the playlist,
  then `all_channels` appended `extras` a second time (What's on
  Now would have listed provider channels twice). `all_channels`
  and `with_all_channels` now read the promoted catalogue model
  itself (`catalogue()`), which is also the slice the published
  search rows index into.
- Tests: `epg::tests::bulk_snapshot_and_row_guide_match_single_lookups`,
  `browser::tests::with_all_channels_covers_playlist_and_extras_in_order`.
- Not done: no 500,000-channel timing benchmark was run (the
  audit ran none either); the claim is algorithmic — per-refresh
  work is O(programmes + channels) with no per-channel set
  rebuilds or catalogue clones on these paths — not a measured
  keyboard-latency figure. The whole-guide in-memory storage
  limit from validation 0011 is unchanged.

## Gates

Run 2026-11-03 in this worktree (Linux, rust 1.97.1):

- `cargo fmt --all -- --check` — pass.
- `cargo test --workspace` — pass, 0 failures in every suite
  (iptv-core lib 190 tests in the per-crate run; iptv-app,
  iptv-vlc, iptv-signer, iptv-sapi and the integration suites —
  local_browse, epg_variants, playlist_parity, stalker_portal —
  all ok).
- `cargo clippy --workspace --all-targets -- -D warnings` —
  pass, no warnings.
- `python3 tools/check-playlist-parity.py ~/workspace/pyclone-pinned`
  — PASS (four shared fixtures, parsed fields/order, 24 searches
  and load/empty outcomes; pin unchanged at 9aa89e0).
- `git diff --check` — pass.

Environment note: the run began on a cold VM (platform replaced
it mid-batch, wiping /root and the apt packages); dev
dependencies were restored and the wxWidgets/wxdragon builds ran
under heavy contention from the sibling audit batches sharing
the VM, which stretched wall-clock time but not results.
