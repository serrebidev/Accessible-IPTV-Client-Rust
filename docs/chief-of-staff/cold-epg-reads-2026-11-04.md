# Cold EPG reads

**Decision date:** 2026-11-04
**Status:** Design confirmed; formal specification recorded in `docs/specs/0007-cold-epg-reads.md`; implementation not started.

## Decision

On 2026-11-04, the user chose to move all cold SQLite-backed EPG reads and index construction off the UI thread. Background results must leave focus in place and be discarded when their request, selected channel, query, playlist model, or guide generation has become stale. The agreed test seam is a core worker request/result path exercised with a temporary EPG database, plus app-side stale-result and focus behavior checks.

## Evidence at decision time

- The checkout was `serrebidev/Accessible-IPTV-Client-Rust@654f43e` on `feature/new-feature`.
- The issue report and measurements are recorded in `docs/validation/0070-playback-cpu-focus-lag.md` and `docs/validation/0068-startup-load-audit.md`.
- Code inspection at `654f43e` showed store index construction, per-channel reads, bulk snapshots, EPG search queries, and startup store opening in the app adapter. These are the cold store-backed paths covered by the decision.
- `EpgWorker` at `654f43e` already merges successful guide loads and builds `GuideIndexes` on its worker before delivering an `EpgSnapshot`. The contrary statement in validation 0070 is stale and is included for correction in the implementation scope.

## Follow-up

- [Cold EPG Reads spec](../specs/0007-cold-epg-reads.md) records user-visible behavior, boundaries, and test seams.
- Before implementation, split the multi-path work into tickets if it remains a multi-session change.
- Correct validation 0070 when implementation updates its cold-store measurements and remaining-work description.
