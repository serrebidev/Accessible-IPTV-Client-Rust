# VOD discovery scope

**Decision date:** 2026-11-04
**Status:** Design confirmed; formal specification recorded in `docs/specs/0006-vod-discovery.md`; implementation not started.

## Decision

Add VOD discovery with an Enter-applied text filter and an item information surface.

- Search movies and series across every group in the loaded catalogue. While a query is active, the selected group does not limit results; clearing the query restores the prior group.
- Search only top-level movies and series. Browse episode titles inside a series.
- Apply the query on Enter and keep keyboard focus in the search field.
- Include each result's group and source label. Use the configured friendly source name when available; otherwise use a host with URL user information removed, or the local filename. Use a generic label if neither is available.
- Announce a short item title and type. Make full available information, including EPG description and URL, reachable in a detail control. Do not invent richer provider metadata.
- Going Back from a series restores the global filtered results and the same series selection.

These are user-confirmed product decisions from the Chief-of-Staff planning conversation on 2026-11-04. The global search scope and Back behavior intentionally differ from the local Python behavior observed during planning; they require explicit specification as Rust product behavior.

## Evidence at decision time

- `docs/validation/0015-vod.md` records VOD browsing and playback implementation and the missing text filter and information surface.
- `docs/feature-parity-audit.md`, dated 2026-11-03, records the VOD filter limitation and identifies its Python comparison snapshot as v1.150.58 at commit `6afe6014fa773a5489d4c43ccafad5726386d931`.
- Relevant implementation inspected during planning: `crates/iptv-core/src/vod.rs`, VOD catalogue worker code, and the VOD browser adapter in `crates/iptv-app/src/main.rs`.
- The sibling Python checkout inspected during planning did not have a verified revision. Its observed selected-group filtering and detail behavior are therefore unverified against the audit-pinned Python snapshot. Check that pinned reference before treating Python behavior as authoritative for details beyond the confirmed Rust-specific decisions.

## Follow-up

The formal specification captures source identity propagation, keyboard and screen-reader behavior, and regression acceptance criteria. The audit-pinned Python commit was unavailable in the shallow sibling checkout, so historical Python UI behavior remains unverified. No ADR was needed for this reversible UI behavior decision.
