# 0080 — wxMenu::Delete assert on submenu rebuilds

Date: 2026-11-04. Reported by Brandon from the Windows
release build: a wxWidgets debug check stopped the app at
`menucmn.cpp(486): assert "item" failed in Delete():
invalid item in wxMenu::Delete`.

## Root cause

`wxMenu::Delete(id)` requires the item to be in the menu;
deleting an absent id trips that assert in debug builds and
is invalid in release builds too. Three player-menu rebuild
sites deleted placeholder ids unconditionally:

- The Subtitles placeholder "No subtitle tracks available"
  is built with id `PM_SUB_BASE` — the same id the first
  radio item ("Off") gets when `refresh_subtitle_menu`
  republishes the list. The refresh drains the tracked radio
  ids (which include `PM_SUB_BASE` once the radios exist)
  and then deleted `PM_SUB_BASE` a second time. The second
  subtitle refresh on any window therefore always asserted.
- The Audio Track placeholder `PM_NO_TRACKS` is deleted by
  both `apply_engine_to_playback_menu` (which re-adds it)
  and the audio track refresh (which does not). A track
  refresh after a refresh, or applying the engine after a
  track refresh, deleted an id that was no longer there.

## Fix

Every menu deletion in the app now goes through
`menu_delete_if_present`, which checks
`Menu::find_item(id)` first and deletes only when the item
is actually present. All six call sites were converted (the
two drain loops and placeholder delete in each of the three
rebuilds); a grep for `.delete(` in the app sources now
finds only the guarded call inside the helper itself. The
end states are unchanged: the placeholder is restored by
the engine-apply path exactly as before, and rebuilds leave
exactly the fresh radio items.

## Verification

- Root cause established by reading the three rebuild sites
  against the menu construction (placeholder ids at build,
  drained id sets per refresh) and the wxdragon 0.9.24
  `Menu::find_item` / `Menu::delete` signatures.
- Full gates green locally: `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`, playlist parity against the
  pinned Python oracle, `git diff --check`.
- The assert itself fires only inside a running wx session;
  Brandon's re-run of the Windows build (play two streams
  with different subtitle/audio lists, or switch engines) is
  the live confirmation. The guarded delete makes the
  invalid call structurally impossible: wx is only ever
  asked to delete an item `find_item` just returned.
