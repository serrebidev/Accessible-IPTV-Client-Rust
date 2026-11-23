# Python parity, round 3: design

Date: 2026-11-23. Owner: Brandon (serrebidev). Status: approved by Brandon 2026-11-23.

## Intent

Brandon is close to replacing the Python client with this Rust client. Before the swap, every user-facing feature the Python client has must exist here. Python went from v1.150.58 (last audited, see `docs/feature-parity-audit.md`) to v1.151.6 (`a29ad9a3`). Rust ported only Python PR #36 (validation 0101), the 200 percent volume boost and the About links from that range. About 33 features are missing.

Success: Brandon can swap and lose nothing. His data (settings, schedules, reminders, bookmarks, favourites) carries across, and every feature in his Python user guide works here under NVDA.

## Goals

- Port the 21 feature groups below, behavior per Python (`agents.md`: Python is the spec, Rust that differs is wrong).
- Data written by Python is read by Rust and the reverse, for every store these features add.
- Each feature lands with core tests next to the code and an NVDA-checkable acceptance line.
- Record round 3 in `docs/feature-parity-audit.md` as the ledger, one entry per feature, closed by validation notes like earlier rounds.

## Non-goals

- New features Python does not have.
- Rewording Python's strings. Strings are copied verbatim, then translated through the existing patois catalogues.
- AirPlay and MPV subtitle sessions (earlier recorded non-ports).
- Releasing. A release happens after the round, by the normal GitHub-runner path, only when Brandon asks.

## Anti-goals (failure even if it works)

- A store format that Python cannot read back, or Rust ignoring data Python wrote. That loses user data on the swap.
- Any UI-thread block, focus theft, or silent control. `agents.md` accessibility and performance laws apply to every dialog.
- A feature that passes core tests but is not wired into the app (round 2 lesson, N02). Every feature needs an in-app path checked.

## Constraints

- `agents.md` laws: wxDragon only, keyboard first, real accessible names, live region for state changes, no blocking the UI thread, Ponytail (stdlib, then existing crates, new dependency last), clippy clean, rustfmt.
- Shortcuts go through `iptv-core/src/shortcuts.rs` with Python's ids and defaults (`shortcuts.py`).
- Settings keys, file names and JSON shapes come from the Python module named per feature. Read the Python module before writing the Rust one.
- Windows-only behavior (sleep prevention, wake timers) is a no-op elsewhere, as Python's is.
- New strings need entries in every catalogue the tree ships (patois), English copied from Python's `.pot`, translations from Python's `locale/` where the msgid matches.
- The CI pin of the Python reference (`.github/workflows/pull-request.yml`, ref `9aa89e05...`) moves to `a29ad9a3` (v1.151.6) as the round's first commit.

## Approaches considered

1. Parallel split by difficulty (chosen by Brandon 2026-11-23). Claude ports the hard groups. muse.ai ports the easy groups in parallel, one Forgejo pull request per group, each based on latest main. Claude pulls each PR, runs the full suite, clippy and a Windows build, reviews, resolves conflicts, merges. Cost: merge conflicts in `crates/iptv-app/src/main.rs` (21k lines), resolved by Claude.
2. Serial: muse.ai first, then Claude. No conflicts, about twice the elapsed time. Rejected for time.
3. muse.ai writes core modules only, Claude wires UI. Rejected: the easy groups are mostly UI over existing core, so the split would not save work.

GitHub pull-request workflows are disabled by the Forgejo-to-GitHub sync, so there is no CI on PRs. Claude's local run is the gate.

## The split

Python source files named per group are the spec. Rust landing spots are the expected home; the builder may choose another if the code says so.

### Claude (hard)

C1. Recording planning engine. Review Guide Changes and the "Notify me of recording time changes after guide updates" option; Find Another Airing (14 days, episode-number match vs title-only possible repeat, connection-limit count); Skip This Occurrence; Repeat on Weekdays plus weekday choice in Schedule by Time (local times kept across DST, nonexistent windows skipped); Padding for This Recording with "Apply to future occurrences too"; episode-aware rerun suppression for series rules; Partial and Stopped-by-user outcomes with rescheduling from a later airing; unreadable-schedule backup that blocks writes if the backup fails. Python: `dvr.py`, `recording_edits.py`, `recording_edits_ui.py`, `recording_conflicts_ui.py`, `planning_tools.py`, `planning_tools_ui.py`, `recording_outcome.py`, `main.py`. Rust: `iptv-core/src/dvr.rs`, `recording_limits.rs`, app scheduled-recordings window.

C2. Recover from Channel Archive for missed, failed or partial recordings. Python: `main.py`. Rust: reuses `catchup_direct.rs`, downloads, `recording_limits.rs`.

C3. Prevent Automatic Sleep While Recording (default on) and Wake for Scheduled Recordings (default off, 60 s before padded start, removed on exit). Python: `recording_power.py`, `accessible_tools_ui.py`. Rust: new core module with Windows calls (`SetThreadExecutionState`, waitable timer) behind `cfg(windows)`.

C4. Recording Space Monitor: 5 s check, warning default 1024 MiB, optional stop default 256 MiB, warning >= stop, one warning per drive, graceful stop of affected recordings. Python: `storage_monitor.py`, `accessible_tools_ui.py`, `main.py`. Rust: new core module, housekeeping tick, recording stop path.

C5. Save Recent Broadcast from the local live buffer to a new MPEG-TS file, complete segments only, cancellable. Python: `buffer_clip.py`, `client_timeshift.py`, `internal_player.py`. Rust: `timeshift.rs`.

C6. Read Embedded Subtitle Track for local MKV recordings (language and track choice, image tracks identified, cue review) and Recording Chapters (list, Enter jumps). Python: `recording_media.py`, `media_navigation_ui.py`, `internal_player.py`. Rust: `subtitles.rs`, `iptv-vlc`, ffmpeg/ffprobe as Python uses them.

C7. Recording Playback Queue (add from library, reorder Ctrl+Up/Down, Delete, Play Queue, Stop after Current, pause on missing/changed/still-recording) and starting playback at a Recording Problem Times entry from the library. Python: `recording_queue.py`, `recording_library_ui.py`. Rust: `recording_library.rs`, `recording_log.rs`, player.

C8. Automatically Play the Next Episode: off by default, 10 s countdown after a normal finish, Play Now and Cancel/Escape, invalidated by newer playback or playlist change. Python: `episode_continuation_ui.py`, `accessible_tools_ui.py`. Rust: `episode_continuation.rs` (exists), player end-of-media path.

C9. Full backup: format `accessible-iptv-settings-v2` with schedules, reminders and search history, import preview, pending-file redo journal finished at startup before schedulers run, legacy backups replace settings only. Must round-trip with Python. Python: `full_backup.py`, `settings_backup.py`. Rust: `settings_backup.rs`.

C10. Assign Programme Guide: per-playlist/provider override of the guide channel, used by labels, guide views, search and scheduled recordings; Use Automatic Matching removes it; a vanished id stays unavailable. Python: `accessible_tools_ui.py`, `main.py`, `playlist.py`. Rust: `guide_match.rs`, `guide.rs`, `epgstore.rs`.

C11. Saved Programme Searches: up to 30 rules, scope all/selected channel/named-list snapshot, run after imports and every 15 minutes over 14 days, 500 results, new-match announcements, results create reminders or recordings. Python: `saved_searches.py`, `personal_tools_ui.py`. Rust: new core module, `epgstore.rs`, `background.rs`. Depends on M9 for the named-list snapshot scope; Claude does C11 after M9 merges.

C12. Guide ids scoped per guide source: `epg:<source_key>:<id>` splitting on collision or rename, `channels.xmltv_id` and `channel_sources`, and `choose_source_collision` in matching. Python: `playlist.py`, `channel_identity.py`. Rust: `epg.rs`, `epgstore.rs`, `guide_match.rs`. Done together with C10. Added by the scope sweep.

### muse.ai (easy)

M1. Mute / Unmute (Ctrl+Shift+M main window, M in player, survives channel change and reconnect) and Player > Remember Volume for Each Channel (off by default, per playlist/provider). Python: `channel_preferences.py`, `internal_player.py`, `accessible_tools_ui.py`, `shortcuts.py`.

M2. On-demand seeking: Ctrl+Left/Right 10 s, Ctrl+Shift+Left/Right 60 s, Ctrl+T Go to Time, T announces elapsed, total and remaining. Seekable media with known duration only; live unchanged. Python: `media_navigation.py`, `media_navigation_ui.py`, `shortcuts.py`.

M3. Named Bookmarks: Add Named Bookmark, list with Go to, Rename, Delete; unique names per item; 1000 total; separate from resume positions. Python: `named_bookmarks.py`, `session_tools_ui.py`, `internal_player.py`.

M4. Reminder Remind Again in 5 Minutes and Remind at Programme Start (saved, survives restart, before programme end) and Export to Calendar (.ics, UTC times) from programme menus and reminders. Python: `programme_reminders.py`, `planning_tools.py`, `planning_tools_ui.py`, `dialogs_misc.py`. Rust: `reminders.rs`.

M5. View > Notification History: latest 200 queued notifications this session, Details, Refresh, Copy Details, credential masking, Clear History with confirmation. Python: `session_tools_ui.py`, `accessible_tools_ui.py`. Rust: the N18 notification queue in the app.

M6. Per-event announcement switches (Playback, Subtitles, Guide) under Automatic Announcements. Python: `main.py` (PR #118), `internal_player.py`.

M7. Recordings > Recording Connection Limits settings dialog (per account, 1 to 64, 0 for no limit). Python: `recording_limits_ui.py`. Rust: `recording_limits.rs` (logic exists).

M8. Recording Library > Export List to CSV (filtered and sorted rows, no paths or URLs, formula-prefix protection). Python: `recording_library_ui.py`. Rust: `recording_library.rs`.

M9. View > Favourite Lists (50 lists of 2000, ordering, missing channels kept), Export and Import Favourite List (no URLs or credentials, match review, ambiguous choice), Relink Missing Channel. Python: `favorite_collections.py`, `favourite_transfer.py`, `favourite_transfer_ui.py`, `personal_tools_ui.py`. Rust: new core module beside `favorites.rs`.

M10. Programme Guide by Time (local date and time, scope all/favourites/named list, rows with channel, title, start, end; schedule or remind) and Programme Details. Python: `personal_tools_ui.py`, `dialogs_misc.py`. Rust: `epgstore.rs`, `guide.rs`. The named-list scope arrives with M9; if M9 has not merged, M10 ships all/favourites and adds named lists after.

M11. Recently Watched keyed per playlist source (`favorites.history_key`, `history_channels`, `history_labels`) and `playlist_index.channel_number` for Go to Channel. Python: `favorites.py`, `playlist_index.py`. Rust: `favorites.rs`. Added by the scope sweep.

The scope sweep also folds series padding into C1, subtitle-speech reset on seek into M2, reminder alert choice into M4, and start-at-problem-time seek into C7.

## How muse.ai works

- Claude briefs muse.ai through `muse-cli` in a side chat titled "IPTV Rust parity round 3" and watches Forgejo for its pull requests. Progress is tracked in the round-3 section of `docs/feature-parity-audit.md`, so work can resume in a later session from the repo alone. (Claude's call.)
- Takeover rule (Claude's call): if no pull request for the next M item appears within two days of the previous one, or a pull request still fails review after one round of requested changes, Claude ports that item itself. Before starting C11, Claude checks M9; if M9 has not merged, Claude ports M9 first.
- Repo: Forgejo `serrebi/Accessible-IPTV-Client-Rust`, Python reference `serrebi/Accessible-IPTV-Client` at `a29ad9a3`.
- One branch and one Forgejo pull request per group, named `muse/m<N>-<slug>`, based on the newest `main`. Rebase before opening.
- Before opening: `cargo test -p iptv-core`, `cargo clippy -p iptv-core --all-targets`, `cargo fmt --check`. The VM has 2 CPUs; the full app build is optional there.
- PR body: what landed, Python files it follows, how to check it in the app with NVDA, residuals.
- Do not edit `.github/workflows`, do not bump versions, do not release, do not merge.
- Order: M1 to M11 as listed, M9 before M10 if possible.

## How Claude works

- Branches `parity-round3/c<N>-<slug>` from newest `main`, merged by Claude after the full suite (`cargo test --workspace`), clippy and a Windows build pass.
- Each muse.ai PR: pull, review against the Python source, run the same gate, fix or request changes, merge to Forgejo `main`.
- Scope sweep (Claude's call): the 21 groups came from Python's user-guide diff. Before C1, Claude reads the code diffs of Python PRs #118, #119, #33, #34, #36, #39 and #42 (the range `6afe6014..a29ad9a3`) for behavior the guide does not describe: reliability fixes, data-recovery rules, new settings. Anything found joins the ledger as a new C or M entry, or is recorded as not ported with the reason.
- Order: housekeeping (CI pin, audit round 3), scope sweep, then C1, C9, C10, C3, C4, C2, C7, C8, C5, C6, C11. C12 goes with C10. C1 first because most other recording work builds on its schedule shape; C9 next because it fixes the data format the swap depends on.
- Each closed group gets a validation note under `docs/validation/` and its ledger entry marked fixed, as in rounds 1 and 2.

## Acceptance

- Every round-3 ledger entry is fixed with a validation note.
- `cargo test --workspace` and clippy green on `main`.
- Brandon's NVDA pass is the final acceptance (ADR 0004). It happens after the round, on a build he chooses.

## Left to the builder

- Internal module boundaries and names, beyond the landing spots above.
- How the app wires each dialog into existing windows, provided Python's menu placement, labels and shortcuts are kept.
- Test fixtures, including using the Python reference as an oracle where earlier rounds did.
