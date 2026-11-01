# 04: Playlist recovery and parity evidence

**What to build:** Make local playlist loading resilient and auditable: announce empty, missing, unreadable, and malformed inputs; preserve valid entries when possible; handle cancellation and stale work; and compare observable behavior against a pinned Python reference revision.

**Blocked by:** 03: Incremental channel search

**Status:** ready-for-agent

- [ ] Empty playlists produce an announced, focusable empty state.
- [ ] Missing and unreadable sources produce an announced recovery state with retry or choose-file action.
- [ ] Malformed entries do not crash the application or hide otherwise valid entries.
- [ ] Cancellation and replacement by a newer load/search prevent stale state from reaching the UI.
- [ ] Shared fixtures identify the pinned Python reference revision they represent.
- [ ] Automated parity checks cover parsed channel fields, ordering, search behavior, and recovery outcomes.
- [ ] Automated results clearly distinguish domain evidence from desktop accessibility evidence.
