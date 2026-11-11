# agents.md — Accessible IPTV Client, Rust build

Read this first. All of it. Then work.

## What this is

Rust port of Accessible-IPTV-Client.
Python repo is the spec: https://git.serrebiradio.com/serrebi/Accessible-IPTV-Client
Python behavior wins every argument. If Rust differs, Rust is wrong.
Python stays the shipping product until Brandon says swap. No swap without his word. No deleting Python features. Port them, all of them.

## The model: trypsynth

Build like Carter Temm (trypsynth) builds. Look at his repos before you design anything:
https://github.com/trypsynth?tab=repositories

Study these:

- paperback — accessible ebook reader, Rust. The template. Cargo workspace, `crates/` split: app crate thin, core crate holds logic, formats crate holds parsers, xtask crate holds build jobs.
- fedra — accessible Mastodon client, Rust. Same shape.
- wxDragon — his Rust bindings for wxWidgets. Our GUI. Only our GUI.

Copy his habits:

- Workspace, small crates, clear borders. Core logic knows nothing about windows or widgets.
- Dependencies pinned in `[workspace.dependencies]`, latest versions, `Cargo.lock` committed.
- clippy `all` deny, pedantic on. rustfmt on. No warnings shipped.
- `tracing` for logs. No println debugging left behind.
- Tests in the crates, next to the code they guard.

## More models

Two more accounts to study. Different lessons:

- masonasons — https://github.com/masonasons
  No Rust there. Do not go looking for Rust code, there is none. Steal the patterns instead:
  - FastSMRW: portable core, thin native front end. Same border we draw between core crate and app crate. He proves it survives a full rewrite.
  - SingingVoiceStudio: wxWidgets, screen-reader first. Our toolkit's family — study how he names controls and orders focus.
  - FastPlay: accessible audio player, tempo/pitch, effects. Our player must feel that direct from the keyboard.
  Lesson: architecture and player craft, not language.

- CVS Health accessibility techniques — https://github.com/cvs-health/ios-swiftui-accessibility-techniques
  SwiftUI, iOS, not our platform. Steal the discipline, not the code:
  - Every pattern written twice: good example, bad example, side by side, testable with a screen reader. Copy that habit in our tests and docs.
  - Their checklist mindset: labels, grouping, focus order, announcements, large text, contrast, reduced motion — audited, written down, checked again. We keep the same kind of checklist for NVDA/JAWS/VoiceOver on desktop.
  Lesson: accessibility is a catalogue of named patterns with pass/fail checks, not a vibe.

## GUI law

- wxDragon only. No Electron. No web UI. No second toolkit "just for this one window".
- Use trypsynth's ecosystem crates where they fit, before writing your own:
  - patois — i18n.
  - ship-shape — auto-updater for wxDragon apps.
  - shipfitter — packaging: installers, app bundles, docs.
  - quiet-list — lists whose rows change without the screen reader re-reading everything.
  - live-region — speak announcements through a live region.
  - homeport — find config/resource dirs, portable or installed.
  - key-chord — shortcut chord parsing and matching.
  - wx-utils — wxDragon helpers.
- Need something his stack lacks: say so, prove it, then build small.

## Accessibility law

This app exists for blind users. Brandon is blind. He tests with NVDA himself.

- Keyboard first. Everything reachable, everything operable, no mouse-only anything. Ever.
- Every control gets a real accessible name. No name, no merge.
- Focus never gets stolen. Background work never yanks focus.
- Lists must not re-announce rows that did not change. Use quiet-list patterns.
- Speak state changes through live region, not dialogs, not focus tricks.
- No ASCII art. No box drawing. No clever visual-only layout.
- Respect system settings: large text, high contrast/dark mode, reduced motion. Never hard-code around them.
- Related controls grouped and labelled as a group, not a pile of loose widgets. Focus order matches the logical order a listener expects.
- If a sighted user can do it and a screen reader user cannot, it is a bug. Top priority bug.

## Parity list — port all of it

From the Python spec:

- Playlists: M3U files and URLs, cached, huge ones included.
- Providers: Xtream Codes and Stalker Portal. Auth, session state, account info screen.
- EPG: XMLTV import (gzip too), SQLite store, search, guide-to-channel matching. Channel-name normalization is one shared implementation — UI and EPG must normalize identically or matching silently breaks. One copy. Never duplicate it.
- Browsing: groups/categories, favorites (keyed on provider-stable identity, never on resolved stream URL), incremental search.
- Catch-up and timeshift URL building.
- Playback: internal player, plus external player launch.
- Recording and DVR, with spoken report when a recording finishes.
- Casting: Caster engine is the port unit. Port/sync it, never reimplement it.
- VOD browsing and playback.
- Tray icon, restore from tray.
- Self-update.
- i18n: every language the Python app ships.

## Performance law

Playlists reach 500,000 channels. Design for that on day one.

- UI thread never blocks. Parsing, EPG import, search indexing: background, cancellable, gated.
- Incremental everything. Search filters as data arrives, not after a full rebuild.
- Python's responsiveness budgets (spec #105–#110 by thebetterfarhan) are the floor, not the target. Rust must beat them.
- A frozen window is a failed build.

## Playback law

Ported from the Python player. Do not reinvent:

- Live playback starts on a half-second cache, then fills at full speed.
- Never touch the player's playback rate. Rate under 1.0 to grow cushion made speech drag. Forbidden.
- Saved buffer durations are explicit user choices. Preserve them on load. Defaults apply to missing values only.
- Dispatcharr channels keep the saved buffer alone — it restreams from its own server-side buffer. Python detects them from the playlist header; do the same.
- Stall recovery: backed-off, gradual cache increase. Segment rollovers are not stalls, reopen them with the small opening cache.

## Working rules

- Reproduce first. Every bug Brandon reports gets a repro and a regression test, then the fix. Test things regularly, not at the end.
- Full test suite green before any release. No exceptions, no "pre-existing failure" hand-waving.
- Ponytail mode: stdlib first, crates already in the tree second, new dependency last. No unrequested abstractions. No speculative features.
- Blunt commits. One fix per commit. Say what broke and what changed.
- Never edit `.github/workflows` without Brandon's direct OK.
- Releases: check latest published release AND latest tag first. Version only goes up. Never re-release or re-tag an old version. CI green on main, then tag.
- This repo is desktop only. Android signing rules and the mobile apps live elsewhere.

## People

- Brandon Bracey (serrebidev) — owner. Blind, NVDA user. His hands-on test is the acceptance test.
- Farhan Khan (thebetterfarhan) — trusted co-developer. Wrote the Python responsiveness spec. His budgets and designs carry over.

When unsure: read the Python code. It already answered.

## Agent skills

### Issue tracker

Issues and specs for this repository live in Forgejo issues (git.serrebiradio.com). Use the `tea` CLI. See `docs/agents/issue-tracker.md`.

### Triage labels

Use the canonical labels `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, and `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

This is a single-context repository. Read `GLOSSARY.md` and relevant records under `docs/adr/` when they exist. See `docs/agents/domain.md`.
