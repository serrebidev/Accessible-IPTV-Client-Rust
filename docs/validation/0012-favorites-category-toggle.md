# Issue #12: Favorites category and keyboard toggle

Implemented in the requested `ticket-12` worktree on `agent/ticket-12`, from
clean base `e4de2e3` (merged #10/#11). The commit containing this record identifies
the implementation. Requirements: published GitHub parent #9 and ticket #12.

## Behavior

`Category::Favorites` is a typed sentinel after All Channels, independent of its
display label and distinct from a literal playlist group called Favorites.
That group displays as `Favorites (playlist group)`. Favorites stays present when
empty, exposes zero real virtual rows, and reports explicit status guidance.

The native `Toggle favorite` button follows Channels and precedes Playlist status
in Tab/Shift+Tab order. Space/Enter on the button or its Alt+F mnemonic uses one
command handler. It toggles the focused real channel by #11's stable identity,
reports added/removed/unkeyable/no-selection outcomes through the existing
status/live-region path, and makes no keyboard-focus request. All/group toggles
leave the unchanged row projection alone. Favorites toggles schedule a fresh
worker projection with the updated immutable favorite snapshot.

Favorites rows use current playlist order, not toggle order. Selection and item
focus retain source identity within a snapshot; on refresh, a worker-built key
lookup restores matching stable identity without scanning channels on the UI
thread. Ambiguous matching namesakes use the first matching row across snapshots.
Missing identities fall back to the first real row, or no row when empty.
Absent channels disappear from the projection without deleting their in-memory
favorite keys. Failed loads keep usable rows; generations reject superseded
projections. A command against a retired playlist snapshot cannot change favorites.

No new dependencies or GUI coupling in core. No provider authentication, remote
loading, persistence, playback, ticket #13 search work, workflows or release changes.

## Regression and parity evidence

The first TDD test failed with missing `Category::Favorites` and
`BrowserWorker::toggle_favorite`, then passed after their implementation.
Controller coverage includes empty Favorites, exactly-once add/remove, playlist
ordering, item restoration, rotated URLs/reordering, absent/returning channels,
failed loads, retired rows, rapid toggles/category changes, and a parsed but
superseded replacement. Adapter tests cover names, disambiguated category labels,
real/empty accessible rows, and all action status outcomes.

Python oracle: `serrebidev/Accessible-IPTV-Client` revision
`9aa89e053225ef2c83c4f3659775a00723012ff7`. Reviewed `favorites.py`, `main.py`
group/category wiring, `channel_widgets.py`, `tests/test_favorites.py`, and native
accessibility smoke patterns. The parity tool executes committed source only;
it reads no configuration and opens no fixture URLs. Seven new golden scenarios
exercise the reference parser, `_favorite_channels` and `_source_for_group`:
empty, toggle order, refresh, absent channels, literal Favorites group, same names
with distinct IDs, and double toggle. Existing 27 identity, six normalization and
54 toggle/filter outcomes remain unchanged.

Ticket #12 intentionally differs from Python's empty-category removal/dialog and
focus-event feedback: the published spec requires a stable empty category and
status/live-region announcements without moving focus. These UI requirements
are not presented as byte-identical Python UI parity.

An existing playlist parity-tool failure was reproduced: its query literal was
mojibake while the committed golden query was `映画`. Replacing that literal with
Unicode escapes restored the check without changing any golden or Rust behavior.

## Final automated gate (2026-11-01)

Before every Rust command, session-only environment:
`LIBCLANG_PATH=C:\Program Files\LLVM\bin`;
`C:\Program Files\CMake\bin;C:\Program Files\LLVM\bin` prepended to PATH;
`CARGO_TARGET_DIR=C:\Users\farhan\Documents\Codex\Accessible-IPTV-Client-Rust\target`.
No global environment changes.

| Command | Exit | Result |
| --- | --- | --- |
| `cargo fmt --all -- --check` | 0 | Clean |
| `cargo test --workspace` | 0 | 40 tests passed; none failed or skipped |
| `cargo check --workspace` | 0 | Both crates checked |
| `cargo clippy --workspace --all-targets -- -D warnings` | 0 | No source warnings |
| `python tools/check-favorites-parity.py <Python-checkout>` | 0 | Existing goldens and seven category projections match |
| `python tools/check-playlist-parity.py <Python-checkout>` | 0 | Parser/search/category/recovery goldens match |
| `git diff --check` | 0 | Clean |

An existing running app locked the shared target's `iptv-app.exe`; it was left
running. Native smoke instead uses a separately named local executable:

```powershell
cargo rustc -p iptv-app --bin iptv-app -- -o C:\Users\farhan\Documents\Codex\Accessible-IPTV-Client-Rust\target\debug\ticket12-smoke.exe
powershell -NoProfile -ExecutionPolicy Bypass -File tools/smoke-foundation.ps1 -ExecutablePath C:\Users\farhan\Documents\Codex\Accessible-IPTV-Client-Rust\target\debug\ticket12-smoke.exe
```

Both exit 0. Rustc reports output-naming warnings for the explicit `-o` override;
the normal workspace Clippy gate above is warning-free. Smoke checks native MSAA
names/roles/focusability, the Favorites choice, zero-row status, forward/reverse
native Tab order through Toggle favorite, empty toggle command feedback, and
unchanged UI-thread keyboard focus. It dispatches native selection/button
commands; it does not send physical keys or claim audible NVDA acceptance.
`CBN_SELENDOK` commits a wx Choice selection. `BN_CLICKED` tests the button
handler without `BM_CLICK`'s simulated mouse-focus change. Smoke closes only its
own test process normally.

Reviewed commit scope: core category/search/browser and their category/parity
tests, desktop adapter/tests, favorite browse tests and three projection fixtures,
both parity tools (the existing playlist tool changes one query literal), native
smoke, fixture README, root README, and this evidence record. Post-commit status
must be clean. No issue modifications, push, merge or PR creation performed.

## Remaining owner-heard gate

Implementation and automated gates have no unresolved blocker. Brandon's manual
NVDA/keyboard acceptance remains open:

- Tab/Shift+Tab through Categories, Search, Channels, Toggle favorite and status;
  Space/Enter and Alt+F must toggle the intended focused row once.
- Hear added/removed outcomes during ordinary and rapid toggles, including last
  removal in Favorites; ensure background completion never moves keyboard focus.
- Inspect populated/empty Favorites, same-name channels, literal Favorites groups,
  selection/item-focus restoration and playlist refresh/recovery.
- Check large playlists, system large text, contrast/dark mode and reduced motion.

Populated native add/remove and physical keyboard/speech behavior are supported by
widget-free adapter/controller evidence here, but were not exercised by this
empty-state native smoke. Ticket #13 owns category/Favorites search interaction.
Python remains the shipping product; the replacement gate remains open.
