# 0109 — C8: Automatically Play the Next Episode (round 3)

Port target: Python v1.151.6 (`a29ad9a3`) `episode_continuation.py`
(`natural_end`), `episode_continuation_ui.py` (`finished`,
`NextEpisodeDialog`), and the `auto_next_episode` toggle in
`accessible_tools_ui.py` (`_add_sound_tools_menu`, Player menu).
Plan: `docs/superpowers/specs/2026-11-23-python-parity-round3-design.md` (C8).

## What landed

- Core (`episode_continuation.rs`): `natural_end` (VOD only,
  seen playing, not manually stopped, ending within the
  smaller of 2.5 s and 5% of the media before the end, or at
  most 1 s past it), `COUNTDOWN_SECONDS` (10) and
  `countdown_seconds` (per-tick label seconds, floored at 1).
- Core (`config.rs`): `auto_next_episode`, default false,
  coerced on normalize like Python's `options.py`.
- App (`main.rs`): the Player menu check
  "Automatically Play the Next Episode" (saved with
  `set_and_save`, help topic `video-on-demand`), the
  continuation poll on the main timer — one worker lookup per
  generation, strict Xtream re-fetch for episodes, the
  launch snapshot otherwise — and the `NextEpisodeDialog`:
  title "Next Episode", label
  "The next episode starts in {seconds} seconds: {title}",
  Play Now and focused Cancel, Escape cancels, 250 ms tick
  cancels when newer playback or a playlist change moves the
  generation/playlist pointer or the option turns off, and
  the lookup error arrives as Python's warning box
  ("Could not load episodes:...", "Next Episode").
- English help (`docs/help/en.md`, Video on demand): Python's
  paragraph verbatim. Dialog and menu strings already have
  catalogue entries, so no new translations were needed.

## Evidence

- Core tests beside the code: `episode_continuation.rs`
  (`natural_end_matches_python`, `countdown_ticks_down_and_floors_at_one`).
- `cargo test -p iptv-core` green; `cargo check -p iptv-app`
  green. `cargo clippy -p iptv-core -p iptv-app --all-targets -- -D warnings`
  reports only pre-existing lints in untouched files under
  this toolchain; no hits in the files this group touched.

## Brandon's NVDA check

1. Player menu: check "Automatically Play the Next Episode";
   restart — still checked.
2. Play a series episode to its end: "Next Episode" opens,
   label counts 10 to 1, focus starts on Cancel. Let it run
   out: the next episode starts by itself.
3. Play Now starts it at once; Cancel and Escape leave
   playback ended.
4. Stop a playback by hand before the end, or play a live
   channel: nothing opens.

## Residuals

- The countdown dialog itself does not move the main
  window's playlist pointer check for playlist loads that do
  not change the browsed model; newer playback (session
  generation) always cancels.
- The modal-box guard skips the countdown while another
  modal box owns input, exactly as Python does; a queued
  notification behind it still shows.
