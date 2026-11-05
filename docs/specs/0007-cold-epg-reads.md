# Cold EPG Reads

## Status

Design approved 2026-11-04. Implementation not started.

## Problem Statement

When the application uses its persisted EPG database before a guide has loaded into memory, opening or querying the database can block the interface. Cold index construction, current/next lookups, programme queries, bulk snapshots, and EPG search routing can delay keyboard input and screen-reader interaction, including when the user returns to the window.

## Solution

Run all cold SQLite-backed guide reads and index construction in background work. Keep the interface responsive and apply results only while their request and guide inputs are still current. Background completion must not move focus.

## User Stories

1. As a user, I want the application to open the saved guide database without freezing the window, so that I can keep using the keyboard while guide data becomes available.
2. As a user, I want current and next programme information to load in the background, so that channel browsing and playback controls remain responsive.
3. As a user, I want a channel's guide view to load stored programmes in the background, so that opening a large guide does not block input.
4. As a user, I want “What's on Now” to obtain its whole-guide snapshot in the background, so that the window remains responsive while the list is prepared.
5. As a user, I want EPG search results and their channel matches to be prepared in the background, so that searching a large playlist does not stall the interface.
6. As a keyboard and screen-reader user, I want background guide results to update the existing view without moving focus, so that I can continue where I was.
7. As a user, I want results from an old channel, query, playlist, or guide generation to be discarded, so that delayed work cannot replace newer information.
8. As a user, I want existing guide information to remain available while a refresh is in progress, so that background work does not cause unnecessary blanking or flicker.
9. As a maintainer, I want worker-backed guide reads to preserve current matching, time-window, ordering, and error behavior, so that responsiveness work does not change guide results.

## Implementation Decisions

- Keep the loaded in-memory guide snapshot as the fast path. Move the SQLite fallback path to worker-owned work; the UI adapter must not open or query the EPG database for interactive guide results.
- Cover cold store operations used by application flows: opening the read connection, checking whether usable data exists, loading guide channels and building `GuideIndexes`, per-channel now/next and programme reads, bulk now/next snapshots, search candidates, and routing guide results to playlist channels.
- Reuse the existing EPG and suffix worker patterns where they fit. Expose a pollable request/result seam in wx-free core logic, and keep widget updates and announcements in the wxDragon adapter.
- Tag work with the relevant request identity and guide generation; include the playlist/model identity for catalogue-routed results. Drop results when the selected channel, query, catalogue, or guide generation has changed.
- Keep focus where it is when work completes. Update the relevant existing row, detail surface, or dialog in place. For user-requested EPG views, expose concise loading and completion states through the existing accessible status path.
- Preserve the last valid displayed guide values until a current replacement is ready. Failed or unavailable store reads retain the existing no-data/error behavior and must not erase a usable in-memory snapshot.
- Keep EPG import, merge, and loaded-guide index construction on the existing background load path. This change addresses reads from the persisted store before a loaded in-memory snapshot is available.
- Correct validation record 0070: its claim that guide merging and `GuideIndexes::build` remain on the UI thread at guide delivery does not match the implementation. Record that `EpgWorker` already performs those steps on its worker; identify the cold persisted-store fallback as the remaining UI-thread path.

## Testing Decisions

- Use a temporary SQLite EPG database as the highest core seam. Exercise the worker request/result path for index construction, per-channel reads, bulk snapshots, programme queries, and search-routing data; verify returned values match existing guide semantics.
- Test missing or unreadable stores, empty guides, and a store update while a request is in flight. A failed or stale request must not erase the last valid result.
- Test app-side result acceptance against request generations, guide generation, selected-channel identity, query, and playlist/model identity. A stale result must not replace the current view.
- Test that completion updates result state without invoking focus changes. Keep owner-heard NVDA and physical-keyboard acceptance separate; verify focus stability and usable loading/completion announcements manually.
- Follow the existing EPG worker temporary-store tests and suffix-result generation tests as prior art.
- Implementation gates: focused EPG tests, `cargo fmt --all -- --check`, `cargo test --workspace`, `cargo check --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, and `git diff --check`.

## Out of Scope

- Changing EPG match rules, programme time windows, row labels, or guide import/merge semantics.
- Moving the already-loaded in-memory snapshot path to a worker.
- Changing EPG schema or persistence format.
- Moving playlist parsing or provider/network work as part of this change.
- Reworking playback or the suffix worker's existing generation behavior beyond sharing the cold-read worker boundary where useful.

## Further Notes

- The design was confirmed on 2026-11-04. The repository's validation record 0068 documents a cold `all_now_next` read of 3.20 seconds and a warm read of 65.6 milliseconds at its measured scale; validation 0070 moved the channel suffix pipeline to a worker.
- As of `serrebidev/Accessible-IPTV-Client-Rust@654f43e`, `EpgWorker` already merges the guide and builds `GuideIndexes` before delivering a loaded snapshot. The remaining cold-store callers are the scope of this spec.
- Implementation is tracked in [issue #181](https://github.com/serrebidev/Accessible-IPTV-Client-Rust/issues/181).
