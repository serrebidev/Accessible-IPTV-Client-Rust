# Validation 0023: GUI parity G3b — Categories as a tree

Date: 2026-11-01. Issue: #57 (program #54).

## Landed

- The Categories control is now a wx TreeCtrl with a hidden root,
  matching the Python main window's category tree. Top-level items
  are the categories in order (All Channels, Favorites with its
  live count, then groups).
- sync_category_control rebuilds the tree's children when the
  category set changes (and refreshes the Favorites count label),
  expands the root, and keeps the selection on the active category
  without moving focus — the same contract the ListBox version had.
- Selection flows through TREE_SEL_CHANGED; item identity is by
  the wx item id (tree_item_index / tree_item_at helpers), also
  used by the View menu's Go to Favorites.

## Automated evidence

- cargo fmt --check, clippy zero warnings, 113 workspace tests
  pass, playlist parity PASS, git diff --check clean.

## Owner-heard evidence still owed

NVDA pass: tree navigation (arrows, first-letter), selection
announcements, and the Favorites count label in tree form.
