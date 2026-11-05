# 0087 — Second-round batch 15: browsing details (audit N22)

## Scope

- Audit finding N22 in `docs/feature-parity-audit.md`, against the
  pinned Python oracle: `channel_names.py` (`group_synonyms`,
  `extract_group`), `main.py:1411`–`1417` (scope choice labels),
  `main.py:1762`, `:1869`, `:1927` (Favorites boxes), `main.py:2664`
  (Delete key), and `main.py` ~6148–6200 with `channel_widgets.py:51`
  (the category tree, including the "Other playlists" branch at
  `main.py:6180`).

## Per sub-item

1. **Group synonyms / `extract_group` — already covered, cited.**
   Ported in the N11 batch: `crates/iptv-core/src/guide_match.rs`
   has `extract_group` and `variant_tags()`, driven by
   `GROUP_SYNONYMS` in the generated
   `crates/iptv-core/src/guide_match_data.rs`, including Python's
   word-variant exclusions (`can`, `car`, `nor`, `per`, `ind` never
   match as free words) and the leading-code and parenthesised-code
   rules. In Python, `extract_group` feeds EPG matching votes
   (`playlist.py`), not the category list itself, so no browsing
   work remained here.
2. **"Other playlists" branch — implemented.** Core
   `categories::branch_categories` builds the All-playlists
   category list as per-playlist branches when at least one source
   is configured: `Category::PlaylistAll(scope)` then its
   `Category::PlaylistGroup(scope, group)` leaves, groups sorted.
   Configured scopes come first in picker order; any scope no
   configured source owns — including untagged channels — forms
   the leftover branch the app labels "Other playlists"
   (`main.py:6180`). `BrowserWorker::set_branch_sources` is fed
   from `fill_scope_choices`; with no configured branches the
   settled flat list is unchanged. The app renders the tree like
   Python's `_AccessibleCategoryTree`: group names nest via
   `categories::category_path_parts` (strip a trailing " (n)"
   count, split on " > " else "/", trim, drop empties), nodes are
   shared by path prefix, a category that is itself a branch keeps
   its leaf text, every branch starts expanded, and branch headers
   select nothing. Branch leaves carry Python's counts:
   "All Channels (n)" and "Group (n)".
3. **Account-type scope labels — fixed.** The scope picker now
   uses `ProviderAccount::scope_label()`:
   "Xtream Codes – {name}" / "Stalker Portal – {name}" with
   Python's fallback chain (name, then username, then base_url,
   then "Provider") and its en dash. `describe()` is unchanged —
   it keys provider clients state and VOD provider ids, where the
   parenthesised form is the established identity.
4. **Empty-Favorites and removal messages — implemented as
   boxes**, all titled "Favorites" and shown via
   `show_modal_tracked`:
   - Toggling an entry with no channel key (`Toggle::Unchanged`)
     shows Python's warning box "This entry cannot be added to
     Favorites." (`main.py:1762`) instead of the old status-line
     text.
   - A removal inside the Favorites view that empties it shows
     "{name} was removed. Favorites is now empty."
     (`main.py:1869`).
   - Go to Favorites with no favorites shows "You have not added
     any favorites yet. Select a channel and press Ctrl+D to add
     it." (`main.py:1927`) and does not navigate. Ctrl+D is the
     favorite binding here too, so the text is exact.
5. **Delete removes a favorite — was missing, implemented.** The
   channel list now binds KEY_DOWN: a plain Delete (code 127, no
   Ctrl/Alt/Shift) on a row whose channel is a favorite runs the
   same toggle flow as Ctrl+D and consumes the key
   (`main.py:2664`); any other Delete passes through untouched.

## Evidence

- New core tests: `scope_label_uses_python_form_and_fallbacks`
  (providers), `branch_categories_order_scopes_then_leftovers`,
  `branch_rows_filter_by_scope_and_group`,
  `path_parts_split_and_strip_counts` (categories), and
  `branch_categories_split_the_all_view_per_playlist` (browser:
  branch order, per-branch row counts, leaf filtering, the
  untagged leftover branch, and restoring the flat list).
- Gates: `cargo fmt --all` clean, `cargo clippy --workspace
  --all-targets -- -D warnings` clean, `cargo test --workspace`
  (383 tests), playlist parity check PASS,
  `git diff --check` clean.
- The tree rendering, the three boxes, and the Delete binding are
  GUI wiring; wx handles cannot be built headless, so they are
  covered structurally (the row model and counts are core-tested)
  and by the gates.

## Residuals

- Python's third scope-label branch, "Provider – {name}", has no
  Rust counterpart: `ProviderAccount::from_config` only models
  Xtream Codes and Stalker accounts, so no picker entry can take
  that label.
- Flat (single-scope or no-source) category order stays the
  settled first-appearance order, and flat group rows carry no
  counts; only the All-playlists branch leaves gained Python's
  counts. Both were earlier-slice behavior, left as recorded.
- Provider accounts without a provider id contribute untagged
  channels, which land in the "Other playlists" branch; Python
  tags every loaded source, but the Rust scope picker already
  cannot select such an account (pre-existing B01-era behavior),
  so the branch is the consistent outcome.
- Python's "Select a channel first." box (`main.py` ~1755) is not
  cited by the audit; the Rust no-selection status line is
  unchanged.
