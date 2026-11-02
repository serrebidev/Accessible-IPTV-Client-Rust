# 0047 — Large playlists and guides silently failed (ureq 10 MiB cap)

Date: 2026-11-02.

The owner tried to add his Dispatcharr Xtream playlist
(get.php?type=m3u_plus) and its XMLTV. The playlist would
not add. Server-side checks (same day, from this machine):
the playlist URL returns HTTP 200, 27,792,068 bytes,
115,651 channels; the XMLTV returns HTTP 200,
263,438,064 bytes, 813,298 programmes. Cross-check: the
EPG carries exactly 115,651 channels and every playlist
tvg-id (100%) is present as an EPG channel; 99.0% have
programmes. The two URLs are a matched pair.

Root cause: ureq 3.5.0's Body::read_to_string and
read_to_vec stop at a built-in 10 MiB cap
(MAX_BODY_SIZE = 10 * 1024 * 1024 in ureq's body module).
The playlist fetch (sources fetch_http) used read_to_string
and mapped the cap error to LoadFailure::Unreadable, so any
playlist over 10 MiB failed to load with no useful message.
The EPG fetch used read_to_vec — a 263 MB guide never stood
a chance. Python imposes no such cap on either.

Fix: one shared helper, sources::read_body_unbounded,
streams the body through ureq's unbounded reader. Applied
at every user-content fetch that can legitimately be large:
playlist fetch (sources), EPG fetch (epg), Xtream text
downloads (providers http_get_text — the same portal M3U),
and Stalker portal JSON reads (providers). Left alone on
purpose: the updater's GitHub API read (small JSON; asset
downloads already stream via as_reader), cast device
responses (small control XML), and the live-buffer HLS
resolver (its own 256 KiB cap is Python law). Playlist text
now decodes lossy, matching Python and the providers'
existing decode_text.

Regression tests (loopback HTTP, bodies just over 11 MiB):
sources::tests::http_playlist_over_ureq_default_cap_loads
and epg::tests::http_guide_over_ureq_default_cap_loads —
both failed against the old calls and pass now.

Evidence class: automated — fmt clean, clippy zero
warnings (-D warnings), workspace tests pass, playlist
parity PASS. The owner's re-add of the playlist and guide
on Windows is the closing check; ships as v0.3.2.
