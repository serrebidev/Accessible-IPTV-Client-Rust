# 03: Incremental channel search

**What to build:** Add responsive, case-insensitive channel-name search to the local playlist browser so a user can narrow a large channel collection while retaining predictable selection and focus behavior.

**Blocked by:** 02: Local playlist browse seam

**Status:** ready-for-agent

- [ ] Search updates incrementally as the user types.
- [ ] Matching is case-insensitive and uses deterministic ordering.
- [ ] No-result and cleared-search states are understandable and accessible.
- [ ] Selection remains stable when the selected channel remains in the result set.
- [ ] Focus restoration is predictable when search results change or the user returns to the list.
- [ ] Search work does not block the desktop UI thread or allow stale results to overwrite newer input.
- [ ] Automated tests cover search results, selection, focus signals, and replacement by newer searches.
