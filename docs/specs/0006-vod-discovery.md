# VOD Discovery

## Status

Design approved 2026-11-04. Implementation not started.

## Problem Statement

The VOD browser currently presents catalogue groups and their movies or series, but it has no text filter or selected-item information surface. Large catalogues make discovery slow, and the user cannot inspect available item details without starting playback.

The Rust port must keep catalogue work off the UI thread, preserve keyboard and screen-reader access, and carry stable source identity without using stream URLs as identity.

## User Stories

1. As a user, I want to find movies and series by name across the loaded VOD catalogue, so that I do not have to inspect each group.
2. As a user, I want each filtered result to identify its group and source, so that similarly named items remain distinguishable.
3. As a user, I want to inspect available information for a selected movie, series, or episode without playing it.
4. As a keyboard and screen-reader user, I want search results and details to be announced and reachable without focus being stolen.
5. As a user, I want Back from a series to restore the filtered result list and the series selection, so that I can continue browsing.

## Behavior

### Search

- Add a labelled search field to the VOD browser. Search applies only when the user presses Enter; editing the text alone does not change results.
- Match the query as a case-insensitive substring of the item title. Trim surrounding query whitespace. An empty or whitespace-only query clears the filter.
- Search all top-level movies and series across all groups in the loaded catalogue, regardless of the currently selected group. Do not search episode titles until the user opens a series.
- Show each result's title, item type, group, and source label. Preserve catalogue order within groups and group order from the catalogue.
- While a query is active, retain the previously selected group as view state. Clearing the query restores that group and its unfiltered contents.
- Keep keyboard focus in the search field when Enter applies or clears a query. Do not move focus when background work completes.
- Announce a concise result count or no-results status after applying a query. Moving through results announces the item's short title and type; group and source remain part of the accessible row name.

### Source labels and identity

- Xtream items use the configured friendly provider account name when available; otherwise use its host label.
- M3U items use the configured friendly playlist name when available; otherwise use the URL host with any user information removed, or the local filename. Use a generic source label when no safe label can be derived.
- Carry M3U source identity from the channel's stable playlist identity into VOD entries and series entries. Never key or join source identity using a resolved stream URL.
- Do not expose URL credentials, authorization data, cookies, or headers in row labels, announcements, or status messages.

### Item details

- Provide a labelled, keyboard-reachable read-only detail control for the selected item. Update it when selection changes without moving focus.
- Show the available title, item type, group, source label, EPG description when one is already available for the item, and stream URL. Omit unavailable fields rather than inventing metadata.
- Keep the complete detail text reachable by normal keyboard navigation and screen-reader reading commands. The short result announcement must not speak the full URL or description automatically.
- Treat stream URLs as sensitive display data: show only in the explicit detail control; never include them in announcements or logs.
- For a series, show its available series-level fields. Opening a series switches the item list to episodes; selecting an episode shows that episode's available fields.

### Series navigation

- Opening a series displays its episodes using the existing inline M3U or asynchronous Xtream episode path.
- Back returns to the global filtered result list when a query is active, otherwise to the selected group's items.
- Restore the same series selection and keep the search query and focus state. Background episode loading must not move focus.

## Implementation Boundaries

- Keep filtering and source-label derivation in wx-free core logic where practical; keep widget creation, focus, speech, and control updates in the wxDragon adapter.
- Reuse the existing VOD catalogue worker and existing playlist/provider source-label and stable-identity helpers. Do not perform catalogue scans or provider I/O on the UI thread.
- Source identity must be propagated from M3U channels into both flat movie entries and M3U series folders/episodes. Preserve provider identity for Xtream items.
- Search must operate on the loaded catalogue snapshot. Catalogue reloads supersede stale results using the existing generation/cancellation pattern.
- Do not add provider metadata requests, a new persistence format, or a new UI toolkit in this feature.

## Testing Decisions

- Core tests cover case-insensitive substring matching, whitespace trimming, empty-query clearing, all-group results, group and catalogue ordering, movie/series inclusion, and episode exclusion until drill-in.
- Test that filtering does not mutate catalogue data or lose the previously selected group, and that clearing restores the prior group contents.
- Test M3U stable source identity propagation for movies, series, and episodes; verify labels prefer configured names and safely fall back to URL host/local filename/generic label.
- Use synthetic credential-bearing URLs to prove credentials do not appear in labels or accessible announcements. Verify a URL appears only in the explicit detail value.
- Adapter checks cover Enter application, focus retention, accessible names/roles, concise status announcements, detail updates without focus movement, series Back restoration and selection, and asynchronous result delivery.
- Keep automated accessibility evidence separate from owner-heard NVDA and physical keyboard acceptance. Brandon's hands-on test remains required for spoken labels, traversal, detail reading, no-results announcement, focus stability, and series Back behavior.
- Run focused VOD tests, `cargo fmt --all -- --check`, `cargo test --workspace`, `cargo check --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, and `git diff --check` for implementation.

## Out of Scope

- Searching episode titles from the top-level catalogue.
- Fetching plot, cast, year, artwork, or other new provider metadata.
- Changing playback, resume identity, bookmark resolution, provider authentication, or catalogue eligibility rules.
- Persisting the filter query across application restarts.
- Adding Stalker VOD support.

## Reference and Evidence

- User-confirmed design decisions recorded 2026-11-04 in `docs/chief-of-staff/vod-discovery-2026-11-04.md`.
- `docs/validation/0015-vod.md` records the existing VOD implementation and its earlier missing-filter/detail boundary.
- `docs/feature-parity-audit.md` (2026-11-03) identifies Python v1.150.58, commit `6afe6014fa773a5489d4c43ccafad5726386d931`, as its comparison snapshot.
- The local sibling Python checkout was at `dd5fa2ef3f5bbf012393f753329a7561ec287464`, on `main`, with a shallow history. The audit-pinned commit was unavailable there (`git cat-file` failed), so Python UI details were not verified against that snapshot. Do not treat observations from the current sibling checkout as historical parity evidence.
- Relevant Rust code inspected for this spec: `crates/iptv-core/src/vod.rs`, `crates/iptv-core/src/sources.rs`, and VOD browser code in `crates/iptv-app/src/main.rs`.
