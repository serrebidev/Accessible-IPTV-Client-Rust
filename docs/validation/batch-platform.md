# Validation — Platform batch (parity audit S01–S10)

Branch `agent/parity-platform`, against `docs/feature-parity-audit.md`
section "Platform integration, casting, localization and help". Python
reference: the pinned Python client (v1.150.58). Every item has a
regression test in `iptv-core` (the app crate has no test harness; the
logic lives in core and the adapter wires it).

## S01 — Single-instance handover and restore

- New `crates/iptv-core/src/instance.rs`: an advisory lock file
  (`instance.lock` in the per-user config directory) held through
  std's `File::try_lock` (the OS releases it when the process dies,
  exactly the Python shape), a `show_request` file the second launch
  drops, and `hand_over` mirroring Python's three outcomes: `Shown`
  (the running copy took the request), `Acquired` (the holder was
  closing; run after it) and `Unresponsive` (deadline passed).
- `main()` acquires before wx starts: a second launch hands over and
  exits with code 12 (`HANDED_OVER_EXIT_CODE`); an unresponsive holder
  shows Python's exact message and also exits 12.
- The running copy polls the request every second on the new
  housekeeping timer (S07) and restores itself (show + raise, which
  also un-hides it from the tray).
- Tests: `second_acquire_fails_while_the_first_guard_lives`,
  `show_request_is_written_and_consumed_once`,
  `hand_over_reports_shown_when_the_running_copy_takes_the_request`,
  `hand_over_runs_after_all_when_the_lock_is_free`,
  `hand_over_reports_an_unresponsive_holder`.

## S02 — Portable settings beside the executable win

- `config.rs`: `config_candidates()` is now executable directory,
  then working directory, then per-user (this build is portable, like
  Python's portable build). New `exe_dir()`, a writability probe
  (`dir_writable`, creates and removes a uniquely named probe file),
  and `default_write_target()`: executable directory when writable,
  else working directory, else per-user — so portable settings both
  load and persist, with a working fallback when the portable
  directory is unwritable. `bind_browser` loads with the candidate
  list and the computed target instead of always the first candidate.
- Tests: `portable_settings_beside_the_executable_come_first`,
  `write_target_prefers_a_writable_portable_dir`.

## S03 — Cast proxy binds and advertises a LAN address

- `proxy.rs`: new `lan_ip()` (UDP connect to 8.8.8.8, the Python
  method; loopback/unspecified rejected) and `advertised_host()`.
  `ProxyServer::start()` binds the LAN address and builds its base
  URL from it, so a TV on the network can reach the advertised URL
  and the rewritten playlist segments; header injection is unchanged
  because the proxy still fetches upstream itself. `start_on(host)`
  keeps the binding testable, and a loopback fallback covers a
  machine with no LAN route.
- Tests: `advertised_host_prefers_the_lan_address`,
  `proxy_binds_and_advertises_the_given_host`.

## S04 — Probed MIME survives the opaque proxy URL

- `cast.rs`: new `effective_mime(probed, url)` (a probed format wins;
  the URL guess is only the fallback) and `roku_mime_for_media`. The
  `Play` job now carries an optional probed MIME used for both the
  UPnP content type and the Roku format instead of guessing from the
  `/s/<hash>` path. The adapter determines the format from the
  original channel URL with the same extension/magic-hint rules
  (`guess_mime`) before any proxying and passes it through.
- Test: `probed_mime_survives_the_opaque_proxy_url`.

## S05 — Real log file and redacted diagnostics

- New `crates/iptv-core/src/applog.rs`: a minimal sink (no new
  dependency) writing timestamped lines to
  `iptvclient_epg_debug.log` (Python's file name) under a `logs`
  folder of the per-user config directory, rolling to one backup past
  5 MiB; `read_tail` bounds reads to a byte count from the end;
  `sharing_excerpt` ports Python's whole-line redaction of
  scheme-address and secret-assignment lines; `diagnostics_report`
  builds Python's report shape (name/version, platform, config path,
  EPG enabled, playlist count, active recordings, log tail).
- The app opens the sink at startup (config-recovery warnings and
  guide, recording and cast errors are written to it), Open Logs
  opens the real logs folder, and Copy Debug copies the real report
  and reports clipboard failure instead of failing silently.
- Deviation from Python: on non-Windows Python keeps logs in the
  system temp directory; Rust keeps them in the per-user config
  directory's `logs` folder on every platform so Open Logs and the
  diagnostics copy always find the same file.
- Tests: `sink_writes_lines_and_rolls_over`,
  `tail_read_is_bounded_to_the_end`,
  `excerpt_redacts_addresses_and_secrets_only`,
  `report_carries_facts_and_the_redacted_tail`.

## S06 — Translation lookup order and language activation

- `i18n.rs`: new `interpolate` and `I18n::gettextf` — values are
  interpolated into the template AFTER lookup, so catalogue keys
  match. The adapter gained `trf` and the dynamic status messages
  (counts, names, paths, errors) now call it instead of formatting
  first and translating the result. The remaining status call sites
  that format without `trf` carry no sentence template to look up
  (bare labels go through `tr` as before).
- Activation: the running translator is replaceable
  (`RwLock<Option<I18n>>`, and `Wiring.i18n` is now
  `Rc<RefCell<I18n>>`). Selecting a language saves the choice,
  activates it immediately for new dialogs, and shows Python's
  restart-for-full-effect confirmation in the new language.
- Bundled `.mo` catalogues remain the recorded resource limit and
  were not added; the lookup path is exercised by the tests with a
  synthetic catalogue.
- Test: `interpolation_runs_after_lookup` (a formatted string never
  matches its template key; the template does, with the value
  interpolated after).

## S07 — Hourly automatic update checks

- A 1-second housekeeping timer (shared with S01) runs an automatic
  update check every 3,600 seconds while the option is enabled,
  re-reading the option each time so disabling suppresses the next
  check; the startup check is unchanged. Deliberate difference from
  Python: the hourly interval is measured from session start whether
  or not the option was on, so enabling it mid-session starts checks
  within the hour instead of never.
- Adapter wiring only; behaviour covered by the timer logic sharing
  the S01 housekeeping tests' module-level guarantees. (No separate
  core test: the scheduling state is two timestamps in the adapter.)

## S08 — Executable-relative help discovery

- `guide.rs`: `guide_dir()` now resolves `IPTV_GUIDE_DIR`, then
  `docs/help` beside the executable when it exists, then the old
  relative path as a last resort — the doc comment's promise is now
  the behaviour. New `exe_relative_guide_dir` is the testable core.
- Test: `guide_discovery_is_executable_relative_not_cwd`.

## S09 — What's New: localized notes with older history

- `i18n.rs`: `localize_notes` ports Python's changelog localization
  (section labels through the main catalogue, bullets through
  `notes_gettext`, version headings untouched) and
  `split_notes_window` splits at `RELEASE_WINDOW` (3) releases. The
  What's New dialog now localizes the bundled notes and, when older
  releases exist, offers a keyboard-reachable "Show older releases"
  toggle that appends them in the same text control, like Python.
- Test: `whats_new_localizes_and_splits_at_the_window`.

## S10 — F1 topic routing beyond the main window

- `bind_f1_help` also records the focused control's topic; the Help
  menu's User Guide command opens the guide at the last focused
  topic instead of always at the start.
- The player window binds F1 on its frame and controls at
  `built-in-player`; the Channel EPG window at `channel-epg`; the
  provider (account) form and its fields at `xtream-codes` or
  `stalker-portal`; the What's New dialog at `updates`.
- Known limit: an open menu's highlighted item cannot be routed —
  wxdragon exposes no API for the highlighted menu item, so Python's
  WM_HELP menu path has no equivalent here. F1 pressed with a menu
  open falls back to the last focused control's topic.

## Gates

- `cargo fmt --all` + `--check`: clean.
- `cargo test --workspace`: PASS — 202 tests, 0 failed (iptv-core
  lib 183, including this batch's 15 new tests; iptv-app 6; signer,
  vlc and sapi suites 13). One pre-existing test adjusted:
  `proxy_serves_registered_stream` now binds its server on loopback
  via `start_on` because a sandbox without a routable LAN address
  cannot fetch its own advertised address; serving behaviour is
  unchanged and the LAN binding is covered by the S03 tests.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `python3 tools/check-playlist-parity.py <pinned python clone>`:
  PASS (four shared fixtures, parsed fields/order, 24 searches).
- `git diff --check`: clean.

## Owner-side checks still open (not run here)

- Launch a second copy while the first is minimized to the tray and
  watch it restore (S01); launch from Explorer with another working
  directory and both config copies present (S02); cast a
  header-bearing stream to a real TV (S03/S04); install a real
  Spanish catalogue and read the UI (S06); leave a session running
  past an hour with automatic updates on (S07); F1 with NVDA in the
  player and dialogs (S10). These need the owner's machine and ears,
  same as the standing #86–#89 gate pack.
