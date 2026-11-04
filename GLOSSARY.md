# Accessible IPTV Client Rust

Shared language for the Rust port of Accessible-IPTV-Client and its relationship to the existing wxPython product.

## Migration

**Python reference**:
The pinned wxPython implementation and its observable behavior used as the compatibility oracle for the Rust port.
_Avoid_: legacy app, old app

**Rust port**:
The Rust implementation being developed to replace the Python reference after the replacement gate is met.
_Avoid_: rewrite, new app

**Vertical slice**:
A user-visible Rust capability that crosses the required domain, interface, test, and accessibility boundaries rather than stopping at an isolated library layer.
_Avoid_: demo, spike

**Replacement gate**:
The evidence required before Rust can replace Python as the shipping product: feature parity, regression coverage, build and release checks, and owner-heard accessibility acceptance.
_Avoid_: launch date, migration complete

**Replacement readiness**:
Candidate approval for Rust after current owner-heard accessibility, installed-libVLC playback, live ffmpeg recording, and packaging/updater VM evidence pass. Physical-renderer evidence remains a separately reported risk; a maintainer may record an explicit dated waiver with owner notification. Readiness does not switch the shipping product or authorize a release.
_Avoid_: replacement complete, Python retired

**Owner-heard validation**:
Hands-on validation by the blind product owner using NVDA and keyboard interaction; automated accessibility evidence does not substitute for it.
_Avoid_: accessibility test, screen-reader automation

## Playlist browsing

**Playlist**:
An ordered collection of channels loaded from an M3U or M3U Plus source.
_Avoid_: channel list, provider list

**Channel**:
A browsable playlist entry with a display identity, optional metadata, and a stream location.
_Avoid_: station, item

**M3U Plus metadata**:
The optional channel attributes carried alongside an M3U entry, including identity, display name, logo, and group information.
_Avoid_: tags, playlist fields

**Category**:
A named subset of the playlist a user browses: the All Channels and Favorites sentinels, or one M3U group. Channels with no group belong to Uncategorized.
_Avoid_: folder, filter

**Favorite**:
A channel the user marked for the Favorites category, identified by a stable key built from provider identity fields or the display name — never from a stream URL.
_Avoid_: bookmark, starred item

## Video on demand

**VOD catalogue**:
A browsable collection of movies and series available to play; a series contains its episodes.
_Avoid_: VOD playlist

**VOD group**:
A named set of VOD items presented together in the catalogue.
_Avoid_: VOD category
