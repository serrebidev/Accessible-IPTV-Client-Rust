# Issue #28 (parity slice 6): catch-up/timeshift + downloads

Status: **automated gates PASS**. Implemented 2026-11-01 on branch
`agent/catchup`. Python remains the shipping product and the behavior
oracle.

## Implemented behavior

- `iptv-core::catchup` ports the resolution rules from `catchup_urls.py`
  and `client_epg.py`:
  - has-catchup detection: `catchup-source`/`catchup` attributes, the
    teleelevidenie implicit template (a `timeshift="N"`-only host rule),
    and Stalker timeshift/archive flags.
  - the catch-up window check against `catchup-days`/`timeshift`, with
    Python's lenient unparseable-window behavior.
  - `resolve_show_url`: airing now → live; future → live; finished →
    catch-up with the reference's exact error messages for no-catch-up,
    outside-window, and invalid programme times.
  - template expansion with the full placeholder table (`${start}`,
    `{utc}`, `{S}`, `{Y}`…), the offset shift, and the provider wall
    clock from `catchup-timezone` (UTC aliases and IANA zones via
    chrono-tz, local fallback like Python).
  - the Xtream timeshift URL implied by a live URL
    (`/timeshift/U/P/MINUTES/YYYY-MM-DD:HH-MM/ID.ts`), keeping the live
    URL's query and header-modifier tail, adding the channel User-Agent
    when the tail lacks one.
  - generic path styles: `<source>/<start>/<duration>/` and flussonic's
    `<stamp>-<duration>.m3u8`, with stream-id extraction and the same
    User-Agent tail rules.
- Stalker archive resolution joins the provider worker
  (`resolve_catchup`, start text in local `YYYY-MM-DD:HH-MM` form), and
  its result completes a pending play through the same event the live
  resolution uses.
- `iptv-core::downloads`: a background `DownloadWorker` that streams a
  resolved URL to a destination file with progress events and
  cancellation (partial file removed), sending the channel's playback
  headers (`channel_headers` reuses the slice-3 header port).

## Evidence

- 83 workspace tests pass (6 new: template expansion with wall-clock
  tokens, Xtream timeshift URL in UTC and local forms, the
  teleelevidenie append flow end-to-end through `resolve_show_url`,
  window/error messages, flussonic path style, download header pairs).
- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets`
  (zero warnings), `tools/check-playlist-parity.py` PASS against pinned
  Python 9aa89e0, `git diff --check`.

## Evidence boundary

- Per-programme play/download is driven from the EPG programme UI in
  Python; that surface (the EPG viewer) is not built yet, so this slice
  is core + workers, like the account dialog deferral in slice 5.
- Python's catch-up *download* runs through its ffmpeg recorder
  (redirect probing, retry policy, audio classification); the recorder
  is slice 7, and this slice's download worker is the plain file
  transfer half.
- Wall-clock offset subtraction is epoch-based; Python subtracts on the
  zone-aware datetime, which can differ by an hour across a DST edge for
  the Xtream stamp. Recorded here rather than hidden.
