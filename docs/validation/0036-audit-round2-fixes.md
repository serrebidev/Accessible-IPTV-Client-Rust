# 0036 — Audit round 2: shortcut dispatch and editor fixes

Date: 2026-11-01. From the full Rust GUI audit (taken at the
stalker-dvr-hardening state) diffed against current main; items
already fixed by PRs #91/#93/#100 are not repeated here.

## Fixed in this change

- Main-window hotkeys with no menu item were never dispatched
  (wxdragon has no accelerator table; only menu `\t` suffixes
  fired). Ctrl+Up / Ctrl+Down (built-in player volume, steps of
  2, Python's main-window behavior) and Ctrl+Shift+R (record the
  selected channel) now ride KEY_DOWN bindings on the six
  focusable main controls, next to the F1 bindings, matched
  against the effective main shortcut table — so user remapping
  applies to them too.
- Shortcuts editor showed each label twice and prefilled the
  Edit dialog with the "{label}: {binding}" display string.
  Core `searchable_commands` now also returns the raw binding;
  rows render "{context} — {label} — {binding}" and Edit starts
  from the bare binding.
- Player key labels: Escape is now spelled "Esc", matching the
  core parser's canonical form — an Esc player binding could
  never match a key event before.
- Stale accessibility text: the Search description pointed at
  the Toggle favorite button and the Categories description at
  the retired Cancel loading button; both now describe the
  actual tab ring. The tree's accessibility label is
  "Categories", not "Category".

## Filed, not in this change

- #101 main-window layout (two columns, no button row, no
  visible Playlist status field).
- #102 announcement-level gating for player announcements.
- #103 tray menu parity.
- The View menu's favorite item keeps its static "Add to
  Favorites" label: wxdragon's Menu offers no set_item_label,
  and the menu handle is consumed by the menu bar, so a
  state-following label needs a retained-handle workaround.
  The channel context menu's favorite item does follow state.

## Evidence class

Automated: fmt clean, clippy zero warnings, 132 workspace tests
pass (core searchable-commands test extended), playlist parity
PASS. Owner-heard NVDA evidence remains issue #86.
