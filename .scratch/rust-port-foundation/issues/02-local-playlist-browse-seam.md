# 02: Local playlist browse seam

**What to build:** Let a user choose a local M3U or M3U Plus playlist and browse its channels in an accessible keyboard-operated list. The browse behavior must expose common channel metadata and keep the parsing and application behavior independent from desktop widgets.

**Blocked by:** 01: Toolkit and workspace foundation

**Status:** ready-for-agent

**Python reference:** Mirror the observable channel records produced by `M3uCacheMixin` and the existing `main._parse_m3u_return` compatibility surface. Use the Python parser tests and channel-list behavior as the fixture source, not as Rust implementation structure.

- [ ] A local playlist can be opened through the desktop flow.
- [ ] Common channel identity, display name, logo, group, and stream metadata are represented.
- [ ] Unknown M3U attributes are preserved for later parity work.
- [ ] Valid channels appear in deterministic source order in an accessible list.
- [ ] The list, selection, and file-open path are keyboard reachable and have explicit accessible names.
- [ ] Parsing and browse behavior have automated tests at the playlist-browse application seam.
