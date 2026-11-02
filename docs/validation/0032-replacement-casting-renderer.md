# Validation 0032: casting protocol and renderer evidence

Issue: #82. This record separates deterministic protocol confidence from real
renderer acceptance.

## Protocol evidence

The existing cast-core tests cover URL and MIME handling, SSDP discovery
parsing, UPnP control URLs and SOAP payloads, Roku launch/key requests, Kodi
JSON-RPC envelopes, Sonos DIDL construction, and proxy-header normalization.
These tests use synthetic messages and inert URLs. They do not contact a device
or claim that a renderer played media.

The automated protocol gate is:

```text
cargo test -p iptv-core cast
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
git diff --check
```

Record the exact revision and test output. Chromecast, AirPlay/RAOP, Sonos
native control, Yamaha side-channel control, mDNS discovery, and the Python
relay/transcode compatibility machinery remain out of scope.

## Physical-renderer gate

When a compatible renderer is available on the same test network, record:

1. renderer identity and firmware/model, without credentials;
2. discovery or explicit device refresh and the spoken device/status result;
3. selection of a sanitized fixture channel and Play;
4. observable renderer playback or a clear device-side failure;
5. Stop, Refresh, and recovery after an unavailable/offline device;
6. the owner-heard status and focus behavior in the cast window.

Each row is `Pass`, `Fail`, `Blocked`, or `Not run`. A parser or builder test
cannot pass a physical row. No renderer available is `Blocked`, not `Pass`.

## Evidence record

Record the app revision, fixture identity, renderer identity if present, network
conditions, exact actions, observed status/speech, and any follow-up issue. Keep
private network details and credentials out of the repository and issue data.
