# 01: Toolkit and workspace foundation

**What to build:** Establish a reproducible Rust workspace and a minimal runnable desktop shell that demonstrates an accessible empty list. Compare wxDragon with viable alternatives using the agreed written assessment and prototype, then record the toolkit decision before production UI work.

**Blocked by:** None (can start immediately)

**Status:** ready-for-agent

- [ ] A stable Rust toolchain and explicit MSRV are recorded and used consistently by local checks.
- [ ] Workspace boundaries keep domain logic independent from desktop widgets.
- [ ] The toolkit comparison covers Windows fit, keyboard operation, accessible names, focus, announcements, packaging, and maintainability.
- [ ] A minimal accessible-list prototype exercises keyboard focus and screen-reader inspection.
- [ ] The selected toolkit and rejected alternatives are recorded as an ADR.
- [ ] The workspace builds and its baseline checks pass without unresolved warnings.
