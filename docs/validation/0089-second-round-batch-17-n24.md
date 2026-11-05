# 0089 — Second-round batch 17: explanatory text in forms and dialogs (audit N24)

## Scope

- Audit finding N24 in `docs/feature-parity-audit.md`, against
  Python `playlist.py` (the Xtream Codes and Stalker Portal
  dialogs), `dialogs_misc.py` (Preferred Audio Track,
  Recording Padding, Catch-up) and `planning_ui.py` (the
  recording storage check) in the pinned oracle.
- Every text below is Python's wording verbatim. All of it
  lands as visible dialog text (static text lines and prompt
  text), never as an accessible description on a control —
  the standing names-only rule for controls (validation
  0062).

## What changed

- Provider forms (`open_account_form`): each field now has
  Python's description as a static line under it. Xtream:
  "Optional nickname for this Xtream Codes account",
  "Base URL of the Xtream Codes server", "Xtream Codes
  username", "Xtream Codes password", "Select the playlist
  format to download" (Playlist type), "Preferred stream
  container format" (Stream output), and "Include the
  provider's XMLTV guide automatically" under the auto-EPG
  switch. Stalker: "Optional nickname for this portal
  account", "Base URL of the Stalker/Ministra portal",
  "Optional portal account username", "Optional portal
  account password", "MAC address presented to the portal",
  and "Try to add the portal's XMLTV guide automatically".
  Python hangs these on the fields as hint / tooltip / help
  text (and an accessible description); wxDragon has no
  field-hint or tooltip surface for these controls, so the
  text rides as dialog text instead.
- Preferred Audio Track (`open_audio_prefs`): the dialog now
  follows Python's layout order and wording — the intro
  "When a channel offers more than one audio track, the
  built-in player can switch to the track you want on its
  own.", the switch labelled "Prefer an audio description
  track when the channel has one" (was "Prefer audio
  description tracks"), the keywords field labelled
  "Preferred track names or languages, most wanted first:"
  (caption and the field's accessible name, as Python names
  the control), and the hint "Separate them with commas, for
  example: audio description, English. A track is used when
  its name contains one of these words. Leave this empty to
  keep whatever track the channel starts on."
- Schedule Padding (`prompt_schedule_padding`): Python's
  introduction — "Start scheduled recordings early or keep
  recording after the programme ends. Manual recordings are
  not changed." — now heads the first prompt.
- Catch-up (`open_catchup_dialog`): Python's intro line is
  in the dialog: "Select a programme, then press Enter to
  open it, or use the context menu to download it. Press
  Escape to close."
- Check Recording Storage (`show_recording_storage`): the
  report is Python's verbatim — "Recording folder: {path}",
  "Free space: {free} MiB", "Required reserve: {reserve}
  MiB.", then "This check does not guarantee sufficient
  space for the entire recording." (was "Recordings folder /
  Space: {n} MB free / Minimum free space before a recording
  starts", with no disclaimer and MB for MiB). A failed
  check now shows the error text alone in an error box
  (Python's failure shape) instead of embedding it as the
  space figure, and the box goes through
  `show_or_queue_notification` like Python's
  `_show_or_queue_message_box`.

## Divergences kept from earlier recorded shapes

- The padding dialog still asks its two fields one at a time
  (the recorded Rust shape); the explanation heads the first
  prompt rather than sitting above a two-field grid.
- The catch-up intro is created with the dialog's other text
  controls rather than above the list, because the shared
  `list_dialog` builder creates the list first.

## Evidence

- Gates: `cargo fmt --all` clean, `cargo clippy --workspace
  --all-targets -- -D warnings` clean, `cargo test
  --workspace` (378 tests), playlist parity check PASS,
  `git diff --check` clean.
- No behavior changed: same config keys written by every
  dialog, same validation, same storage gate.

## Residuals

- Brandon's NVDA pass on the reworded dialogs (reading order
  of the new static lines in the unsized house panels)
  remains his, as with the other windows.
