# Local browse fixture

Python reference: `serrebidev/Accessible-IPTV-Client` revision
`9aa89e053225ef2c83c4f3659775a00723012ff7`, inspected locally.

`local.m3u` uses the ABC East metadata example and the quoted-comma and
apostrophe-in-name cases from `tests/test_m3u_plus.py`. Synthetic UTF-8,
single-quoted, lower-case directive, unknown attribute and bare URL entries
exercise the common parser rules in `client_m3u.py` and `playlist_headers.py`.
URLs are fixture data only; tests never fetch them.

Expected shared fields are name, group, URL, tvg-id and tvg-name, in source
order. This ticket additionally requires preserving tvg-logo and unknown
attributes; Python deliberately omits logos in this revision. Those fields
are Rust retention requirements, not a claim of identical Python output.
This fixture is focused browse evidence, not the full parity gate of ticket 04.

## Recovery/parity fixtures (issue #5, ticket 04)

Every fixture in `python-channels.tsv`, `python-search.tsv`, and
`python-outcomes.tsv` represents Python revision
`9aa89e053225ef2c83c4f3659775a00723012ff7` (also recorded in each golden header).
`recovery.m3u` adds CR-only lines, duplicate keys, overwritten/dangling metadata,
unbalanced quotes, duplicate names, a bare URL and Unicode search ranking.
`empty.m3u` has only a header/comments. `invalid-utf8.m3u` exercises the Python
local loader's `utf-8-sig`, `errors="ignore"` policy. All URLs are inert fixtures.

Regenerate or check from committed Python source (no UI imports or user config):

```powershell
python tools/check-playlist-parity.py <Python-checkout> --update
python tools/check-playlist-parity.py <Python-checkout>
cargo test -p iptv-core
```

The oracle extracts the actual parser, attribute regex, separator and ranking
function from committed source with AST; it does not copy their implementations.
Goldens cover the five shared channel fields, source ordering, 24 search queries,
and loaded/empty outcomes. Rust additionally retains logos/unknown attributes.
Python ignores missing configured sources and returns read exceptions through its
load result; Rust's local chooser exposes explicit missing/unreadable recovery
with choose/retry guidance. Recovery wording and malformed-input warnings are
requirements of this slice, not byte-identical Python strings. Worker/controller
tests cover missing/unreadable/retry, retention, cancellation, and replacement.
Domain tests and these goldens do not prove desktop announcements or focus.

## Stable favorites fixtures (issue #11, ticket 02)

`favorite-channels.tsv` supplies synthetic loaded-channel fields, including
provider/stream identity, URL refreshes, credentials in inert URLs, duplicate
names across groups, missing/whitespace fields, aliases, pipes and backslashes.
The first column names each case; subsequent columns use Python metadata names.
Empty values model absent optional strings in Rust's loaded `Channel` type.
`favorite-stored.tsv` supplies typed stored-value cases: `s:` marks strings;
nulls, numbers, booleans, maps and lists are invalid entries. Missing, bare-string
and bytes containers normalize to no keys. A final `.` preserves empty TSV fields.

`python-favorites.tsv` records 27 keys, six normalization results, and 54
successive toggle/filter outcomes from committed `favorites.py` at Python
revision `9aa89e053225ef2c83c4f3659775a00723012ff7`. The Rust integration test
consumes the same inputs and goldens without invoking Python or copying its
implementation. The oracle runs the committed, GUI-free module itself:

```powershell
python tools/check-favorites-parity.py <Python-checkout> --update
python tools/check-favorites-parity.py <Python-checkout>
cargo test -p iptv-core --test favorites
```

Rules match `tests/test_favorites.py`'s identity, normalize, filter and toggle
cases. Plain M3U namesakes share one favorite across groups. Provider keys include
display name as well as provider type/ID, stream ID and XMLTV ID; renaming a
channel changes its key in this reference. Fields are omitted when absent, not
given invented IDs. Group, logo, resolved URL and credential attributes are never
used. Backslash is escaped before pipe. Keys remain case-sensitive. Stored keys
are opaque: trim, drop blanks/invalid entries, deduplicate in first-seen order;
do not parse, unescape, or discard unknown nonblank keys.

Rust accepts typed optional strings at the storage boundary: a future config
adapter must map invalid entries to `None` and missing/non-list configuration
to an empty iterator. Persistence and desktop Favorites controls are outside
this ticket. Filtering scans the supplied playlist in source order and should
run on a worker for large playlists. This evidence does not prove NVDA speech,
keyboard traversal or focus preservation after future desktop integration.
## Grouped categories (issue #10)

`categories.m3u` adds out-of-order groups, missing/whitespace-only metadata,
explicit Uncategorized, duplicate channel names with distinct tvg-id values,
and a literal All Channels group. The same pinned Python revision above is
used by `python-categories.tsv`. The oracle extracts the real grouping fallback
from `main.py`, the `sorted(scoped_groups)` expression in `_refresh_group_ui`,
and `_source_for_group`; scoped group keys distinguish a literal group from
the All Channels sentinel. No user config, UI modules or network are loaded.
The parser trims group values; missing, empty and whitespace-only groups join
Uncategorized. All Channels is first, then case-sensitive lexical group order;
category projections retain source order. Nonempty searches retain existing
Python name ranking within the selected category. All five fixtures now cover
30 searches as well as fields, load outcomes and category projections.

Rust uses a typed All sentinel and displays a literal All Channels group as
`All Channels (playlist group)`. This avoids the ambiguous unscoped string-key
path in Python while matching its scope-aware group projection. Categories
are flat in this local seam: Python tree hierarchy/provider branches and
Favorites are outside this ticket. Selection identity is the existing source
index within the same immutable playlist snapshot; duplicate names/URLs remain
separate rows. A successful playlist replacement keeps an existing category
key or falls back to All Channels, and keeps the existing first-row selection
policy for a new snapshot. Persistent/favorite identities remain #11/#12.
