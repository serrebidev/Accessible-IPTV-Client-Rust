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
