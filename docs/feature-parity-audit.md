# Feature parity audit: current Python reference versus Rust

Date: 2026-11-03. Status: **addressed 2026-11-03** — every entry below carries its fix and regression evidence; the only entries not fully closed are B19 (responsiveness half open), E12 (no scale benchmark), and the recorded limits on E10, P18, R10, R19, S05 and S10. Consolidated record: docs/validation/0063-parity-audit-closure.md. Owner-heard NVDA acceptance remains separate under ADR 0004.

## Reference and scope

- Python checkout: `C:\Users\admin\git\Accessible-IPTV-Client`, revision
  `6afe6014fa773a5489d4c43ccafad5726386d931` (v1.150.58).
- Rust checkout: `C:\Users\admin\git\Accessible-IPTV-Client-Rust`, revision
  `ea9131d167f8a14955cf47ef2af591b4b386fb1e` (v0.4.4).
- Compared reachable Python behavior with Rust core logic **and adapter callers**:
  playlists/providers, browsing/VOD, EPG, playback/audio/subtitles, recording/DVR,
  catch-up/downloads/reminders, casting, settings, updates, help and diagnostics.
- `Python` source references below are relative to the Python checkout; `Rust`
  references are relative to this repository. Line numbers describe these snapshots.
- Compared existing validation notes through 0062. Historical implementation
  records remain historical evidence; a control or helper existing does not prove
  the complete Python behavior is connected to it.
- This is a source audit, not a runtime reproduction or an accessibility audit.
  Checks below are acceptance cases for future fixes, not claimed passes. No
  application code, workflows, user settings, or Python files were changed.

The existing playlist parity tool pins Python `9aa89e0`, checks four fixtures and
24 searches, and rejects a different reference HEAD. Its historical PASS does
not establish parity against the current Python checkout or the features below.
Do not change that pin or regenerate goldens without reviewing the changed
reference behavior.

## How to use this ledger

Each entry identifies an observable gap, source evidence and a concrete closing
check. **High** means a core action can fail, lose state/data, or violate the
large-playlist responsiveness requirement. **Medium** means a working feature
omits meaningful Python behavior. **Low** means a smaller missing convenience.
These are implementation priorities, not measured runtime failure rates.

IDs stay stable when fixes land. Link a fix and its regression evidence beside
the entry; do not mark a whole area complete because its menu or core helper
exists. Owner-heard and live-environment evidence remain separate under ADR 0004.

## EPG and programme browsing

### E01 — Python EPG source settings are ignored (high)

Python loads and manages `epgs`; Rust even defaults that key, but its startup,
manager and import actions use `epg_urls`. Importing a Python settings backup can
therefore preserve the guide list without loading or displaying any of it.
Evidence: Python `main.py:950`, `main.py:6524`, `main.py:6779`;
Rust `crates/iptv-core/src/config.rs:42`,
`crates/iptv-app/src/main.rs:7582`, `main.rs:7963`, `main.rs:8043`.
Check: import a synthetic Python backup containing only `epgs`; open the manager,
load its guide, edit it and round-trip the settings back to Python.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-epg.md and validation 0063.

### E02 — Saved guide database is never reopened for reading (high)

The EPG worker writes `epg.db`, but the adapter initializes `epg_store` to `None`
and never assigns an opened store. `store_programmes` consequently returns
nothing. The restart/offline schedule fallback claimed in validation 0029 is
not connected. Python reads its persisted database before a fresh import.
Evidence: Python `main.py:2796`;
Rust `crates/iptv-app/src/main.rs:613`, `main.rs:4551`, `main.rs:4574`,
`crates/iptv-core/src/epg.rs:813`.
Check: import a guide, restart without network access, then open Channel EPG,
What's on Now and the selected channel's information panels.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-epg.md and validation 0063.

### E03 — EPG enable and refresh settings have no adapter behavior (medium)

Rust stores `epg_enabled` and `epg_auto_import_interval_hours` but never reads
them outside configuration defaults. It lacks Python's source-hash, last-import
and guide-usability decision. A disabled guide still loads, and reopening or
reloading sources does not apply the saved guide-freshness interval.
Evidence: Python `main.py:6555`, `main.py:6588`, `main.py:6636`;
Rust `crates/iptv-core/src/config.rs:56`, `config.rs:58`,
`crates/iptv-app/src/main.rs:7566`, `main.rs:7581`.
Check: disabling EPG prevents automatic work; changed sources refresh; an
automatic load decision respects the saved interval for an already usable guide.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-epg.md and validation 0063.

### E04 — One-source or partial imports erase other sources (high)

`Import EPG` loads just the chosen file. Every Rust store import deletes all
channels and programmes before inserting the new merged result. Importing one
file, or refreshing A successfully while B fails, drops B's previously usable
guide. Python preserves sources omitted from a run or failing during that run.
Evidence: Python `playlist.py:2853`;
Rust `crates/iptv-app/src/main.rs:4016`,
`crates/iptv-core/src/epg.rs:793`,
`crates/iptv-core/src/epgstore.rs:52`.
Check: import independent guides A and B; re-import only A, then refresh with B
unreachable. B's retained schedules must remain available.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-epg.md and validation 0063.

### E05 — Invalid feeds can be reported as successful guide loads (high)

Rust stops XML parsing on an error and returns the partial guide; gzip failure
falls back to interpreting compressed bytes as text. The fetch function still
returns `Ok`, increments the loaded-source count, and can replace a good store
with an empty or partial guide. Python has transient-transfer retries, robust
gzip download/verification and a source-success count rather than treating every
HTTP body as a valid guide.
Evidence: Python `playlist.py:2541`, `playlist.py:2574`, `playlist.py:2626`,
`playlist.py:2802`, `playlist.py:2853`;
Rust `crates/iptv-core/src/epg.rs:186`, `epg.rs:263`, `epg.rs:795`, `epg.rs:857`.
Check: supply HTML with HTTP 200, truncated XML and a damaged gzip; retain the
usable guide, report the failure, and retry transient transfers within bounds.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-epg.md and validation 0063.

### E06 — Supported XMLTV variants lose programmes or text (medium)

Python accepts `end` as an alternative to `stop`, calculates an end from
`length`/`duration`, and strips element namespaces. Rust only accepts `stop`,
matches literal tag names, and ignores CDATA events. Duration-only programmes
are discarded; prefixed XMLTV elements and CDATA titles/descriptions are lost.
Evidence: Python `playlist.py:839`, `playlist.py:2698`, `playlist.py:2721`;
Rust `crates/iptv-core/src/epg.rs:212`, `epg.rs:225`, `epg.rs:252`, `epg.rs:263`.
Check: golden XMLTV fixtures for `end`, minute/second lengths, ISO duration,
prefixed namespaces and CDATA must retain the same times and text as Python.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-epg.md and validation 0063.

### E07 — Multi-source programme replacement and deduplication are absent (medium)

Rust appends and sorts programme rows when merging guides. It does not implement
Python's identity-based upsert, longer-description enrichment or replacement of
overlapping obsolete slots. Two sources describing the same slot can produce
duplicate schedule rows and ambiguous now/next results.
Evidence: Python `playlist.py:1556`;
Rust `crates/iptv-core/src/epg.rs:47`, `epg.rs:67`,
`crates/iptv-core/src/epgstore.rs:72`.
Check: import duplicate slots, an expanded synopsis and a corrected overlapping
time slot from two sources; compare schedule and now/next with Python.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-epg.md and validation 0063.

### E08 — EPG rows are missing from main-window search (medium)

Python appends bounded programme rows for sufficiently specific channel-name
searches in All Channels, then routes highlight/play/record through their channel
match. Rust searches channel names only. Core `EpgGuide::search` and
`EpgStore::search_titles` have no application callers. Python deliberately
disables broad title search on this route; that guard must survive the port.
Evidence: Python `main.py:5879`, `main.py:6037`, `main.py:6054`,
`client_epg.py:80`;
Rust `crates/iptv-core/src/search.rs:19`,
`crates/iptv-app/src/main.rs:826`.
Check: a qualifying channel-name query appends the same bounded guide rows,
rejects stale search results and keeps their play/record actions working.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-epg.md and validation 0063.

### E09 — Channel EPG omits descriptions, dates and full upcoming selection (medium)

Rust shows at most 200 entries from one hour ago and selects row zero. It renders
only clock times and title; the status field is not a programme description.
Python queries seven days of current/upcoming programmes, dates non-today rows,
selects the airing programme and exposes the highlighted description through Tab.
Evidence: Python `main.py:667`, `main.py:2796`, `main.py:9213`, `main.py:9266`;
Rust `crates/iptv-app/src/main.rs:4795` through `main.rs:4834`.
Check: open a dense seven-day guide with yesterday/today/tomorrow entries; begin
on the airing programme, distinguish dates, reach later programmes and read each
description without replacing it with action instructions.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-epg.md and validation 0063.

### E10 — Main information panel loses now/next context (medium)

Python's highlighted-channel panel includes current title, times and description,
then the next programme and description; a gap shows the next start. Rust fills
the field with only the current description or title and shows a placeholder if
nothing is airing, even when a next programme exists.
Evidence: Python `client_epg.py:88`;
Rust `crates/iptv-app/src/main.rs:4945` through `main.rs:4982`.
Check: compare the panel during a current show, a schedule gap and an empty
description, with a next show present in every case.

**Fix:** Fixed with one deliberate deviation: channels with no guide match keep the existing placeholder text instead of Python's exact wording. See docs/validation/batch-epg.md.

### E11 — Guide row freshness depends on an unrelated active worker (medium)

Python uses an independent 60-second now-playing timer. Rust refreshes suffixes
only inside the completion timer, after 300 seconds, and stops that timer when
loaders/recordings/reminders are idle. Rows can therefore retain an ended show
indefinitely while the main window remains open.
Evidence: Python `main.py:1720` through `main.py:1749`;
Rust `crates/iptv-app/src/main.rs:743` through `main.rs:777`.
Check: finish loading, leave every worker idle, cross a programme boundary and
verify rows update without another search, load or recording.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-epg.md and validation 0063.

### E12 — Large-guide presentation still runs expensive work on the UI thread (high)

Moving SQLite writes off the UI thread in validation 0049 did not move guide
index construction, all-channel suffix building or What's on Now construction.
`all_channels` clones the catalogue; `current_programme` builds a guide-ID set
and clones the matched schedule per channel. What's on Now calls it for every
channel. Highlighting also rebuilds the guide-ID set. Python builds bulk
now/next indexes and descriptions in gated background work.
Evidence: Python `main.py:1546` through `main.py:1612`;
Rust `crates/iptv-core/src/browser.rs:155`,
`crates/iptv-app/src/main.rs:1315`, `main.rs:4551`, `main.rs:4945`,
`main.rs:8180`, `main.rs:9565`.
Check: measure keyboard response during guide completion, row refresh and What's
on Now with 500,000 channels and a large guide. Publish background snapshots
without stale updates. No timing benchmark was run in this audit. The existing
whole-guide in-memory storage limit is also still relevant; see validation 0011.

## Playlists, providers, favorites and VOD

**Fix:** Fixed (algorithmic): bulk now/next snapshots replace per-channel lookups; no 500k-channel benchmark was run. See docs/validation/batch-epg.md.

### B01 — Provider playlist scopes hide their channels (high)

The scope picker selects a provider account id, but the browser filters on
`playlist-id`, which neither provider mapper sets. Python tags every source's
channels. Evidence: Python `main.py:2560`; Rust
`crates/iptv-app/src/main.rs:948`, `crates/iptv-core/src/browser.rs:123`,
`crates/iptv-core/src/providers.rs:601`, `providers.rs:980`.
Check: select each Xtream/Stalker account in Playlist view; its channels,
categories and favorites must remain visible.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-playlists.md and validation 0063.

### B02 — Provider reloads append duplicates and retain removed accounts (high)

Rust extends `extras` on provider completion without clearing it on reload.
`all_channels` also appends extras to a model already containing them. Python
replaces its catalogue and provider registry. Evidence: Python `main.py:2574`;
Rust `crates/iptv-app/src/main.rs:1214`,
`crates/iptv-core/src/browser.rs:155`, `browser.rs:261`.
Check: save an unchanged provider twice, then remove it; counts stay stable,
then its channels disappear from every browsing surface.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-playlists.md and validation 0063.

### B03 — Removing the last plain playlist leaves its channels (high)

Rust `reload_sources` skips loading when the source list is empty. Python
publishes the resulting empty catalogue. Evidence: Python `main.py:6734`;
Rust `crates/iptv-app/src/main.rs:7545`.
Check: remove the only file/URL source and save; channels and categories empty.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-playlists.md and validation 0063.

### B04 — Editing an account deletes fields absent from the form (high)

Rust reconstructs the account map and retains only the original id. A name-only
edit can discard Stalker credentials, Xtream output/type, auto-EPG and other
existing fields. Evidence: Python `playlist.py:3431`, `playlist.py:3490`,
`playlist.py:3542`, `playlist.py:3621`; Rust
`crates/iptv-app/src/main.rs:7890`, `main.rs:7922`.
Check: import non-default account settings, change only its name, and compare
every saved field, including credentials and unknown retained settings.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-playlists.md and validation 0063.

### B05 — Account forms cannot configure all supported provider options (high)

Rust omits optional Stalker username/password, both auto-EPG switches and
Xtream playlist type/stream output. Python exposes these controls. Evidence:
Python `playlist.py:3387`, `playlist.py:3431`, `playlist.py:3542`;
Rust `crates/iptv-app/src/main.rs:7890`.
Check: configure a credential-requiring portal and an HLS-output Xtream account
entirely through the application.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-playlists.md and validation 0063.

### B06 — Provider validation and MAC entry conveniences are missing (medium)

Rust saves blank/untrimmed values without Python's required-field checks,
Stalker URL/MAC validation, MAC normalization, default MAG prefix or Randomise
MAC action. Evidence: Python `playlist.py:3471`, `playlist.py:3593`;
Rust `crates/iptv-app/src/main.rs:7921`.
Check: reject incomplete accounts without closing the form; normalize accepted
MAC spellings and provide Python's default/random MAC behavior.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-playlists.md and validation 0063.

### B07 — M3U directives lose stream headers and catch-up fields (high)

Rust ignores comment directives other than `EXTINF`. Python consumes
`EXTVLCOPT` and `KODIPROP` headers/catch-up properties. Existing attribute and
URL-pipe handling cannot recover metadata supplied only through these directives.
Evidence: Python `client_m3u.py:144`, `client_m3u.py:245`;
Rust `crates/iptv-core/src/lib.rs:104`.
Check: header-protected playback, recording and casting, plus directive-defined
catch-up, receive the same user agent, referer, origin, cookies and extra headers.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-playlists.md and validation 0063.

### B08 — Playlist-wide Dispatcharr and catch-up clock metadata is lost (high)

Python reads `EXTM3U catchup-timezone`, marks channels as Dispatcharr and passes
the clock to archive entries. Rust skips that header. This also prevents applying
the Dispatcharr buffering exception correctly once playback cache wiring lands.
Evidence: Python `client_m3u.py:76`, `client_m3u.py:237`;
Rust `crates/iptv-core/src/lib.rs:104`.
Check: import a Dispatcharr playlist; preserve its saved buffer and UTC archive
interpretation, including around daylight-saving transitions.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-playlists.md and validation 0063.

### B09 — Playlist-linked guide discovery and its opt-out are absent (high)

Python extracts and deduplicates `x-tvg-url`/`url-tvg` guides, removes guides
belonging to removed sources and exposes the playlist-guide checkbox. Rust
returns channels only; `use_playlist_epg` exists only as a default. Evidence:
Python `playlist_headers.py:17`, `main.py:2469`, `main.py:6520`,
`playlist.py:3156`; Rust `crates/iptv-core/src/sources.rs:140`,
`crates/iptv-core/src/config.rs:57`, `crates/iptv-app/src/main.rs:7981`.
Check: a playlist-linked guide works without manual entry; disabling/removing
that source stops importing its guide without deleting manually added sources.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-playlists.md and validation 0063.

### B10 — Imported Xtream favorites lose their stream-id identity (high)

Python extracts `stream-id` from provider stream URLs. Rust tags provider type
and id but not that stream id; both favorite-key algorithms include it. Thus
using the same key algorithm still produces different keys for ordinary Xtream
entries lacking an explicit attribute. Evidence: Python `client_m3u.py:269`,
`favorites.py:57`; Rust `crates/iptv-core/src/providers.rs:980`,
`crates/iptv-core/src/favorites.rs:39`.
Check: import Python Xtream favorites, refresh/renew URLs, and retain the marks.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-playlists.md and validation 0063.

### B11 — Plain playlist cache and parsed-cache prefill are missing (high)

Python displays parsed cached channels before network work, caches URL downloads
for 15 minutes and reuses parsed file/URL results. Rust always reads/downloads
and reparses plain sources. Provider caching is implemented and is a different
path. Evidence: Python `main.py:2262`, `main.py:2440`, `main.py:2470`;
Rust `crates/iptv-core/src/sources.rs:81`, `sources.rs:152`.
Check: reopen a previously loaded URL offline; unchanged large file/URL sources
should expose cached entries promptly, then refresh without losing the model.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-playlists.md and validation 0063.

### B12 — A partial plain-source failure drops usable channels (high)

Rust publishes only successful sources when any source loads. Its prior-model
retention handles total failure, not a failing member of a merged catalogue.
Python retains that source's previous channels/session. Evidence: Python
`main.py:2248`, `main.py:2511`; Rust `crates/iptv-core/src/sources.rs:160`.
Check: load two sources, take one offline, reload; retain both with a clear
stale-source message for the failing member.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-playlists.md and validation 0063.

### B13 — Source manager row actions and friendly names are missing (medium)

Python supports selected/all refresh, F5, Ctrl+A, F2 rename, Del and Copy URL;
plain-source friendly names persist separately. Rust offers add/edit/remove,
ignores those names, and saving does not bypass the provider cache for a forced
refresh. EPG sources also lack Python's shared rename/copy/row-shortcut behavior.
Evidence: Python `playlist.py:2991`, `playlist.py:3284`, `main.py:6746`;
Rust `crates/iptv-app/src/main.rs:7595`, `main.rs:7624`, `main.rs:7963`,
`crates/iptv-core/src/providers.rs:1006`.
Check: exercise each row action, refresh one source independently and retain
friendly names after restart.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-playlists.md and validation 0063.

### B14 — Playlist deletion has no default-No confirmation (medium)

Python names the selected playlist and asks before deletion. Rust removes it
immediately; saving makes the removal durable. Evidence: Python
`playlist.py:3328`; Rust `crates/iptv-app/src/main.rs:7770`.
Check: deletion names the account, defaults to keeping it and preserves all
credentials when canceled.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-playlists.md and validation 0063.

### B15 — Stalker top-level tokens are rejected (high)

Python accepts a token at the top level or under `js`; Rust accepts only the
nested shape in handshake/login responses. Evidence: Python `providers.py:322`,
`providers.py:342`; Rust `crates/iptv-core/src/providers.rs:537`,
`providers.rs:561`.
Check: a fake portal returning `{"token":"abc"}` supports handshake/login,
channel loading, account information and link resolution.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-playlists.md and validation 0063.

### B16 — Query/fragment-bearing provider URLs normalize incorrectly (medium)

Python parses and rebuilds the base URL without query/fragment components before
stripping endpoint names. Rust's manual split retains those components when
appending API paths. Evidence: Python `providers.py:26`;
Rust `crates/iptv-core/src/providers.rs:37`.
Check: pasted server, userinfo, `get.php?...` and fragment-bearing variants
normalize to the intended base and generate valid provider endpoints.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-playlists.md and validation 0063.

### B17 — VOD ignores scope and reclassifies provider content as M3U (high)

Python limits VOD to the selected source and excludes provider channels from
the plain-M3U heuristic. Rust always passes all accounts/all channels, then runs
the heuristic after loading provider catalogues. Evidence: Python
`main.py:6342`; Rust `crates/iptv-app/src/main.rs:1941`,
`crates/iptv-core/src/vod.rs:640`.
Check: select one playlist before entering VOD; only its entries appear, once.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-playlists.md and validation 0063.

### B18 — Late VOD replies replace newer navigation (medium)

Python rejects stale catalogue/episode generations. Rust jobs/events have no
request identity, and every episode reply updates the current list. Evidence:
Python `main.py:6316`, `main.py:6376`, `main.py:6481`;
Rust `crates/iptv-core/src/vod.rs:590`,
`crates/iptv-app/src/main.rs:2114`.
Check: request series A, change group/reload/close, then deliver A's late reply;
it must not replace the current list or reopen obsolete navigation.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-playlists.md and validation 0063.

### B19 — Large-playlist navigation still scans and clones on the UI thread (high)

Category/scope/search submission computes projections before entering its worker;
provider intake and VOD clone the catalogue there. Favorites test channels
against a linear key vector during category synchronization. Python has a
background navigation index and cached favorite projection. Evidence: Python
`main.py:1290`, `main.py:1324`;
Rust `crates/iptv-core/src/browser.rs:94`, `browser.rs:217`, `browser.rs:261`,
`crates/iptv-core/src/favorites.rs:118`,
`crates/iptv-app/src/main.rs:1955`, `main.rs:9411`.
Check: measure scope/category/favorite/VOD keyboard response at 500,000 channels
with many favorites. Actual latency was not measured in this audit.

**Fix:** Fixed in part: catalogue clone removed, O(1) favorite membership, VOD filtering off the UI thread. Python's background navigation index was not built and 500k-playlist latency is unmeasured; the responsiveness half of this entry stays open. See docs/validation/batch-playlists.md.

### B20 — Favorite counts remain stale and ignore scope (medium)

Rust counts favorites across the whole catalogue and rebuilds category labels
only on category-identity changes or while Favorites is active. Toggling from
All Channels can leave the old count. Python updates the scoped count. Evidence:
Python `main.py:1290`, `main.py:1876`;
Rust `crates/iptv-core/src/browser.rs:169`,
`crates/iptv-app/src/main.rs:9411`.
Check: toggle favorites from All Channels and switch scopes; the displayed
count updates immediately and agrees with the visible source.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-playlists.md and validation 0063.

### B21 — Identical entries within one source are not deduplicated (medium)

Python deduplicates by source scope, name, URL and provider id, while preserving
entries belonging to distinct sources. Rust concatenates parsed entries.
Evidence: Python `main.py:2559`;
Rust `crates/iptv-core/src/sources.rs:160`.
Check: repeat an entry twice in A and once in B; show one from each source.

## Playback, audio, subtitles and history

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-playlists.md and validation 0063.

### P01 — Saved/opening/recovery cache settings do not reach VLC (high)

Python applies opening/saved cache values, gradually increases recovery cache,
preserves Dispatcharr choices and uses the small cache for segment rollovers.
Rust's `start_cache` helper has no production caller; VLC loading adds only user
agent/referer options. Evidence: Python `internal_player.py:797`,
`internal_player.py:944`, `internal_player.py:1579`;
Rust `crates/iptv-core/src/playback.rs:236`,
`crates/iptv-vlc/src/lib.rs:316`.
Check: capture VLC options for initial live play, saved choices, Dispatcharr,
stalls and rollovers; preserve the half-second start and unchanged playback rate.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-playback.md and validation 0063.

### P02 — Closing the player leaves VLC and its buffer lease active (high)

Rust's close handler stops the session and MediaCtrl but does not stop VLC,
clear `vlc_active` or release the live-buffer lease. Python stops/releases the
actual engine asynchronously. Evidence: Python `internal_player.py:3328`,
`internal_player.py:3356`; Rust `crates/iptv-app/src/main.rs:6116`.
Check: Alt+F4 during VLC playback stops sound, upstream connection, buffer ingest
and pending reconnect work.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-playback.md and validation 0063.

### P03 — A frozen stream reporting Playing has no watchdog (high)

Python checks decoded/played counters with clock/startup/discontinuity safeguards.
Rust reacts to state changes and Buffering; unchanged Playing does not detect a
frozen presentation. Evidence: Python `internal_player.py:1671`,
`internal_player.py:1770`; Rust `crates/iptv-app/src/main.rs:6860`,
`crates/iptv-core/src/playback.rs:649`.
Check: freeze counters/position while Playing remains reported; recover within
Python's bounds without false stalls for radio artwork or timestamp resets.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-playback.md and validation 0063.

### P04 — Normal live TS rollovers spend the failure retry budget (high)

Python distinguishes expected Xtream TS endings and reopens after 200 ms through
a separately bounded refresh path. Rust sends every live Ended event through
ordinary recovery. Evidence: Python `internal_player.py:1176`,
`internal_player.py:1236`; Rust `crates/iptv-app/src/main.rs:6891`,
`crates/iptv-core/src/playback.rs:677`.
Check: seven normal short TS segments continue with rollover timing and cache
without exhausting the ordinary failure budget.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-playback.md and validation 0063.

### P05 — Direct HLS playback ignores the rendition bitrate cap (medium)

Python resolves HLS masters against the saved cap. Rust reads that cap only in
the local-buffer path and gives direct VLC playback the original URL. Evidence:
Python `internal_player.py:1239`; Rust `crates/iptv-app/src/main.rs:5125`,
`main.rs:6298`.
Check: buffering off, cap below the highest rendition; select Python's rendition.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-playback.md and validation 0063.

### P06 — Manual audio choices are not persisted or restored (high)

Rust keeps an in-memory desired track but never saves the last/per-channel
name/index settings or supplies an index fallback. Python prioritizes channel
memory and restores renamed tracks by slot. Evidence: Python `main.py:7370`,
`main.py:7431`, `internal_player.py:2472`, `internal_player.py:2989`;
Rust `crates/iptv-app/src/main.rs:5836`, `main.rs:7010`, `main.rs:7046`.
Check: choose different tracks for A/B, restart and revisit, then rename A's
chosen track while retaining its slot; keep the intended selection.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-playback.md and validation 0063.

### P07 — Late audio-track publication is never retried (medium)

Rust marks tracks ready before its first fetch, so an initially empty track list
is final. Python retries publication/preference matching for about ten seconds.
Evidence: Python `internal_player.py:2427`, `internal_player.py:2528`,
`internal_player.py:2943`; Rust `crates/iptv-app/src/main.rs:6883`,
`main.rs:6999`.
Check: publish tracks two seconds after Playing; populate controls and apply
the preferred selection.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-playback.md and validation 0063.

### P08 — Embedded subtitles and VLC sidecar attachment are absent (high)

Python exposes track radios/Off, cycles embedded tracks and attaches sidecars
with `add_slave`. Rust has no equivalent VLC subtitle-track API/menu; loading a
file only parses application cues. Historical MediaCtrl limitations do not
establish implementation for the now-present VLC engine. Evidence: Python
`internal_player.py:567`, `internal_player.py:2539`, `internal_player.py:2626`;
Rust `crates/iptv-app/src/main.rs:5515`, `main.rs:7302`,
`crates/iptv-vlc/src/lib.rs`.
Check: select/disable/cycle two embedded tracks; attach an SRT and verify both
VLC-rendered subtitles and spoken cues.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-playback.md and validation 0063.

### P09 — Subtitle loading blocks the UI and lacks buffer/stale guards (medium)

Python parses/indexes on a generation-guarded worker and refuses external files
during local buffering. Rust calls `load_cues` synchronously in the file handler
without that guard. Evidence: Python `internal_player.py:2597`,
`internal_player.py:2635`, `internal_player.py:2653`;
Rust `crates/iptv-app/src/main.rs:7323`.
Check: load a large subtitle file while navigating, switch playback before
delivery, and test buffered playback; remain responsive and reject stale work.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-playback.md and validation 0063.

### P10 — One-second polling skips short subtitle cues (high)

Python samples cues every 150 ms; Rust samples display/speech every second.
A cue entirely between samples is never presented. Evidence: Python
`internal_player.py:2695`; Rust `crates/iptv-app/src/main.rs:6007`,
`main.rs:6036`, `main.rs:7478`.
Check: a cue spanning 200–650 ms between one-second ticks displays and speaks.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-playback.md and validation 0063.

### P11 — Subtitle review reads future cues and cannot traverse shown history (medium)

Rust Previous always computes current-minus-one, Next can read future text,
Current loses the last cue during gaps, and Recent lists the file's final 20
cues even before playback. Python maintains a cursor bounded by shown history.
Evidence: Python `internal_player.py:2767`, `main.py:7508`,
`subtitle_speech.py:258`, `subtitle_speech.py:369`;
Rust `crates/iptv-app/src/main.rs:3027`, `main.rs:7233`.
Check: repeated Previous walks backward; Next stops at the latest shown cue;
review starts empty and retains shown text during gaps.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-playback.md and validation 0063.

### P12 — Player subtitle review bypasses the selected speech backend (medium)

Despite validation 0055's shared-route claim, player `announce_cue` calls the
live region directly instead of `speak_cue_text`. Python uses its subtitle
manager/sink. Evidence: Python `internal_player.py:2795`;
Rust `crates/iptv-app/src/main.rs:7251`.
Check: select application voice, disable automatic speech, invoke player
Current/Previous/Next; hear exactly one result through that voice.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-playback.md and validation 0063.

### P13 — Player shortcut actions and volume steps differ (medium)

Python A cycles audio tracks, S cycles embedded subtitles, Up/Down change volume
by 2 and Ctrl+Up/Down by 5; arrows in the track choice retain native behavior.
Rust A focuses the choice, S opens a file, and volume steps by 10 with no fast
alias. Evidence: Python `internal_player.py:2240`;
Rust `crates/iptv-app/src/main.rs:7193`.
Check: compare defaults and remapped equivalents, including arrows inside the
focused track choice.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-playback.md and validation 0063.

### P14 — Stop leaves the sleep deadline and fade active (medium)

Python manual Stop cancels the timer and restores fade gain. Rust `player_stop`
leaves that state intact, affecting a subsequently selected stream. Evidence:
Python `internal_player.py:894`; Rust `crates/iptv-app/src/main.rs:6797`.
Check: Stop during a sleep fade, play another channel and cross the old deadline;
the new stream must retain its volume and continue playing.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-playback.md and validation 0063.

### P15 — Saved VOD positions resume without the choice dialog (medium)

Python offers Resume, Beginning and Cancel before switching playback. Rust
always supplies the saved position. Evidence: Python `internal_player.py:3058`,
`resume_dialogs.py:13`; Rust `crates/iptv-app/src/main.rs:5017`.
Check: all three choices work; Cancel retains the current stream and Beginning
clears the selected bookmark.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-playback.md and validation 0063.

### P16 — A failed resume seek can overwrite the useful checkpoint (high)

Python protects the old checkpoint until restoration succeeds and reports a
failure after 60 seconds. Rust consumes `pending_resume_ms` on one seek attempt
then checkpoints every second without confirmation. Evidence: Python
`internal_player.py:3099`; Rust `crates/iptv-app/src/main.rs:6878`,
`main.rs:6007`, `main.rs:7511`.
Check: reject the initial seek; preserve the prior bookmark and report failure
instead of deleting/replacing it through the early-position rule.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-playback.md and validation 0063.

### P17 — Recent/previous channels lose persisted stable history (medium)

Python persists a stable-key 20-entry MRU and resolves current catalogue entries.
Rust stores 50 session-only channel clones, deduplicates consecutive URLs only,
and updates a separate Previous slot on just one play route. Evidence: Python
`main.py:2912`; Rust `crates/iptv-app/src/main.rs:5010`, `main.rs:8495`,
`main.rs:4933`, `main.rs:3747`.
Check: A/B/A creates two history rows; restart retains them; renewed URLs resolve
fresh entries; repeated Previous toggles A/B across provider and recent routes.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-playback.md and validation 0063.

### P18 — Blocking VLC open/stop operations run on the UI thread (high)

Python serializes transport work through `VLCControl` and tears down in a worker.
Rust calls load/stop directly from handlers/timers. Evidence: Python
`internal_player.py:269`, `internal_player.py:923`, `internal_player.py:3328`;
Rust `crates/iptv-app/src/main.rs:5125`, `main.rs:5273`, `main.rs:6809`,
`crates/iptv-vlc/src/lib.rs:316`, `lib.rs:355`.
Check: inject slow open/stop operations; maintain keyboard response and ordered
provider handoff. No real-world latency measurement is claimed here.

**Fix:** Fixed with one residual: the VLC engine's final release at app exit still runs on the UI thread (the handle is created once and never replaced). See docs/validation/batch-playback.md.

### P19 — Player menu labels and fullscreen behavior are incomplete (low)

Python labels commands with effective shortcuts, hides controls in fullscreen,
changes the button name and exits fullscreen with Esc. Rust labels only
Play/Pause with its shortcut and otherwise toggles the frame flag. Evidence:
Python `internal_player.py:547`, `internal_player.py:2293`,
`internal_player.py:3025`; Rust `crates/iptv-app/src/main.rs:5483`,
`main.rs:6082`, `main.rs:7218`.
Check: remap commands and inspect labels; enter fullscreen, inspect state/name,
then exit with Esc.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-playback.md and validation 0063.

### P20 — Elapsed playback/debounce timing depends on the wall clock (medium)

Python uses monotonic time for launch debounce, recovery and sleep. Rust passes
`SystemTime` milliseconds into launch/session timing. Clock corrections can
change elapsed deadlines. Evidence: Python `external_player.py:70`;
Rust `crates/iptv-app/src/main.rs:4997`,
`crates/iptv-core/src/playback.rs:453`.
Check: move wall time forward/backward during playback; retries, sleep and
same-stream debounce retain elapsed-time semantics.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-playback.md and validation 0063.

### P21 — Imported Python playback bookmarks are not consumed (high)

Python saves `playback_bookmarks`; Rust reads/writes `resume_positions` with a
different key and record shape. Preserving unknown settings during import does
not make those saved positions available. This is separate from the known
catalogue-only locator restriction in validation 0041. Evidence: Python
`playback_resume.py:28`, `internal_player.py:3063`;
Rust `crates/iptv-app/src/main.rs:511`, `main.rs:558`.
Check: import a synthetic Python bookmark, resolve the refreshed movie/episode
and offer its saved position without losing source identity or credentials.

## Recording, DVR, catch-up, downloads and reminders

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-playback.md and validation 0063.

### R01 — Work created after idle does not restart its polling timer (high)

Rust stops the completion timer when idle. Adding reminders/DVR jobs or using
Recordings > Start does not restart it; context Record does. Python starts a
dedicated reminder timer and ensures the DVR scheduler runs when adding work.
Evidence: Python `planning_ui.py:159`, `main.py:3277`;
Rust `crates/iptv-app/src/main.rs:762`, `main.rs:3903`, `main.rs:4602`,
`main.rs:4620`, `main.rs:4728`.
Check: after all loaders settle, create the first reminder, job and menu
recording; each fires/completes without an unrelated action restarting polling.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-recording.md and validation 0063.

### R02 — Catch-up downloads copy manifests instead of capturing media (high)

Rust copies the archive HTTP response verbatim into a `.ts` file. An HLS archive
therefore saves playlist text rather than its segments. Python probes direct
files and otherwise performs finite-duration ffmpeg capture. Evidence: Python
`main.py:8317`, `main.py:8427`, `recorder.py:1117`;
Rust `crates/iptv-app/src/main.rs:8926`,
`crates/iptv-core/src/downloads.rs:83`.
Check: download a completed HLS programme and verify playable programme media,
not a manifest under a media extension.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-recording.md and validation 0063.

### R03 — Same-title downloads overwrite existing recordings (high)

Rust names files only from sanitized title plus `.ts` and uses `File::create`.
Same-title episodes/channels or a repeated download can truncate prior output.
Python includes channel/airtime and reserves collision-free paths. Evidence:
Python `main.py:8285`, `main.py:8410`, `recorder.py:1086`;
Rust `crates/iptv-app/src/main.rs:8935`,
`crates/iptv-core/src/downloads.rs:103`.
Check: download two programmes named News and repeat one request; preserve
earlier files and refuse duplicate active captures.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-recording.md and validation 0063.

### R04 — Archive downloads ignore output format and schedule padding (medium)

Rust requests exact programme start/stop and always writes `.ts`. Python applies
configured pre/post padding and the media-specific recording preset. Evidence:
Python `main.py:8232`, `main.py:8299`, `main.py:8427`;
Rust `crates/iptv-app/src/main.rs:8878`, `main.rs:8936`.
Check: select MP4 or audio output with nonzero padding; verify requested window,
extension and actual container/audio contents.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-recording.md and validation 0063.

### R05 — Failed transfers leave partial final files and lack recovery (high)

Rust writes directly to the final path and removes it only on explicit cancel.
Read/write errors leave partial files; clean early EOF has no media-duration
check. Python stages `.part` files, retries transient failures with backoff and
detects short successful captures. Evidence: Python `recorder.py:1114`,
`recorder.py:1380`, `main.py:8588`, `main.py:8630`;
Rust `crates/iptv-core/src/downloads.rs:103`, `downloads.rs:109`,
`downloads.rs:115`.
Check: interrupted reads, 503 and clean early EOF yield honest retry/failure
outcomes without a misleading completed file.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-recording.md and validation 0063.

### R06 — Downloads is a static snapshot with ambiguous cancellation (medium)

Rust builds window rows once; polling updates stored rows without rerendering.
Cancel ignores selection and uses one shared flag, reset by starting another
request. Python associates progress/cancel with each capture. Evidence: Python
`main.py:8470`, `main.py:8740`, `main.py:8896`;
Rust `crates/iptv-app/src/main.rs:8956`, `main.rs:8981`, `main.rs:9024`,
`crates/iptv-core/src/downloads.rs:64`, `downloads.rs:72`.
Check: leave Downloads open through progress/completion; cancel one selected
request while another waits, then start a third without reviving canceled work.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-recording.md and validation 0063.

### R07 — Exit does not finalize recording children; stop cannot escalate (high)

Rust main close cancels browsing/buffers/timer but never stops or waits for
recordings. The engine has no shutdown cleanup; stop writes `q` and ignores
failure. Python finalizes captures and escalates wait/terminate/kill. Evidence:
Python `main.py:4004`, `recorder.py:1405`, `recorder.py:1437`;
Rust `crates/iptv-app/src/main.rs:682`,
`crates/iptv-core/src/recording.rs:421`.
Check: exit during direct/buffered capture, including ffmpeg ignoring stdin;
leave no orphan provider connection and persist the interrupted job outcome.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-recording.md and validation 0063.

### R08 — Storage reserve is bypassed on Windows; writeability is unchecked (high)

Rust free-space lookup returns `None` outside Unix, and the caller skips the
reserve gate. Its GNU `df` flags also need a macOS implementation. No flushed
write probe protects stream opening. Python checks destination creation,
writeability and known free space first. Evidence: Python
`recording_storage.py:27`, `recorder.py:1051`;
Rust `crates/iptv-core/src/recording.rs:482`,
`crates/iptv-app/src/main.rs:1429`, `main.rs:3345`.
Check: low-space, unknown-space and unwritable Windows destinations refuse
capture before opening upstream. Validations 0030/0034 overstate this gate's
platform coverage.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-recording.md and validation 0063.

### R09 — Empty exit-zero recordings are reported as saved (high)

Rust trusts ffmpeg exit status and marks DVR completion before measuring output;
zero bytes can be announced as saved. Python requires nonempty output and reports
Nothing was recorded. Evidence: Python `client_recording.py:18`,
`client_recording.py:77`; Rust `crates/iptv-core/src/recording.rs:436`,
`crates/iptv-app/src/main.rs:1865`, `main.rs:1878`.
Check: exit-zero/no-file and zero-byte captures never become Completed/saved.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-recording.md and validation 0063.

### R10 — Recording does not use the heard/preferred audio track (medium)

The adapter supplies default ffmpeg audio options despite core selection fields.
Python derives the playing/remembered/described track, probes if needed and
passes it into capture. Evidence: Python `main.py:3690`, `main.py:3780`,
`main.py:3824`, `main.py:8444`; Rust `crates/iptv-app/src/main.rs:1466`.
Check: choose audio description and capture manually, by DVR and from catch-up;
audio-only output contains that track and video output defaults to it.

**Fix:** Fixed with one bound: the preferred-audio probe is capped at 6 s where Python allows 20 s. See docs/validation/batch-recording.md.

### R11 — Separate radio/video recording presets are unused (medium)

Rust captures through legacy `recording_format` despite storing audio/video
preferences. Python classifies media and uses the corresponding preset across
entry points. Evidence: Python `main.py:3479`, `main.py:4112`, `main.py:4143`;
Rust `crates/iptv-core/src/config.rs:64`,
`crates/iptv-app/src/main.rs:1390`, `main.rs:3855`, `main.rs:4621`.
Check: keep different radio/TV presets across manual, scheduled and archive work.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-recording.md and validation 0063.

### R12 — Simultaneous reminders are marked delivered without all being spoken (high)

Rust marks every due row Notified but announces only the first string. Python
acknowledges one delivered reminder at a time. Evidence: Python
`planning_ui.py:199`, `planning_ui.py:220`;
Rust `crates/iptv-app/src/main.rs:1331`, `main.rs:1350`.
Check: three simultaneous reminders each deliver exactly once.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-recording.md and validation 0063.

### R13 — Reminder lead editing, state and delivery actions are missing (medium)

Rust creates only five-minute reminders and offers Delete/Close. Python supports
0–120-minute lead choice/editing, status, refresh, Clear All and Open Channel
when a reminder is delivered. Evidence: Python `planning_ui.py:20`,
`planning_ui.py:29`, `planning_ui.py:210`, `planning_ui.py:241`;
Rust `crates/iptv-app/src/main.rs:4604`, `main.rs:8528`.
Check: create/edit zero- and 30-minute leads, inspect Notified/Expired states,
clear reminders and open the uniquely resolved notified channel.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-recording.md and validation 0063.

### R14 — Channel context scheduling/reminding targets the wrong programme (medium)

Rust schedules the airing programme immediately instead of opening Python's
seven-day chooser. Set Reminder also takes the airing programme, which the
reminder builder rejects as already started. Evidence: Python `main.py:2768`,
`main.py:2782`, `main.py:2838`; Rust `crates/iptv-app/src/main.rs:4730`,
`main.rs:4734`, `crates/iptv-core/src/reminders.rs:115`.
Check: Schedule opens upcoming programme choices; Reminder operates on a future
programme rather than submitting an impossible current-programme request.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-recording.md and validation 0063.

### R15 — Duplicate schedules and creation-time overlap confirmation are absent (medium)

Rust appends every requested job. Python detects duplicate channel/windows and
asks before overlaps. Start-time connection conflict handling in validations
0058/0059 does not replace these checks. Evidence: Python `main.py:3259`,
`main.py:3270`; Rust `crates/iptv-app/src/main.rs:4651`,
`crates/iptv-core/src/dvr.rs:369`.
Check: scheduling one programme twice creates one job; overlaps get Python's
explicit confirmation before adding work.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-recording.md and validation 0063.

### R16 — Recurrence cannot be authored through the GUI (medium)

Rust job construction hardcodes `repeat: "none"`; Python exposes Daily, Weekly
and Series from programme rows. Core imported daily/weekly support does not
provide authoring parity. Series scanning itself was already deferred in 0014.
Evidence: Python `main.py:9329`, `dialogs_misc.py:1100`;
Rust `crates/iptv-app/src/main.rs:4648`, `main.rs:4821`.
Check: create daily/weekly jobs and observe following occurrences; Series follows
matching guide episodes once the previously recorded scanning gap is closed.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-recording.md and validation 0063.

### R17 — Shutdown arming/cancel/revalidation differs (high)

Rust retains the armed flag and old session-wide recorded state across toggles;
cancel only stops the countdown and leaves `shutdown_attempted` set. Python
resets activity on arming and disables shutdown after cancel/use. Evidence:
Python `main.py:3885`, `main.py:3980`, `main.py:3989`;
Rust `crates/iptv-app/src/main.rs:1860`, `main.rs:1901`, `main.rs:3836`,
`main.rs:9374`.
Check: arming after an old completed capture must not immediately shut down;
cancel clears the saved option, and explicit rearming allows a new countdown.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-recording.md and validation 0063.

### R18 — Stop Recording loses the single-active-recording fallback (low)

Rust requires the selected channel to be recording. Python stops the sole
active recording even after selection moves, and handles zero/multiple cases.
Evidence: Python `main.py:3857`; Rust `crates/iptv-app/src/main.rs:3786`.
Check: record A, select B, invoke Stop Recording; stop A as Python does.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-recording.md and validation 0063.

### R19 — Scheduled Recordings omits fields and keyboard batch operations (low)

Rust rows omit the complete padded window/format, replace all items on refresh
and operate on one selection. Python preserves selection and supports Ctrl+A,
Delete and context cancel/delete. Evidence: Python `main.py:9013`,
`main.py:9142`, `main.py:9200`; Rust `crates/iptv-app/src/main.rs:8694`,
`main.rs:8727`, `main.rs:8774`.
Check: retain a later selected job on refresh, inspect format/end time and
remove selected completed jobs through the keyboard.

**Fix:** Fixed with one omission: Python's right-click menu was not ported; buttons and keyboard (Ctrl+A, Delete) cover the actions. See docs/validation/batch-recording.md.

### R20 — Catch-up browsing omits dates, descriptions and return behavior (medium)

Rust shows only clock/title rows and Play/Download/Close buttons, without
programme descriptions or row activation/context bindings. Python dates rows,
exposes descriptions, supports Enter/Apps-key actions and restores the programme
selection after player close. Evidence: Python `dialogs_misc.py:473`,
`dialogs_misc.py:547`, `main.py:7249`, `main.py:7263`;
Rust `crates/iptv-app/src/main.rs:8805`, `main.rs:8829`.
Check: distinguish same-time episodes on different dates, read descriptions,
activate through Enter/context menu and return to the played programme.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-recording.md and validation 0063.

### R21 — Buffered recording contains an overlapping mutable borrow (high)

`start_recording` holds the temporary `buffers.borrow_mut()` from an `if let`
condition while borrowing the same `RefCell` again to store a successful lease.
The workspace uses Rust 2021, where that first temporary survives the body.
Evidence: Rust `Cargo.toml:9`, `crates/iptv-app/src/main.rs:1451`,
`main.rs:1459`, `crates/iptv-app/src/buffer.rs:652`.
A standalone Rust 1.97.1, edition-2021 `catch_unwind` probe reproducing the same
owned-return/second-borrow pattern confirmed the panic on 2026-11-03. This proves
the language-lifetime mechanism; the full app path was not executed.
Check: start recording with an existing matching buffer and successful cursor;
store/release its lease without panic and without an extra provider connection.

## Platform integration, casting, localization and help

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-recording.md and validation 0063.

### S01 — Single-instance handover and restore are absent (high)

Python locks per user and sends a second launch to the existing window. Rust
always creates another application, allowing duplicate schedulers, config writers
and provider connections. Evidence: Python `main.py:9417`,
`single_instance.py:58`; Rust `crates/iptv-app/src/main.rs:37`.
Check: launch again while minimized to tray; restore the original window without
another scheduler, and handle an unresponsive existing instance clearly.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-platform.md and validation 0063.

### S02 — Portable settings lookup ignores the executable directory (high)

Python prioritizes settings beside the executable and retains the loaded
writable location. Rust tries per-user settings first, then the working
directory, ignoring executable-relative portable settings. Evidence: Python
`options.py:289`, `options.py:351`;
Rust `crates/iptv-core/src/config.rs:428`,
`crates/iptv-app/src/main.rs:588`.
Check: launch through Explorer/a shortcut with another working directory and
both config copies present; portable settings win and persist, with a working
fallback when the portable directory is unwritable.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-platform.md and validation 0063.

### S03 — Header-bearing casts advertise an unreachable loopback URL (high)

The app proxies streams needing headers, but the proxy binds and advertises
`127.0.0.1`; a TV connects to itself. Python selects/binds a LAN address.
Evidence: Python `stream_proxy.py:1304`;
Rust `crates/iptv-app/src/main.rs:2214`,
`crates/iptv-core/src/proxy.rs:40`.
Check: cast protected HLS/TS to another host; the advertised URL and rewritten
segments are reachable there and preserve required headers.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-platform.md and validation 0063.

### S04 — Casting guesses MIME from an opaque proxy URL (medium)

Rust turns a stream into `/s/<hash>`, then infers format from that path. UPnP
falls back to `audio/mpeg`; Roku to `video/mp4`, losing HLS/TS metadata. Python
retains the probed MIME independently of the playback URL. Evidence: Python
`casting.py:1006`; Rust `crates/iptv-core/src/proxy.rs:78`,
`crates/iptv-core/src/cast.rs:875`.
Check: inspect UPnP/Roku requests for protected HLS/TS; preserve the original
media format in renderer metadata.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-platform.md and validation 0063.

### S05 — Logs and copied diagnostics are placeholders (medium)

Python opens actual logs and copies operational details plus a redacted,
bounded 128 KiB tail. Rust opens the settings directory and copies version,
OS and config path while saying Log and debug information copied. No logging
sink/tracing dependency is configured in the Rust crates. Evidence: Python
`main.py:4173`; Rust `crates/iptv-app/src/main.rs:3962`.
Check: trigger a harmless synthetic media/guide error; expose its real log,
copy redacted useful diagnostics and report clipboard failure.

**Fix:** Fixed with one deviation: logs live in the config directory's logs folder on all platforms. See docs/validation/batch-platform.md.

### S06 — UI translation bypasses and language activation remain (medium)

Even with a catalogue installed, main/player controls bypass translation, and
formatting dynamic status messages before lookup prevents template keys from
matching. The language selector only saves its choice; Python activates it for
new dialogs and its confirmation. Evidence: Python `main.py:4632`;
Rust `crates/iptv-app/src/main.rs:49`, `main.rs:299`, `main.rs:1874`,
`main.rs:3877`, `main.rs:5381`.
Check: install a real Spanish catalogue; main/player/dialog labels and messages
containing names/counts translate, interpolating after lookup. Changing language
affects new dialogs immediately. Missing bundled resources are a separate known
limit below, not a reason to declare these call sites translated.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-platform.md and validation 0063.

### S07 — Automatic update checks run only at startup (medium)

Python schedules startup and hourly checks. Rust calls its automatic check only
at startup; toggling the option only changes configuration. Evidence: Python
`main.py:1235`; Rust `crates/iptv-app/src/main.rs:4404`, `main.rs:3934`.
Check: start a session with automatic updates enabled; subsequent hourly checks
occur, and disabling the option suppresses them.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-platform.md and validation 0063.

### S08 — External help discovery depends on the working directory (low)

Rust's guide lookup promises executable-relative behavior but returns relative
`docs/help` unless overridden by an environment variable. Python resolves its
installation/bundle directory. Evidence: Python `user_guide.py:191`;
Rust `crates/iptv-core/src/guide.rs:383`.
Check: install translated guides beside the executable, launch elsewhere and
open the appropriate guide through F1 instead of embedded English fallback.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-platform.md and validation 0063.

### S09 — What's New has no localized notes/history path (low)

Python localizes recent notes and supports older-history expansion. Rust inserts
an English constant directly; its `notes_gettext` helper has no app caller.
Evidence: Python `dialogs_misc.py:128`;
Rust `crates/iptv-app/src/main.rs:9309`,
`crates/iptv-core/src/i18n.rs:542`.
Check: show notes for the running Rust version, translated recent entries and
keyboard-reachable older history.

**Fix:** Landed in the parity integration; regression evidence in docs/validation/batch-platform.md and validation 0063.

### S10 — Context-sensitive F1 help stops at the main controls (medium)

Python installs application-wide character/help hooks and resolves focused
controls, parent dialogs and menu topics. Rust binds F1 only to six main-window
controls; other windows lack corresponding topic routing. A main Help menu
command does not supply the player's, dialog's or highlighted menu item's topic.
Evidence: Python `main.py:616`, `main.py:1153`, `user_guide.py:273`,
`user_guide.py:305`; Rust `crates/iptv-app/src/main.rs:880`, `main.rs:9083`.
Check: F1 inside the player, provider form, Channel EPG and an open menu reaches
the relevant section without first returning to the main window.

## Previously documented limits still relevant

These are retained context, not additional newly discovered entries. Where a
later validation fixed an older limitation, the later evidence takes precedence.

- **Casting engine scope:** Chromecast, AirPlay/RAOP, native Sonos control,
  Yamaha side-channel, mDNS/Kodi discovery and relay/transcode compatibility
  remain outside the connected UPnP/Roku path. See validations 0017/0032 and
  `crates/iptv-core/src/cast.rs:869`. Protocol helper functions alone are not
  selectable devices. The repository requires porting/syncing Caster as the unit.
- **Translation resources:** the Rust tree contains no `.mo` catalogues and
  only English help, versus Python's 14 help languages. Validations 0018/0031
  already record this resource limitation. `PACKAGING.md` describes locale
  resources, but current release staging does not copy a locale directory.
  Translation-engine tests do not prove translated packages. Workflows were not
  changed during this audit.
- **VOD:** the text filter and provider/movie/episode resume identity limits
  remain as recorded in validation 0015; catalogue-only bookmark resolution
  without provider locators remains as recorded in 0041. P21 adds the distinct
  failure to consume imported Python bookmark records.
- **External player/subtitle formats:** MPV IPC reuse and its application-owned
  subtitle session remain as in 0010. BOM-less non-UTF subtitle decoding and the
  incomplete named-HTML-entity set remain as in 0016. VLC generic-header/Linux
  embedding limits remain as in 0028.
- **Live buffer and provider connection sharing:** validation 0045 records
  live-buffer casting, provider-connection gating, redirect-base and relaunch
  limitations. Recording shares an already-running local buffer; it does not
  provide Python's unbuffered playing-stream recorder relay (0014/0045).
  Scheduled conflict handling in 0058/0059 does not add every manual/catch-up
  busy-account check.
- **DVR series and diagnostics:** guide-wide series scanning and recording
  logs/statistics/problem-times were deferred in 0014. Current capture discards
  ffmpeg stderr (`crates/iptv-core/src/recording.rs:410`). R16 records the separate
  inability to author even core-supported daily/weekly recurrence through the UI.
- **Stalker cookies:** Python uses a cookie processor; standalone Rust requests
  use a manual Cookie header. Validation 0012 already records this compatibility
  boundary; no live portal evidence was collected here.
- **Installation/update flow:** Python installer integration and macOS manual
  installation limits remain documented. Signed ship-shape updates, settings
  encryption/interchange, import recovery copies, application voice/device-loss
  handling and tray menu content have later implementation records. They must
  not be reported as wholly absent because an earlier audit deferred them.

## Targeted uncertainties and evidence still needed

- Manual Stalker recording does not use the correlated DVR resolver. Empty URLs
  are refused; unresolved nonempty portal commands may reach ffmpeg. Compare
  Python `main.py:3578` with Rust `crates/iptv-app/src/main.rs:1370` and test actual
  portal channel representations before declaring every Stalker recording broken.
- Rust tray restore calls show/raise; Python also explicitly restores foreground
  and channel-list focus. Compare Rust `crates/iptv-app/src/main.rs:2508` with
  Python `main.py:5266`, `main.py:5341`. This is a focus verification target, not
  proof of a failure with NVDA.
- No real provider, renderer, device-loss, installer/updater execution or
  owner-heard NVDA check was run. Performance findings identify synchronous
  scans/copies; they do not invent measured latency numbers.
- No full Cargo/Python suite was run for these documentation-only edits. The
  standalone Rust borrow probe is narrower than an application regression test.
- The current Python reference has moved substantially beyond the old pinned
  oracle. This broad audit finds concrete gaps; it is not proof that all other
  Python behavior matches or that no further small gaps remain.

## Suggested repair order

1. Prevent data/process damage: R03/R05/R07/R08/R09/R17/R21, B04 and P16.
2. Restore dependable core actions: R01/R12, B01–B03/B10/B12, E01/E02/E04/E05,
   P01–P04 and S01/S03.
3. Move large-catalogue work off the UI thread (B19/E12/P09/P18), preserving
   cancellation, generation checks and stable unchanged rows.
4. Complete remaining behavior through small source-matched fixes and focused
   regression cases. Run live and owner-heard lanes separately; update the
   replacement ledger without treating source inspection as acceptance.

## Audit verification

The documentation update is checked for whitespace errors, valid local document
links, unique sequential finding IDs and source references that exist at these
snapshots. These checks validate the ledger's structure, not the proposed fixes.
Historical automated passes in earlier validations were not rerun or promoted
to current feature-parity evidence.

Checks run on 2026-11-03: 85 unique sequential IDs; 400 source references with
existing files and valid line numbers; six local document links/anchors; balanced
code fences; matching reference revisions; only the four intended documentation
files changed; Python tracked files unchanged. Whitespace checks include the new,
untracked audit file. The separate Rust 2021 borrow probe passed as described in
R21. No application build or full-suite pass is claimed.

**Fix:** Fixed with a recorded limit: wxdragon exposes no API to identify an open menu's highlighted item, so F1 from an open menu falls back to the last focused control's topic. See docs/validation/batch-platform.md.

