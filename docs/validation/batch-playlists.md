# Validation — Playlists / providers / favorites / VOD batch (B01–B21)

Branch: `agent/parity-playlists`. Python oracle: pinned clone
`~/workspace/pyclone-pinned` at 9aa89e053225ef2c83c4f3659775a00723012ff7
(behavior shape; audit line evidence in docs/feature-parity-audit.md governs).
Playlist-parity pin and goldens unchanged.

## Per item

### B01 — Provider playlist scopes hide their channels — done
- providers.rs: `tag_xtream_channels` inserts `playlist-id` = provider id on
  every Xtream channel; Stalker `fetch_channels` inserts `playlist-id`
  alongside `provider-id`. Scoped browsing (browser.rs filters on
  `playlist-id`) now sees provider channels.
- Test: `xtream_channels_carry_scope_and_stream_id`;
  `provider_scope_shows_its_channels_and_replacement_never_duplicates`.

### B02 — Provider reloads append duplicates / retain removed accounts — done
- browser.rs: new `replace_provider_channels` replaces the merged provider
  extras wholesale and reapplies from the raw model; `all_channels()` drops
  the already-merged tail (`playlist_extras`) before appending current
  extras, so provider channels count once. Adapter `poll_providers` calls
  `replace_provider_channels` unconditionally; empty account list clears
  extras.
- Test: `provider_scope_shows_its_channels_and_replacement_never_duplicates`.

### B03 — Removing the last plain playlist leaves its channels — done
- Adapter `reload_sources_mode` always submits a sources load, even with
  zero plain sources; browser `reset_scope` clears the scope without a
  resubmit. An empty source list publishes an empty catalogue.
- Test: `empty_source_list_publishes_an_empty_catalogue`.

### B04 — Editing an account deletes fields absent from the form — done
- providers.rs: `merge_account_fields` clones the existing account object
  and overlays only the fields the form supplies. The account form's OK
  handler merges into the existing entry; id preserved from existing
  id/provider_id.
- Test: `account_edit_merge_preserves_fields_absent_from_the_form`.

### B05 — Account forms cannot configure all provider options — done
- Adapter `open_account_form`: Stalker gains optional Username and Password
  fields and the "Attempt to add provider XMLTV" auto-EPG checkbox; Xtream
  gains Playlist type (m3u_plus/m3u/enigma2, default m3u_plus), Stream
  output (ts/m3u8/rtmp, default ts) and the "Automatically add XMLTV URL"
  auto-EPG checkbox (default checked, matching Python).
- Adapter wiring only; covered by the B04/B06 core tests for the saved
  field shapes.

### B06 — Provider validation and MAC conveniences missing — done
- providers.rs: `sanitize_mac` ('-'→':', '.' dropped, uppercased, bare
  12-hex colon-joined), `mac_is_valid` (six 2-hex octets), `random_mag_mac`
  (00:1A:79 + three random octets), `account_form_error` (Stalker: URL
  required + valid MAC; Xtream: username, password, URL required; messages
  match Python's wording). Form: values trimmed, MAC sanitized, Randomise
  MAC button, new Stalker accounts default to a random MAG MAC; a
  validation error warns and keeps the form open.
- Test: `mac_sanitize_validate_and_form_errors`.

### B07 — M3U directives lose stream headers and catch-up fields — done
- lib.rs: the parser consumes `#EXTVLCOPT` and `#KODIPROP` directives
  (user agent, referer, origin, cookie, authorization, accept, ordered
  extra `http-header*` headers deduplicated case-insensitively by name,
  catchup-source/days/type). playback.rs: `StreamHeaders::for_channel`
  iterates the multi-line `http-headers` value line by line.
- Test: `vlcopt_and_kodiprop_directives_supply_headers_and_catchup`;
  playlist-parity fixtures still pass.

### B08 — Playlist-wide Dispatcharr / catch-up clock metadata lost — done
- lib.rs: `#EXTM3U catchup-timezone` is parsed; every channel in such a
  playlist is marked `dispatcharr=true`, and archive-capable entries
  (with `catchup` or `catchup-source`) carry `catchup-timezone`.
- Test: `extm3u_catchup_timezone_marks_dispatcharr_channels`.

### B09 — Playlist-linked guide discovery and opt-out absent — done
- lib.rs: `playlist_header_epg_urls` reads the first line only
  (`x-tvg-url`/`url-tvg`, comma-separated, http(s) only, scheme lowercased,
  deduplicated); `merge_playlist_header_guides` replaces a read source's
  entry, keeps an unread source's entry, drops a removed source's entry.
  sources.rs reports per-source guide URLs for sources actually read;
  loading.rs carries them on the load result; browser.rs merges them and
  exposes `playlist_guide_urls()`. Adapter: `effective_epg_urls` adds the
  playlist guides to the configured EPG list only when `use_playlist_epg`
  (default true); the EPG manager edits only the user's own EPG list and
  persists the "Also use the guide each playlist links to" checkbox.
  Provider-header guides keep flowing through `poll_providers` as before.
- Tests: `playlist_header_epg_urls_parse_dedupe_and_filter`;
  `header_guides_are_reported_per_read_source`.

### B10 — Imported Xtream favorites lose stream-id identity — done
- lib.rs: `extract_stream_id` implements Python's rule (leading digits of
  the last URL path segment, else the second-to-last). Parser inserts
  `stream-id` when provider-type is xtream or a catchup source exists;
  `tag_xtream_channels` inserts it for provider entries (an explicit
  attribute wins). Favorite keys now match Python's for ordinary Xtream
  entries.
- Tests: `stream_id_comes_from_xtream_url_shapes`;
  `xtream_channels_carry_scope_and_stream_id` (favorite key asserted as
  `xtream|xtream-9|42|Movie`).

### B11 — Plain playlist cache and parsed-cache prefill missing — done
- sources.rs: URL downloads cached 15 minutes; parsed results cached under
  a hash of the text salted with a parser version (`rust-parse-1`), so a
  parser change cannot be hidden behind older cached channels.
  `prefill_model` returns parsed-cache channels for a load; loading.rs
  emits the prefill as an intermediate result before the network load and
  browser.rs displays it while the final load runs.
- Tests: `parsed_cache_prefill_and_reuse`;
  `sources_load_prefills_from_parsed_cache_then_publishes_final`.
- Note: prefill is emitted by the load worker thread, not a separate UI
  prefill pass; the visible behavior (cached channels first, then the
  refreshed model) matches Python.

### B12 — Partial plain-source failure drops usable channels — done
- sources.rs: on a per-source failure, that source's previous channels
  (matched by scope id) are retained and the source is reported in
  `stale_sources`; only a source with nothing retained counts as failed,
  and the load fails outright only when nothing at all is available.
  browser.rs surfaces stale sources on the `Loaded` event; the adapter
  names each stale source in the status line.
- Test: `failed_source_retains_its_previous_channels`.

### B13 — Source manager row actions and friendly names missing — done
- sources.rs: `source_name_key` (SHA-256 hex of the source string, so
  credentials never become a mapping key verbatim),
  `normalize_source_names`, `source_display_label` (friendly name, else
  URL host or file name). Adapter: playlist manager gains Refresh,
  Rename (F2), Copy URL buttons and F5 / Ctrl+A (arm refresh-all) / F2 /
  Delete key handling; plain sources display and persist friendly names
  (`playlist_names`); the scope picker uses friendly labels. Refresh
  drops the refreshed sources' download-cache files; refresh-all forces a
  fresh load (`load_merged_with` force flag; provider worker `load_fresh`
  skips the fresh-cache read). EPG manager gains the same rename/copy/
  row-shortcut behavior with `epg_names`.
- Tests: `friendly_names_labels_and_keys`;
  `url_download_cache_path`/`drop_url_download_cache` covered via
  `parsed_cache_prefill_and_reuse` and the refresh paths.
- Approximation: Python refreshes one source with a partial load that
  merges back; here a single plain source sheds only its own download
  cache before a normal sources load (other sources answer from their
  caches), a single account reloads providers fresh, and refresh-all
  forces a full fresh load. End state matches; the load granularity
  differs.

### B14 — Playlist deletion has no default-No confirmation — done
- Adapter `manager_remove`: a Yes/No dialog with No as the default names
  the playlist ("Delete the playlist “{name}”? This removes it from the
  list of playlists. It cannot be undone."); removal happens only on Yes.
- Adapter wiring only (wx dialog); the guarded remove path is the single
  deletion entry point in the manager.

### B15 — Stalker top-level tokens rejected — done
- providers.rs: `portal_token` accepts `token` at the top level or under
  `js` (non-empty only) and is used for both handshake and login token
  extraction.
- Test: `portal_token_accepts_top_level_and_nested_shapes`.

### B16 — Query/fragment-bearing provider URLs normalize incorrectly — done
- providers.rs: `normalize_base_url` strips query and fragment before
  splitting authority/path; userinfo dropped and trailing portal
  filenames stripped as before.
- Test: `base_url_normalization_drops_query_and_fragment`.

### B17 — VOD ignores scope and reclassifies provider content — done
- vod.rs: `build_catalog` filters provider accounts to the active scope,
  takes channels from the scoped model + extras, and excludes
  provider-type (xtream/stalker) channels from the plain-M3U heuristic.
  Adapter passes `catalogue_parts()` and `browser.scope()` to the worker;
  the catalogue filtering/cloning happens on the worker thread.
- Test: `build_catalog_respects_scope_and_excludes_provider_channels`.

### B18 — Late VOD replies replace newer navigation — done
- vod.rs: jobs and events carry generation identity. A build generation
  supersedes older catalogue builds and their episode requests; a
  navigation generation (group change, Back, window close) supersedes
  episode replies only. Adapter `poll_vod` drops non-current events via
  `is_current_catalog` / `is_current_episodes`; navigation handlers call
  `invalidate()`.
- Test: `worker_events_carry_generations_and_invalidate_stales_them`.

### B19 — Large-playlist navigation scans/clones on the UI thread — partial
- Done: scoped category listing borrows channels (`categories_from`
  iterator) instead of cloning the catalogue; favorite membership is a
  hash index (`FavoriteSet` keeps a `HashSet` in sync with its key list,
  O(1) `is_favorite`); VOD catalogue filtering/cloning moved to the VOD
  worker thread; provider extras are no longer double-counted through
  `all_channels()`.
- Tests: `membership_index_stays_in_sync_with_toggles`;
  `build_catalog_respects_scope_and_excludes_provider_channels`.
- NOT completed: Python's background navigation index was not built;
  this batch removes the identified clones/scans and the linear favorite
  membership test, but category/scope/search projection still runs where
  it did (now over borrowed data). Actual latency at 500,000 channels
  was not measured (the audit itself records it was not measured).

### B20 — Favorite counts stale and ignore scope — done
- browser.rs: `favorite_count()` counts only favorites in the active
  scope. Adapter: the category control rebuilds when the category set
  changes, while Favorites is active, or when the scoped favorite count
  changed since the last sync.
- Test: `favorite_count_follows_the_scope`.

### B21 — Identical entries within one source not deduplicated — done
- sources.rs: merge and prefill deduplicate by (scope id, name, URL,
  provider-id) across the merged catalogue; entries from distinct
  sources keep distinct scope ids and are preserved.
- Test: `identical_entries_dedupe_within_one_source_only`.

## Gates

- `cargo fmt --all` / `cargo fmt --all -- --check` — pass.
- `cargo test --workspace` — PASS: 206 tests, 0 failed (187 iptv-core lib
  tests including every regression test named above, plus integration
  and sibling-crate tests).
- `cargo clippy --workspace --all-targets -- -D warnings` — PASS.
- `python3 tools/check-playlist-parity.py ~/workspace/pyclone-pinned` —
  PASS (four shared fixtures, parsed fields/order, 24 searches and
  load/empty outcomes; pin 9aa89e053225ef2c83c4f3659775a00723012ff7).
- `git diff --check` — pass.

Environment note: gate runs were repeatedly interrupted by this VM's
storage — the 7.5 GB root overlay filled with parallel build target
dirs (one wxdragon archive step failed with "No space left on
device"), a build on the home filesystem hit the btrfs group quota
(EDQUOT), and the VM was replaced twice, wiping system packages (dev
libraries restored via the azure-only apt sidestep). The passing runs
used `CARGO_TARGET_DIR=/root/iptv-target-playlists` with
`CARGO_PROFILE_DEV_DEBUG=0` / `CARGO_PROFILE_TEST_DEBUG=0` to slim the
target; results are otherwise identical to a default-profile run.
