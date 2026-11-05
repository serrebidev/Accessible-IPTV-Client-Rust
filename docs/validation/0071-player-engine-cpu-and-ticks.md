# 0071: Playback CPU, libVLC startup, and player ticks that never ran

Brandon, 2026-11-04: high CPU while playing, and "make sure VLC is the lowest latency large file format support for rust we can use". His settings have Show Player on Enter off.

Reproducing this exposed worse faults than the CPU report. The player window's timers died when they were created, so every shipped build ran without stall recovery. Fixing that exposed two more defects that had never run: a stack overrun and a wx assertion. All six are fixed on branch `fix/hidden-player-no-video`.

## What was wrong

- **Hidden play decoded video.** Python passes `video_visible=show_internal_player`, and a hidden play adds `:no-video`, `:vout=dummy`, `:intf=dummy` and detaches the window (`internal_player.py:830`). Reconnects keep that choice (`internal_player.py:753`). Rust always decoded and rendered into the hidden window.
- **libVLC started on the UI thread.** `VlcEngine::new` ran inside `play_builtin`. On a fresh install or update the first start takes about 16 s, and the window could not respond for that time.
- **The player timers were destroyed at creation.** `wire_player_events` held its 1 s and 150 ms timers in local `Rc` values. Dropping a wxDragon timer destroys it, so neither ever fired. Everything on those ticks was dead in the running app: VLC state polling, the freeze watchdog, reconnect, TS rollover, resume seek and its checkpoints, track restore, subtitle sampling and speech, the sleep timer, and the time label. Core unit tests cover that logic, so every gate stayed green.
- **`MediaStats` was two fields short.** It lacked `i_demux_corrupted` and `i_demux_discontinuity`, so libVLC wrote 8 bytes past the struct on every call. With the timers alive, this crashed the app with `0xC0000005` seconds after play began. The watchdog would also have read the wrong counters.
- **`AudioOutputDevice` had the wrong field order, and its setter the wrong signature.** The header puts `p_next` first. `libvlc_audio_output_device_set` takes `(player, module, device_id)` and returns nothing, but Rust declared `(player, device_id) -> int`.
- **Two Subtitles menu items shared id 5400.** The "No subtitle tracks available" placeholder stayed in the menu while "Off" was added with the same id. Checking that id hit the plain item and raised a wx assertion. While it was up, the tick holding the player state stayed blocked and every later tick panicked with `RefCell already borrowed`.

Python's instance flags (`--quiet`, `--no-video-title-show`, `--intf=dummy`) were also missing and are now passed, with Python's fallback to defaults if libVLC rejects them.

## What changed

- `crates/iptv-vlc/src/lib.rs`: `MediaOptions.video_visible`, instance flags, corrected `MediaStats`, `AudioOutputDevice` and setter signature. `set_window_handle(0)` now detaches the window.
- `crates/iptv-app/src/main.rs`: `PlayerState.video_visible` is set per play and kept across reconnects. libVLC starts on its own thread at startup when the built-in player is selected. A play made before it settles waits in `pending_play`, runs from the 150 ms tick, and is replaced or cleared by a later play or Stop. `PlayerState.player_timers` keeps the timers alive and they stop at exit. The subtitle placeholder is removed before the radios are added.
- `tools/smoke-player-recovery.py`: a repeatable real-app check on a private desktop, because unit tests could not see any of this.

## Measurements

Test machine: i7-9700K (8 cores), GTX 1660 Ti, D3D11VA active, libVLC 3.0.24 from the v0.4.12 zip unless noted. Percentages are of one core. Streams are synthetic, not provider streams. The H.264 and HEVC samples are 1080p, about 6-8 Mbit/s.

libVLC alone, into a hidden window (first group used the system libVLC 3.0.23):

- 1080p50 H.264, video on: 7.8%. With `:no-video`: 1.0%. Hardware decoding off: 26.9%. DXVA2 forced: 39.5%.
- 1080i25 H.264, video on: 23.6%. With `:no-video`: 0.9%.
- 1080i MPEG-2, video on: 2.6%. With `:no-video`: 0.0%.
- HEVC, video on: 5.5%. With `:no-video`: 0.5%.

Audio flows under `:no-video`: decoded audio written to a WAV by libVLC's file output grew the same with and without video, and the position advanced.

Real app, 115,000-channel playlist, private desktop, 8 Mbit/s interlaced H.264 over loopback:

- v0.4.12, hidden: 11.9%. First Enter froze for 15.49 s on a fresh copy.
- Fixed, hidden: 1.1% before the timers were fixed. 1.4% and 1.9% with all ticks running.
- Fixed, visible: 12.7% over a continuous 80 s run. Hidden and visible are now different costs by design.
- With a copy of the owner's real 950 MB guide, hidden, fixed core logic: about 5%. Almost all of it on one worker thread, not the UI thread (UI 0.8%), before the timer fix. That thread was not identified. It is not playback cost and is recorded as open below.

libVLC start, fresh process each sample: 160-200 ms warm, with or without `plugins.dat`. 16.6 s on the first run after extracting the zip, 164 ms on the second. This is antivirus scanning new DLLs, not the plugin cache, so no packaging change is needed.

Recovery, real app:

- v0.4.12, server cuts the stream at 6 s: 1 open in 45 s. It never reconnected.
- Fixed: 4 opens in 45 s, hidden and visible, with Python's backoff.
- Fixed, continuous 120 s stream: 1 open in 80 s, hidden and visible. No false restart.
- Enter pressed while libVLC is still starting: the stream opens 14 s later, the window stays responsive throughout.
- No panics or assertions in any of the final runs. Process alive at the end of each.

## VLC verdict

Keep libVLC 3, loaded directly with `libloading`. It is the current stable line: 3.0.24 is the newest version on download.videolan.org, the release workflow bundled it, and the Windows and macOS packages carry its full plugin set, about 360 on Windows. Calling the C API directly leaves no wrapper between the app and libVLC, and Python's engine is the behavior reference. No other engine was measured, so this is not a benchmark win over libmpv. A swap would need its own decision and evidence.

For latency, the setting that matters is the cache. Live channels open on Python's 0.5 s cache. With that cache, first decoded audio over loopback took about 250 ms with video. The same measure was not reliable for `:no-video` (it read 16 ms, which is too low to trust), so no latency improvement is claimed for hidden play.

A reconnect uses the saved buffer, not the quick-start value, the same as Python (`internal_player.py:814`, `:1425`). At the 25 s default, a real stream drop can mean up to 25 s of silence before sound returns. The saved value is the owner's choice and is unchanged.

## Risks to review

- **Now running for the first time:** the watchdog, reconnect, rollover, resume checkpoints, sleep timer, and subtitle speech run in the real app for the first time. They were tested only as core logic, so expect behavior that has never been heard.
- **Watchdog on discontinuities:** a stream whose timestamps jump backward can stop video progress for 10 s and earn a restart. A looped test file did this once at 43 s. Real provider discontinuities were not tested.
- **VLC version in releases:** `release.yml` picked the newest numeric VLC folder, so a 4.x folder would have bundled a libVLC with a changed API. On Brandon's OK (2026-11-04), the Windows and macOS jobs now take the newest 3.x. The next release run is the first real test of that step.
- **Linux plugins:** the Linux job installs only `vlc-plugin-base`, which may limit video output there. Not changed.

## Gates

`cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` (iptv-app 18, iptv-core 283 plus integration, iptv-vlc 7), and `git diff --check` all passed.

Not run: `tools/check-playlist-parity.py`, which needs the pinned Python clone. No playlist code changed.

Not done: real provider streams, Linux and macOS, and owner-heard NVDA acceptance. Brandon's hands-on test remains the acceptance test.
