# Issue #20 (parity slice 2): remote playlists and multi-source merge

Status: **automated gates PASS**. Implemented 2026-11-01 on branch
`agent/remote-playlists`. Python remains the shipping product and the
behavior oracle.

## Implemented behavior

- `iptv-core::sources` ports the Python source model: a configured source
  is a string (local path or http(s) URL); its channels are tagged
  `playlist-id` with the scope id `m3u:` + SHA-256 of the source string
  (Python `_source_scope_id`, verified against the published SHA-256
  test vector for "abc").
- Merging concatenates sources in configuration order. The reference does
  not dedupe channels across playlists, and neither does this port;
  duplicate names from different sources all stay, each with its own tag.
- `scope_indexes` mirrors `_scoped_channels`: the empty scope (All
  playlists) accepts everything; a specific scope accepts only exact tag
  matches, so untagged legacy channels stay out of a single playlist's
  view.
- Fetching runs on the existing loader worker with the same generation /
  cancellation discipline as local loads. Local sources reuse the
  cancellable chunked reader and its decoding rules; URL sources fetch
  over HTTP(S) (ureq + rustls, 300 s ceiling like the reference, 404 maps
  to Missing, other failures to Unreadable). Failures are per-source: one
  dead source never sinks the others; only when nothing at all was
  produced does the load report the first failure, keeping the previous
  channels per the recovery contract.
- The desktop adapter loads the configured `playlists` list merged at
  startup when it is non-empty. The single-file Open dialog path is
  unchanged.

## Evidence

- 54 workspace tests pass (6 new source tests: scope-id vector, merge
  concatenation/tagging/duplicates, scope filtering incl. untagged
  exclusion, dead-source tolerance, all-dead first-failure, and the real
  DefaultFetcher against temp files).
- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets`
  (zero warnings), `tools/check-playlist-parity.py` PASS against pinned
  Python 9aa89e0, `git diff --check`.

## Evidence boundary

HTTP fetching is covered by error mapping and the fetcher seam, not by a
live network fixture. The Playlist Manager dialog (add/remove/rename
sources) is not part of this slice; sources are read from the config
file, which the Python manager also owns. Scope selection UI arrives
with the manager work; the core filter is in place and tested.
