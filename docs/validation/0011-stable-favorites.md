# Issue #11: stable favorite identity

Implemented directly on `agent/ticket-11`, starting from clean revision
`7df5fb77f979f9cffd94c4195cf7b5ce6c78f61b`. The local commit containing this
record identifies the verified implementation. Requirements: published parent
issue #9 and ticket #11 in `serrebidev/Accessible-IPTV-Client-Rust`.

## Behavior and scope

`iptv_core::favorites` exposes a channel key and an ordered `FavoriteSet` with
normalization, membership, toggle outcomes (`Added`, `Removed`, `Unkeyable`),
and filtering over loaded channels. A hash set supports membership during
playlist scans; the ordered vector preserves normalized storage/toggle order.
Filtering always uses the supplied playlist order and retains matching duplicate
rows, ignoring unknown keys. An unkeyable channel leaves the set unchanged.

Keys use nonempty provider type, provider ID, stream ID, XMLTV ID and display
name in Python order, escaping backslash and pipe. Missing metadata is omitted;
plain M3U uses display name (TVG name/ID fallback). Same names in different
groups deliberately share a favorite. Keys are case-sensitive and include
display name even with provider metadata, so renames change identity. URL
refreshes do not; provider/stream identity changes do. URL, logo, group and
credential attributes are excluded. No resolved URL parsing is performed.

Normalization trims strings, drops invalid/blank/duplicate entries and preserves
first-seen order. Unknown nonblank keys remain opaque. The public storage boundary
accepts optional string entries; a future config adapter maps non-string entries
to `None` and missing/non-list configuration to an empty iterator. No persistence
format or configuration dependency was introduced.

Only a core module/export, integration tests, synthetic fixtures, a parity tool,
and documentation changed. No new dependencies, widgets, category projection,
search/recovery changes, Favorites category, toggle control, workflows or release
changes. Large-playlist filtering is a synchronous domain operation documented
for worker use; desktop scheduling belongs to later integration work.

## Reproducible Python evidence

Pinned reference: `serrebidev/Accessible-IPTV-Client` revision
`9aa89e053225ef2c83c4f3659775a00723012ff7`, especially `favorites.py` and the
identity/normalization/filter/toggle cases in `tests/test_favorites.py`.

`tools/check-favorites-parity.py` executes committed `favorites.py` from that
revision, ignoring any working-copy edits. It reads no user configuration,
imports no desktop UI and opens no URLs. Rust tests consume shared TSV inputs
and generated observable outcomes rather than Python implementation structure.
Coverage: 27 identity cases, six stored-value cases, 54 toggle/filter steps.
Fixture format and regeneration commands are in the fixture README.

Focused TDD evidence: the provider/URL test initially failed with unresolved
`favorites` import, then passed after the key implementation. The ordered-set
test initially failed with unresolved `FavoriteSet`/`ToggleOutcome` imports,
then passed after their implementation. Four new integration tests now pass,
including named duplicate-name/separator and credential-refresh regressions.

## Final automated gate (2026-11-01)

Session-only environment before every Rust command:
`LIBCLANG_PATH=C:\Program Files\LLVM\bin`;
`C:\Program Files\CMake\bin;C:\Program Files\LLVM\bin` prepended to PATH;
`CARGO_TARGET_DIR` resolved to this repository's existing root `target` directory.
No global environment or build baseline was changed.

| Command | Exit | Result |
| --- | --- | --- |
| `cargo fmt --all -- --check` | 0 | Clean |
| `cargo test --workspace` | 0 | 29 tests passed; none failed or skipped |
| `cargo check --workspace` | 0 | Both crates checked |
| `cargo clippy --workspace --all-targets -- -D warnings` | 0 | No warnings |
| `python tools/check-favorites-parity.py <Python-checkout>` | 0 | Pinned favorites goldens match |
| `python tools/check-playlist-parity.py <Python-checkout>` | 0 | Existing parser/search/recovery parity matches |
| `git diff --check` | 0 | Clean |

Reviewed dirty scope for the local commit: `crates/iptv-core/src/lib.rs`,
`crates/iptv-core/src/favorites.rs`, `crates/iptv-core/tests/favorites.rs`,
the three favorite TSV fixtures, fixture README, `tools/check-favorites-parity.py`,
and this evidence record. No implementation blockers remain. No GitHub issue
edits, pushes, merges, PRs or external writes were performed.

## Manual acceptance still open

No native desktop or owner-heard NVDA/keyboard testing ran for this widget-free
ticket. Tickets #12/#13 must integrate and validate accessible Favorites/toggle
controls, naming, Tab/Shift+Tab and shortcuts, status announcements, focus and
selection restoration, empty Favorites, rapid category/search changes, playlist
replacement, large playlists and system presentation settings. Automated domain
and adapter tests do not establish audible speech or physical keyboard behavior.
Python remains the shipping product; the replacement gate remains open.
