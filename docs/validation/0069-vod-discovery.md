# 0069 — VOD discovery and item details (spec 0006, issue #173)

Farhan Khan's issue #173 asked for accessible VOD discovery in
the Rust client, specified in `docs/specs/0006-vod-discovery.md`
(his branch commit cherry-picked onto this work; the
chief-of-staff note in `docs/chief-of-staff/` records the design
decisions he confirmed with Brandon, including where global
search and Back behavior intentionally differ from the Python
client's observed behavior). This validation records what was
built, the choices the spec left open, and the automated
evidence. Brandon's owner-heard NVDA + physical keyboard
acceptance is a separate step, exactly as the spec states — it
is not part of this gate and is not claimed here.

## What was built

Core (`iptv-core`, wx-free, per the spec's boundary):

- `vod::search_catalog` — Enter-applied title search over
  top-level movies and series across ALL groups of the loaded
  catalogue, in catalogue group order and within-group order.
  Case-insensitive substring on the trimmed query; a blank query
  yields no hits (the adapter treats it as clearing); episode
  titles are never searched; the catalogue is not mutated.
- Source identity: `VodItem` now carries `source_id` and
  `source_label`. M3U items take their identity from the
  channel's stable playlist identity (the `playlist-id`
  attribute / `m3u:<sha256(location)>` scope id) — never from
  the stream URL — and it propagates to series folders and
  their episodes. Xtream items carry the provider id and the
  account's label. `ProviderAccount::source_label` is the
  friendly account name when set, else the base-URL host.
  M3U labels come from `sources::source_display_label` keyed by
  scope id (friendly playlist name, else host, else filename);
  an unmapped source falls back to the generic "Unknown
  source" label.
- Credential hygiene: `source_display_label` and
  `providers::host_of` now strip URL userinfo, so a label can
  never contain credentials. Row labels and announcements are
  built only from title, type, group, and source label.
- `vod::item_details` / `VodDetails::lines` — the detail
  surface: title, type, group, source, the EPG description when
  one is already available, and the stream URL, omitting
  unavailable fields. The URL appears only in these lines.
- The VOD worker retains the newest built catalogue and
  answers `VodJob::Search` from it at the build generation, so
  a catalogue rebuild supersedes an in-flight search (the
  existing generation/cancellation pattern).

Adapter (`iptv-app`, the VOD window):

- A labelled search field ("Search VOD", `ProcessEnter`) above
  the group list. Enter applies the query; editing alone
  changes nothing; focus stays in the field; results arrive
  through the existing 50 ms poll. Result rows read "Title,
  Movie/Series, Group, Source"; a concise "{count} results for
  {query}." / "No results for {query}." status follows.
- While a query is active the previously selected group is
  retained; clearing the query (Enter on a blank field)
  restores that group, its list selection, and its unfiltered
  items. Choosing a group while a query is active also leaves
  search mode (field and filter reset) — the one behavior the
  spec did not pin down; recorded here as the chosen reading.
- A labelled read-only "Item details" control updates on every
  selection change without moving focus. Its description comes
  from `guide_now_next` (the current programme of the item's
  channel) only when already available; nothing is fetched.
- Drill-in history now stores (list, selected row): Back from
  a series returns to the global filtered list when a query is
  active (else the group's items) with the same series row
  reselected. A catalogue reload clears the query, the field,
  and the drill-in stack (reloads supersede search results).

## Tests

Core (iptv-core lib suite, 285 tests green):

- `search_matches_top_level_items_across_groups` — matching,
  trimming, case folding, all-groups reach, catalogue order,
  movie + series inclusion, episode exclusion, blank query,
  no catalogue mutation.
- `m3u_source_identity_and_labels_propagate` — scope identity
  reaches movies, series folders, and episodes; friendly label
  wins; unmapped source gets the generic label; a credential-
  bearing stream URL contributes neither userinfo nor password
  to any row label.
- `details_carry_only_available_fields_and_the_url` — a movie's
  details carry the trimmed description and the URL; a series
  folder shows exactly title/type/source (no URL, no
  description).
- `worker_search_answers_from_the_retained_catalogue` — the
  worker builds, retains, and answers a search at the build
  generation.
- `display_label_strips_url_userinfo` (sources) — friendly
  name wins and is trimmed; host labels drop userinfo and keep
  the port; filename fallback.
- `host_of` userinfo stripping (providers) rides the same
  change.

Adapter behaviors (Enter-to-apply, focus retention, accessible
names, concise announcements, details without focus movement,
Back restoration) are bound in wx event handlers, matching how
the rest of this window is wired; they are covered by the
owner-heard NVDA acceptance pass, which the spec keeps
separate from the automated gate.

Gates: `cargo fmt --all -- --check` clean; `cargo clippy
--workspace --all-targets -- -D warnings` clean; `cargo test
--workspace` green (iptv-core 285 lib tests, iptv-app 16);
playlist parity against the pinned Python oracle PASS;
`git diff --check` clean.
