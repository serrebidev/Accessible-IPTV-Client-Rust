# Rust Port Foundation: Accessible Local Playlist Browsing

## Problem Statement

Accessible-IPTV-Client is a mature wxPython desktop IPTV client. Its planned Rust replacement needs a controlled first capability that proves the migration can preserve user-visible behavior, responsiveness, keyboard navigation, and screen-reader accessibility without coupling domain logic to a desktop toolkit. The current Rust repository has no implementation yet, so the first slice must establish the architecture and evidence pattern for the larger port.

## Solution

Build the first Rust vertical slice as an accessible local M3U/M3U Plus playlist browser. A single playlist-browse application seam accepts a local source, parses the common channel metadata, maintains browse and incremental-search state, and emits UI-neutral results, status, and recovery events. A desktop adapter presents that behavior through the toolkit selected by a short evidence-based spike, with wxDragon as the default. The Python reference remains the shipping product and pinned behavior oracle while Rust leads development.

## User Stories

1. As a blind IPTV user, I want to open a local M3U playlist, so that I can browse my channels in the Rust client.
2. As a blind IPTV user, I want the channel list to have a clear accessible name, so that I know which control contains the channels.
3. As a keyboard user, I want every playlist-browse action to be reachable and operable from the keyboard, so that I do not depend on a mouse.
4. As a blind IPTV user, I want each channel row to expose its display name, so that I can identify the channel while moving through the list.
5. As a blind IPTV user, I want channel selection and focus to remain stable when the list changes, so that I do not lose my place while browsing.
6. As a user with a large playlist, I want the list model and search path to avoid blocking the UI thread, so that the application remains responsive.
7. As a user, I want common M3U Plus metadata such as tvg-id, tvg-name, tvg-logo, and group-title preserved, so that later guide, grouping, and presentation features have the information they need.
8. As a user, I want unknown M3U attributes preserved rather than discarded, so that future parity work does not require reparsing old inputs.
9. As a user, I want channel-name search to update incrementally and case-insensitively, so that I can find a channel without manually scanning a large list.
10. As a keyboard user, I want search results to preserve a predictable selection and restore focus appropriately, so that typing does not strand me in an unusable state.
11. As a user, I want an empty playlist to be announced as an understandable empty state, so that I know the load succeeded but produced no channels.
12. As a user, I want an unreadable or missing local file to produce an announced, focusable recovery state, so that I know what failed and can choose another file or retry.
13. As a user, I want malformed entries handled without crashing the application or hiding valid entries, so that one bad line does not destroy the rest of the playlist.
14. As a maintainer, I want parsing, search, and recovery behavior testable without desktop widgets, so that regressions can be caught in fast automated tests.
15. As a maintainer, I want the desktop adapter tested for accessible names, focus order, keyboard operation, and status announcements, so that domain tests cannot mask UI accessibility regressions.
16. As a maintainer, I want Rust behavior compared with a pinned Python reference revision and shared fixtures, so that parity claims are reproducible.
17. As a product owner, I want automated evidence and owner-heard NVDA/keyboard validation recorded separately, so that passing code tests is not mistaken for real accessibility acceptance.
18. As a product owner, I want the Rust slice to preserve existing user-data and configuration compatibility decisions where they apply, so that the eventual replacement does not strand users.
19. As a release maintainer, I want a reproducible stable Rust toolchain and explicit MSRV, so that local and CI results agree.
20. As a product owner, I want the Python product to remain shippable until the replacement gate is met, so that the rewrite does not create an unsafe release gap.

## Implementation Decisions

- Rust leads new development; wxPython remains the shipping product and behavioral reference until the replacement gate is explicitly satisfied.
- The first implementation is a workspace with a wx-free core/domain layer, format and provider boundaries, and a desktop application layer. The first slice need not implement providers or playback, but its boundaries must not prevent them later.
- The primary seam is a playlist-browse application boundary between the desktop adapter and wx-free parsing, search, selection, and recovery behavior.
- The local source path is the first input. Remote URL loading is a later slice.
- The parser supports the common M3U and M3U Plus channel fields used by the Python reference: display identity, tvg-id, tvg-name, tvg-logo, group-title, and stream URL. Unknown attributes remain available for future parity work.
- Search is incremental and case-insensitive over channel names. Result changes preserve deterministic ordering and stable selection/focus behavior.
- Loading and search work must not block the desktop UI thread. Cancellation and stale-result handling must prevent an older operation from overwriting newer user state.
- Empty, unreadable, missing, and malformed inputs produce UI-neutral status/recovery events. Valid entries remain usable when individual entries are malformed.
- The desktop adapter gives the list, search control, and status/recovery state explicit accessible names and a logical keyboard focus order. It must not rely on visual-only cues.
- The toolkit choice is preceded by a short written comparison and one minimal accessible-list prototype. wxDragon/wxWidgets is the default; an alternative must demonstrate better fit for Windows packaging, keyboard behavior, screen-reader names, focus, announcements, and maintainability before displacing it.
- A pinned stable Rust toolchain and explicit MSRV are recorded before production implementation, and CI/local checks use the same baseline.
- Parity evidence references a pinned Python revision and shared fixtures. The Python branch is not treated as an unbounded moving target.
- Existing user-data and configuration formats are preserved where practical, with migration tests introduced before any eventual replacement decision.
- The first slice is complete only when automated domain/UI evidence passes and the owner-heard NVDA/keyboard gate is separately performed and recorded.

## Testing Decisions

- Tests assert external behavior at the playlist-browse application seam: parsed channels, metadata preservation, ordering, search results, selection state, status events, and recovery behavior. They do not assert internal helper layout or crate structure.
- Parser tests cover valid common M3U Plus entries, missing optional metadata, unknown attributes, UTF-8 text, blank lines, malformed entries, empty input, and unreadable source errors.
- Search tests cover case-insensitive incremental updates, deterministic ordering, no-result states, selection preservation, focus restoration signals, and stale-result suppression.
- Application-seam tests cover successful load, partial malformed input, empty playlists, missing/unreadable files, retry/choose-file recovery, and cancellation or replacement by a newer request.
- Desktop adapter tests cover accessible names, keyboard reachability, focus order, selection announcements, search interaction, and announced recovery states using the selected toolkit’s accessible inspection facilities.
- Shared fixtures are derived from a pinned Python reference revision and compare observable channel and search behavior without requiring Python implementation details in Rust tests.
- Existing Python behavior and accessibility tests are prior art, especially the M3U parsing seam, search ranking/state behavior, virtual channel list behavior, explicit accessible naming, and owner-facing NVDA smoke checks.
- Automated results and owner-heard NVDA/keyboard results are reported as separate evidence categories. One cannot be used to claim the other.

## Out of Scope

- Remote playlist URLs, provider authentication, Xtream Codes, Stalker Portal, and network retry policy.
- XMLTV/EPG import, matching, catch-up, timeshift, groups beyond preserving group metadata, and favorites.
- Internal or external playback, recording, casting, VOD, tray behavior, and self-update.
- Full Python feature parity or a Python-to-Rust release switch.
- Final toolkit selection before the written comparison and accessible-list prototype.
- Creating a new user-data format without an explicit compatibility decision.
- Mobile, Android, or TV targets.

## Further Notes

This specification is the first buildable slice of a larger conversion. After publication, split it into dependency-aware tickets: toolchain/workspace foundation, toolkit spike, parser and channel model, browse/search application seam, desktop accessibility adapter, parity fixtures, and the separate owner-heard validation gate. The resulting tickets should preserve the distinction between automated evidence and physical screen-reader acceptance.
