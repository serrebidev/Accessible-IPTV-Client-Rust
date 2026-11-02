# Validation 0030: follow-up pack (core) — cache, proxy, Sonos, recording gate

Date: 2026-11-01. Program #54 follow-ups.

## Landed

- Provider playlist cache (iptv-core::cache + providers):
  Xtream playlist text and Stalker channel lists (JSON) are
  cached under the config dir's provider-cache/, keyed by a
  hash of the account identity. Fresh copies (6 h) skip the
  fetch entirely; on fetch failure a stale copy serves with a
  note in the load failures. Writes are atomic.
- Stream proxy (iptv-core::proxy): a loopback HTTP server on an
  ephemeral port. Cast targets receive a proxy URL when the
  channel carries headers (attributes or pipe modifiers); the
  proxy injects the headers upstream, rewrites HLS playlists
  (entries and URI="..." attributes) so segments follow, passes
  Range through, and caches small segment bodies in memory.
- Sonos play path: Sonos devices (manufacturer or name match)
  get the sonos_didl metadata shape over AVTransport instead of
  the generic video DIDL.
- Recording storage gate: starting a recording (manual or DVR,
  shared start path) refuses under 500 MB free (Unix df; other
  platforms skip the gate), and the saved announcement now
  reports the file size.

## Automated evidence

- cargo fmt --check, clippy zero warnings, 122 workspace tests
  pass (new: cache key/freshness round trip, proxy absolutize +
  playlist rewrite + live serve-through test against a local
  upstream, Sonos detection), playlist parity PASS, git diff
  --check clean.

## Owner-heard evidence still owed

Casting a header-carrying channel through the proxy to a real
device, and a Sonos play, on Brandon's network.
