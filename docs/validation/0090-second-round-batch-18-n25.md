# 0090 — Second-round batch 18: smaller dialogs (audit N25)

## Scope

- Audit finding N25 in `docs/feature-parity-audit.md`, against
  the pinned Python oracle: the About dialog's community
  links (`dialogs_misc.py:28`, `app_links.py`), the Custom
  Player dialog (`options.py:934`), the What Is Playing
  wording (`main.py:1798`–`1844`), and the Go to Channel
  errors (`main.py:2962`–`2991`).

## What changed

- About (Help > About): the bare message box is now Python's
  dialog — heading "{name} {version}", the feature summary
  verbatim, a "Community and support" heading, and the three
  community links as real hyperlink tab stops with Python's
  labels and URLs (Telegram group
  https://t.me/SerrebiProjects, the project page
  https://github.com/serrebidev/Accessible-IPTV-Client, the
  author page https://github.com/serrebidev). Links open in
  the default browser on click; Enter, numpad Enter and
  Space are routed to open as well, and only those keys —
  Python's dialog documents the same wxMSW fix, after an
  earlier every-key handler buried the screen in browser
  windows. OK and Close buttons, both ending the modal.
- Custom Player (Options > Custom Player...): Python's
  dialog instead of a bare file picker — title "Select
  Custom Player", the "Enter player executable or path:"
  field seeded with the configured path (typed paths work,
  not just picked ones), a "Browse..." button opening the
  "Select Player Executable" picker into the field, and
  OK/Cancel. An empty path on OK saves nothing, as in
  Python's `_select_custom_player`; a non-empty path saves
  `custom_player_path` and selects the Custom player.
- What Is Playing: the announcement is now Python's exact
  sentence, "{channel}. Now: {current}. Next: {next}.
  Recording: {recording}.", with each programme described
  as "{title}, {start} to {end}" in local times or "Guide
  information unavailable", the display-name fallback chain
  (name, guide name, guide id, "IPTV Stream"), and the
  recording flag read from the recording engine. The
  channel resolves as Python's does: the playing channel,
  else the most recent, else the selected one, else "No
  channel is selected." The loaded guide answers at once;
  before it loads, the persisted store is read through the
  existing cold EPG read worker (Python reads the guide
  database on its executor), and a failed read still
  answers with the unavailable wording. The player window's
  What Is Playing action routes to the same implementation
  through a `PlayerShared` hook, since the player dispatch
  has no guide access of its own. Answers land on the
  invoking window's status surface, whose label set also
  speaks the text (Python's `_show_playing_info`).
- Go to Channel Number: Python's flow and texts — no dialog
  at all when nothing is in scope; the entry prompt is
  "Enter a channel number:"; an entry that is not a
  positive number gets Python's warning box "Enter a
  positive channel number."; the lookup matches the
  channel's own number first (`number`, then `tvg-chno`
  EXTINF attributes) and falls back to the 1-based
  position; an unmatched number gets Python's information
  box "Channel number {number} was not found." The found
  row is selected as before (the G2 slice's recorded
  select-the-row behavior; Python plays the channel — see
  residuals).

## Evidence

- Gates: `cargo fmt --all` clean, `cargo clippy --workspace
  --all-targets -- -D warnings` clean, `cargo test
  --workspace` (378 tests), playlist parity check PASS,
  `git diff --check` clean.
- URLs and texts were taken from the oracle files named
  above, not from memory.

## Residuals

- Go to Channel still selects the row rather than playing
  the channel as Python does; that behavior was recorded
  for the G2 menus slice (validation 0021) and changing it
  is outside N25's error-text scope.
- What Is Playing answers on the invoking window's status
  surface; Python additionally retargets the text to the
  player window's status label when the announcement is
  invoked from the main window while the player is shown.
  The spoken text is identical either way.
- From the player window, the final selected-channel
  fallback is unavailable (the hook carries no list); the
  playing and most-recent fallbacks work.
- Brandon's NVDA pass on the About links (tab order, link
  activation) and the two Go to Channel boxes remains his,
  as with the other windows.
