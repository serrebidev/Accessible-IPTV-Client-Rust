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
