# Validation 0085 — Second-round batch 13: Chromecast casting (N20)

Date: 2026-11-04. Branch: agent/audit-n20. Audit item: N20, casting
extras beyond the known protocol gap — Chromecast proper, the
connected-device prompt, the saved-position and local-buffer refusals,
the Windows firewall rule, and the single_stream_busy caster arm.

Brandon reversed the Chromecast non-port on 2026-11-04 ("Casting is
possible", pointing at the `chromecast` crate on docs.rs). AirPlay
remains a non-port and is untouched.

## What was built

### Core (crates/iptv-core/src/cast.rs)

New dependencies, workspace level: `rust_cast` (the `chromecast` crate,
0.18.2 — its lib target is named `chromecast`, so the dependency is
renamed to the `rust_cast` name the docs use) and `mdns-sd` (0.20.3),
both at the latest compatible versions.

- `CastKind::Chromecast` joins Upnp and Roku. `CastWorker` discovery
  now scans Chromecast first, then UPnP, then Roku, matching the
  Python scan order.
- Discovery: `chromecast_discover` browses `_googlecast._tcp.local.`
  with mdns-sd for the bounded window, shapes each resolved service
  into the existing `CastDevice` record with pure helpers
  (`chromecast_display_name` — TXT `fn`, else the service name's first
  label; `pick_cast_host` — IPv4 preferred, deterministic order;
  `chromecast_device` — `control_url` carries `host:port`, port from
  the service or 8009, manufacturer from TXT `md` or "Chromecast") and
  disambiguates duplicate friendly names as `name (host)` like Python.
  Unit tests cover the host pick and the record shaping.
- Session: `ChromecastSession` is created, used, and dropped entirely
  inside the cast worker thread. That confinement is forced by the
  crate: its message manager holds the stream lock across a blocking
  read, so a second thread touching the device (a reader thread, a
  side stop) would deadlock against any in-flight read. Connect uses
  `connect_without_host_verification` (Chromecasts present self-signed
  certs; both of the crate's connect paths install a no-verify TLS
  verifier, and this one skips the certificate parse too), then the
  Python-equivalent Cast V2 flow: connect to `receiver-0`, heartbeat
  ping, launch the Default Media Receiver (Python's CAST_APP_ID
  CC1AD845 is that same app), connect to the app's transport id, and
  `media.load` with the cast proxy URL and the guessed content type
  (stream type Live for HLS, Buffered otherwise). One content-type
  flip retry on a load error, like the UPnP path. A successful load is
  confirmed against the media status: Playing is success, an Idle
  status with a reason fails with Python's "did not start playing"
  wording, and Buffering still up at the 8-second bound counts as
  started (Python's ladder fails that case after its own longer wait;
  recorded divergence, the status poll that follows playback start in
  Python is a UI nicety this port does not mirror).
- Upkeep: the worker's command loop now waits with a 200 ms timeout;
  on each timeout the live session gets a heartbeat ping (a ping error
  drops the session) and up to four pending messages are received and
  answered (heartbeats get pongs). The receive bound is safe because
  the upkeep's own ping guarantees a pong is in flight, so a receive
  never waits longer than one round trip in practice.
- Stop mirrors Python's `_release_cast`: media stop, then disconnect
  the media transport, then drop the session. The receiver app itself
  is not stopped, same as Python.

### App wiring (crates/iptv-app/src/main.rs)

- `CastView` now tracks the live session: `active_cast` (device plus
  the pre-proxy source URL of what was cast) is promoted from
  `pending_cast` when the worker reports `Played(Ok)` with the
  matching device name, and cleared on `Stopped`/play failure.
- Disconnect prompt (Python main.py:8703): pressing Play in the cast
  window while connected, or opening the cast window while connected,
  asks "Currently connected to: {device}\n\nDisconnect?" titled
  "Casting" (Yes/No, question icon, via `show_modal_tracked`). Yes
  stops the cast on the worker thread and aborts the pending action;
  No leaves the session alone and also aborts, exactly like Python.
- Saved-position refusal (Python main.py:6279): Continue Watching's
  Resume now checks the player choice and the cast state before
  starting; a non-built-in player or a live cast shows "Saved positions
  can only be restored in the built-in player. Select it and disconnect
  casting before continuing." titled "Continue Watching" and does not
  start playback.
- Local-buffer refusal (Python client_timeshift.py): the buffer
  intercept's cast context is real now (it was hardcoded off in
  validation 0045). While casting with a live buffer session,
  "Stop local buffered playback before switching to a cast device."
  titled "Local Live Buffer" shows and the switch is intercepted; new
  buffering while casting is excluded through the same `wants` gate
  Python uses.
- `single_stream_busy` gained the caster arm its comment always
  described: a live cast whose retained source URL maps to the same
  single-stream provider key marks the provider busy. All four call
  sites (probe, recording toggle, DVR start, catch-up download) pass
  the tracked cast source URL.

### Firewall rule (crates/iptv-core/src/proxy.rs)

Python stream_proxy.py:1579 parity: when the stream proxy binds, on
Windows it deletes then re-adds the inbound rule `IPTV Proxy ({port})`
(TCP, local port, private+domain profiles) via netsh with
CREATE_NO_WINDOW, all output captured and every failure ignored; the
rule is removed when the proxy stops (the server's `Drop`). Off
Windows both functions are no-ops, so the call sites are unconditional.

## Gates

- `cargo fmt --all && cargo fmt --all --check`: clean.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo test --workspace`: 380 passed, 0 failed, 5 ignored
  (378 at batch 11 plus the two new cast tests).
- `python3 tools/check-playlist-parity.py ~/workspace/pyclone-pinned`:
  PASS (pin unchanged).
- `git diff --check`: clean.

Environment note: the VM was replaced mid-run (apt deps wiped and
restored) and the shared target dir's wxWidgets tree had to be
rebuilt under cross-lane contention, so the test gate took hours of
wall time; the result above is the full shared-dir run, exit 0.

## Unverified

- Everything device-facing is compile- and code-verified only: there
  is no Chromecast on this network, so discovery against a real
  advertiser, the TLS connect, app launch, media load, status
  confirmation, upkeep pongs, and stop were never exercised against
  hardware. Brandon's on-device pass (and his NVDA pass over the cast
  window and the new prompt) remains the acceptance step.
- The crate's TCP connect has no timeout hook, so a discovered device
  that vanishes before connect can block the cast worker thread for
  the OS connect timeout; the UI stays responsive (all protocol work
  is on the worker), and later commands queue behind it until the
  connect resolves.
- Python's probe/TS-remux relay ladder for Chromecast is not ported:
  loads go through the existing cast proxy URL with the guessed
  content type, the same footing as the UPnP and Roku paths. A stream
  the Default Media Receiver cannot play fails with the "did not
  start playing" error rather than being transcoded.
- Windows firewall behavior is code-verified only (this host is
  Linux); the netsh argument list is copied from the Python oracle.
