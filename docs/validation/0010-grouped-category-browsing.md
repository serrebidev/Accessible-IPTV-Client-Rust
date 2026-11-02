# Issue #10: grouped category browsing

Implemented directly on `agent/ticket-10`, based on `7df5fb77f979f9cffd94c4195cf7b5ce6c78f61b`.
Use the commit containing this record as the validated revision. No issue,
remote branch, PR, release, or Python product was changed.

## Behavior and boundaries

- The existing wx-free `BrowseModel` owns typed category keys: All Channels
  first, then distinct group names in case-sensitive lexical order. This
  matches Python sorting, including its uppercase/lowercase ordering.
- The parser trims metadata. Missing, empty and whitespace-only group values
  join explicit Uncategorized entries. No blank category is emitted.
- Category rows retain playlist source order. Name search keeps its existing
  ranking within the active category; clearing search restores that category.
- Existing snapshot/source-index identity preserves selected and item-focused
  channels across projections, including duplicate names and URLs. An absent
  selection falls back to row zero only for nonempty results. This does not
  introduce persistent/favorite identity or change the existing new-playlist
  first-row selection policy. Favorites remain tickets #11/#12.
- A replacement preserves the category key if present, otherwise falls back to
  All Channels when published. Failure/cancellation retains the usable source.
  A cancelled replacement cannot commit its provisional category fallback.
- Category/name filtering uses the existing single-flight worker and generation
  gate, with bounded cancellation checkpoints. No channel scans run in native
  event callbacks. Previous usable rows remain while a request is pending;
  obsolete completions cannot replace the current snapshot.
- Native Categories is explicitly named, with Cancel loading -> Categories ->
  Search channels -> Channels tab order. Arrows use native Choice behavior.
  Category changes and background completion never request keyboard focus.
  Completion restores item selection/focus through the existing adapter path.
- Empty projections have zero rows and no selectable status placeholders.
  Category/count/empty text uses the existing status and live-region path.
- The All sentinel is distinct from a literal All Channels group, whose native
  display label is All Channels (playlist group). A flat Choice is sufficient
  for this local single-playlist seam; provider hierarchy is outside scope.

## Python evidence

Pinned Python revision: `9aa89e053225ef2c83c4f3659775a00723012ff7`.
Inspected `main.py` grouping/load wiring, `_refresh_group_ui`,
`_source_for_group`, category selection, `channel_widgets.py`, `favorites.py`,
`tests/test_playlist_scope.py` and missing-group tests in `test_m3u_plus.py`.
The fixture oracle reads committed bytes, extracts actual parser/grouping/
sorting/projection functions with AST, and uses scope-aware reference group
keys to distinguish literal group names from sentinels. It never imports UI
modules, reads user config, or fetches a stream. See the fixture README for
provenance and the synthetic category fixture.

## Automated gate, 2026-11-01

Windows; Rust 1.97.1; dependency/toolchain baseline unchanged. Every Rust command
used `LIBCLANG_PATH=C:\Program Files\LLVM\bin` and prepended
`C:\Program Files\CMake\bin;C:\Program Files\LLVM\bin` to PATH.
`CARGO_TARGET_DIR` points to the repository's shared `target` directory.

| Command | Exit | Observed result |
| --- | --- | --- |
| `cargo fmt --all -- --check` | 0 | Clean. |
| `cargo test --workspace` | 0 | 31 tests passed; zero failed/ignored; zero doctests. |
| `cargo check --workspace` | 0 | Both crates checked. |
| `cargo clippy --workspace --all-targets -- -D warnings` | 0 | No warnings. |
| `python tools/check-playlist-parity.py <Python-checkout>` | 0 | Five fixtures, 30 searches, channel fields/order, category projections and load outcomes match. |
| `git diff --check` | 0 | Clean. |
| `cargo rustc -p iptv-app --bin iptv-app -- -o <shared-target>/debug/iptv-ticket10.exe` | 0 | Separately named Windows executable linked. Rustc reports two output-path option warnings; no source warnings. |
| `tools/smoke-foundation.ps1 -ExecutablePath <shared-target>/debug/iptv-ticket10.exe` | 0 | MSAA named/focusable Categories, initial All Channels value, native tab order, zero startup channel rows and readable status; app closed normally. |

An initial `cargo build -p iptv-app` failed with Windows access denied replacing
`target/debug/iptv-app.exe`: PID 25284 was already running that shared executable
since 17:50. It was left untouched. The separate executable above avoids that
lock; the default output remains locked until its owner closes it.

Changed scope: core category/browse/search code, native adapter, focused tests,
synthetic fixtures/goldens, parity and native smoke tools, README and this record.
No dependencies, lockfile, CI workflows, configuration, or Python files changed.
Final dirty-file set is this implementation slice only; the local commit clears
it. No additional automated implementation blocker remains.

## Manual owner-heard gate: pending

Automated native inspection checks startup semantics/tab order. Adapter unit
tests check populated/empty row naming and category status through the wx-free
snapshot. Neither proves populated native interaction or audible announcements.
Record commit, Windows/NVDA versions, observed speech and pass/fail for:

- Tab/Shift+Tab through Categories; arrow through populated categories; verify
  names/current values and that focus stays on Categories after completion.
- All Channels, News and Uncategorized in `categories.m3u`: verify source-order
  rows and duplicate-name selection preservation/fallback when changing category.
- Search within News, no matches, and clear: zero native rows on no results;
  status spoken without focus movement; clearing restores only News.
- Rapid category/search changes and replacement/cancel/failure: no obsolete
  completion, phantom row or focus theft; vanished categories fall back to All.
- System large text/high contrast and a large synthetic playlist: usable layout,
  responsive navigation and no background focus changes.

No NVDA speech, physical keyboard acceptance, 500,000-channel latency budget,
or owner acceptance is claimed by this implementation record.
