# Issue #26 (parity slice 5): providers

Status: **automated gates PASS**. Implemented 2026-11-01 on branch
`agent/providers`. Python remains the shipping product and the behavior
oracle.

## Implemented behavior

- `iptv-core::providers` ports `providers.py`:
  - Base-URL normalization (scheme defaulting, userinfo stripped, portal
    filenames stripped) shared by both clients.
  - Xtream Codes: get.php playlist URL/fetch with the reference's
    auth-failure detection (a 200 that is not a playlist = rejected
    credentials), xmltv.php EPG URLs, the `player_api` surface (account
    info, VOD/series categories and streams, series info) and the
    movie/series/live stream URL builders with Python's quoting.
  - Stalker/Ministra: MAG handshake + optional login, 25-minute token
    cache, token dropped on 401/403 so one rejection cannot poison the
    next 25 minutes of calls, `get_all_channels` mapped to playlist
    channels with the resolution data kept as attributes, account info,
    and `create_link` stream/catch-up resolution with the `ffmpeg `/`auto `
    prefixes stripped.
- The account report ports `account_info.py`'s formatting: header with
  redacted server URL and masked MAC, subscription lines (status label,
  expiry with day counts, trial, connections, created, formats, provider
  message), the server block, and the extras pass that shows every other
  scalar the server sent except secret-named fields.
- Adapter: `playlists` config entries that are provider dicts load in the
  background through `ProviderWorker` (which owns the Stalker clients and
  their tokens); channels merge into the browse model alongside plain
  sources, provider EPG URLs feed the guide worker, and pressing Enter on
  a Stalker channel resolves its temporary link in the background, then
  plays. `BrowserWorker` gained an extras channel intake for this merge.

## Evidence

- 77 workspace tests pass (6 new provider tests: base-URL normalization,
  Xtream URL shapes and quoting, Stalker row mapping, report redaction /
  MAC masking / secret filtering, Xtream report lines, config entry
  parsing).
- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets`
  (zero warnings), `tools/check-playlist-parity.py` PASS against pinned
  Python 9aa89e0, `git diff --check`.

## Evidence boundary

- No live portal was exercised: both clients are covered by unit tests
  over URL construction, payload mapping and report formatting, not by a
  real Xtream server or Ministra portal. The Stalker flow sends the
  manual MAC/timezone cookie like the reference; portals that demand
  server-set session cookies are untested.
- The account report is produced by the worker and its text is
  unit-tested, but the accounts dialog that requests and displays it is
  shell UI and lands with slice 12; the adapter currently surfaces only
  the first line if a report is requested programmatically.
- Provider playlist caching (Python's parsed-playlist cache) is not
  ported; provider loads always fetch fresh.
