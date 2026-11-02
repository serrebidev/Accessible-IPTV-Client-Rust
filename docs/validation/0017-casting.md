# Issue #42 (parity slice 10): casting (Caster protocol cores) + proxy headers

Status: **automated gates PASS**. Implemented 2026-11-01 on branch
`agent/casting`. Python remains the shipping product and the behavior
oracle.

## Implemented behavior

- `iptv-core::cast` ports the self-contained protocol cores of the
  Python Caster stack:
  - engine helpers: `url_path`, `guess_mime`, `roku_mime`,
    `youtube_id`, and `looks_like_mpegts` with the reference's stride
    search (aligned fast path at 188/192, mid-packet search requiring
    four stride hits);
  - SSDP M-SEARCH discovery with the repeated-send window and
    LOCATION collection/dedupe;
  - UPnP/DLNA: device description parsing (friendlyName, manufacturer,
    AVTransport/ConnectionManager control URLs resolved against the
    description URL per the UPnP rules the reference implements),
    sink-mime probing through GetProtocolInfo (empty means "did not
    say"), and the SOAP control set — SetAVTransportURI with the
    reference's exact DIDL-Lite plus Play, Stop, GetTransportInfo
    state, and RenderingControl SetVolume via the service-name swap;
  - Roku ECP: SSDP discovery, description names, the launch URL with
    the video/audio format maps, Home-as-stop, keypress posts;
  - Kodi JSON-RPC: the request envelope with Basic auth, Player.Open,
    and in-band error surfacing ("Kodi refused …" instead of a false
    success);
  - the Sonos DIDL-Lite builder (musicTrack tagging, per the
    reference's reasoning);
  - stream_proxy.py's header normalization: the canonical alias map
    plus Title-Case fallback, `_extra` handling, no-overwrite extras,
    and the default upstream User-Agent rule (the same normalizer the
    recorder's header args are built from).
- A `CastWorker` runs discovery (UPnP + Roku) and device control off
  the UI thread.
- Adapter: a Cast button opens the cast window for the selected
  channel — device list, Play, Stop, Refresh, spoken status; the
  channel's clean URL (modifiers stripped) goes to the device.

## Evidence

- 101 workspace tests pass (4 new: mime/Roku labels, YouTube ids and
  TS stride detection incl. mid-packet starts, description parsing
  with relative control URLs + DIDL escaping, Roku launch URL and
  header normalization).
- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets`
  (zero warnings), `tools/check-playlist-parity.py` PASS against
  pinned Python 9aa89e0, `git diff --check`.

## Evidence boundary

- No physical renderer was available in this lane; discovery and
  control are exercised at the builder/parser level, with the wire
  shapes copied from the reference implementations.
- Not ported: Chromecast (the CAST protocol needs protobuf + TLS
  session machinery), AirPlay/RAOP, Sonos native control (SoCo),
  Yamaha YXC side-channel, and mDNS-based discovery (Kodi's zeroconf
  path — its RPC core is here). Python's compatibility machinery —
  the local FileServer, the TS relay/transcode sources, sink-mime
  driven transcode decisions — is likewise not in this slice;
  casting sends the channel URL directly, and per-call HTTP timeouts
  ride ureq's defaults rather than the reference's per-protocol
  values.
- `guess_mime` uses a media-extension table in place of Python's
  full mimetypes database; the extensions IPTV URLs actually carry
  are covered.
