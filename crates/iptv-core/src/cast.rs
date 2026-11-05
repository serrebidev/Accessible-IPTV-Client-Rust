//! Casting protocol cores, ported from the Python Caster stack
//! (`caster_engine.py`, `caster_devices.py`, `caster_extras.py`): media
//! helpers, SSDP discovery, UPnP/DLNA control, Roku ECP, Kodi JSON-RPC,
//! the Sonos DIDL builder, and the stream proxy's header normalization.

use std::collections::BTreeMap;
use std::net::UdpSocket;
use std::time::{Duration, Instant};

use rust_cast::channels::media::{GenericMediaMetadata, Media, Metadata, StreamType};

// ---------------------------------------------------------------------------
// Media helpers (caster_engine.py)
// ---------------------------------------------------------------------------

/// The URL with query and fragment removed.
#[must_use]
pub fn url_path(url: &str) -> &str {
    url.split('?')
        .next()
        .unwrap_or(url)
        .split('#')
        .next()
        .unwrap_or(url)
}

fn mime_by_extension(path: &str) -> Option<&'static str> {
    let lower = path.to_lowercase();
    let ext = lower.rsplit('.').next()?;
    if ext == lower {
        return None;
    }
    Some(match ext {
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "ts" | "m2ts" => "video/mp2t",
        "mpeg" | "mpg" => "video/mpeg",
        "mkv" => "video/x-matroska",
        "mov" => "video/quicktime",
        "avi" => "video/x-msvideo",
        "mp3" => "audio/mpeg",
        "aac" => "audio/aac",
        "m4a" => "audio/x-m4a",
        "ogg" => "audio/ogg",
        "flac" => "audio/flac",
        "wav" => "audio/x-wav",
        "m3u8" | "m3u" => "application/vnd.apple.mpegurl",
        _ => return None,
    })
}

/// Port of `guess_mime`: by extension, then the AAC hint, then the
/// radio-stream default.
#[must_use]
pub fn guess_mime(url: &str) -> String {
    if let Some(mime) = mime_by_extension(url_path(url)) {
        return mime.to_string();
    }
    if url.to_lowercase().contains("aac") {
        return "audio/aac".to_string();
    }
    "audio/mpeg".to_string()
}

/// The media type to tell Roku Media Player (port of `roku_mime`).
#[allow(clippy::case_sensitive_file_extension_comparisons)] // path is lowercased first
#[must_use]
pub fn roku_mime(url: &str) -> String {
    let path = url_path(url).to_lowercase();
    if path.ends_with(".m3u8") || path.ends_with(".m3u") {
        return "application/vnd.apple.mpegurl".to_string();
    }
    if let Some(mime) = mime_by_extension(&path) {
        return roku_mime_for_media(mime);
    }
    "video/mp4".to_string()
}

/// Roku's label for a known media type: HLS stays HLS, WAV takes
/// Roku's spelling, other audio passes through, and everything
/// else is presented as MP4 (the non-audio tail of `roku_mime`).
#[must_use]
pub fn roku_mime_for_media(mime: &str) -> String {
    if mime == "application/vnd.apple.mpegurl" {
        return mime.to_string();
    }
    if mime == "audio/x-wav" {
        return "audio/wav".to_string();
    }
    if mime.starts_with("audio/") {
        return mime.to_string();
    }
    "video/mp4".to_string()
}

/// The MIME a cast actually uses: the media type probed from the
/// original stream, carried independently of the playback URL
/// (port of `casting.py`'s probe-then-play split). The playback URL
/// may be an opaque proxy path (`/s/<hash>`) that says nothing
/// about the format; only without a probed type do we guess from
/// the URL itself.
#[must_use]
pub fn effective_mime(probed: Option<&str>, url: &str) -> String {
    match probed {
        Some(mime) if !mime.is_empty() => mime.to_string(),
        _ => guess_mime(url),
    }
}

/// A video id from a `YouTube` URL, when present (port of `youtube_id`):
/// after `v=`, `youtu.be/`, `/shorts/` or `/embed/`, 6+ id characters.
#[must_use]
pub fn youtube_id(url: &str) -> Option<String> {
    for marker in ["v=", "youtu.be/", "/shorts/", "/embed/"] {
        if let Some(at) = url.find(marker) {
            let rest = &url[at + marker.len()..];
            let id: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .collect();
            if id.len() >= 6 {
                return Some(id);
            }
        }
    }
    None
}

/// Whether these bytes are really MPEG-TS (port of
/// `_looks_like_mpegts`): the sync byte repeating at the packet stride,
/// searched at any offset for mid-packet starts, four hits required.
#[must_use]
pub fn looks_like_mpegts(head: &[u8]) -> bool {
    if head.first() == Some(&0x47) {
        if head.len() >= 189 && head[188] == 0x47 {
            return true;
        }
        if head.len() >= 193 && head[192] == 0x47 {
            return true;
        }
    }
    for stride in [188_usize, 192] {
        for start in 1..stride {
            if start + 3 * stride >= head.len() {
                break;
            }
            if (0..4).all(|n| head[start + n * stride] == 0x47) {
                return true;
            }
        }
    }
    false
}

/// Escape text for XML content (`&`, `<`, `>`).
#[must_use]
pub fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

// ---------------------------------------------------------------------------
// SSDP (caster_devices.py / caster_extras.py)
// ---------------------------------------------------------------------------

/// M-SEARCH for one search target; returns the LOCATION of each
/// responder, deduplicated (port of `_ssdp_search`): the search is
/// repeated up to three times across the window because SSDP is lossy.
#[must_use]
pub fn ssdp_search(search_target: &str, timeout_seconds: u64) -> Vec<String> {
    let msg = format!(
        "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: {}\r\nST: {search_target}\r\n\r\n",
        timeout_seconds.saturating_sub(1).max(1)
    );
    let Ok(socket) = UdpSocket::bind("0.0.0.0:0") else {
        return Vec::new();
    };
    let _ = socket.set_read_timeout(Some(Duration::from_millis(400)));
    let deadline = Instant::now() + Duration::from_secs(timeout_seconds);
    let mut locations: Vec<String> = Vec::new();
    let mut searches = 0;
    let mut next_search = Instant::now();
    let mut buffer = vec![0_u8; 65536];
    while Instant::now() < deadline {
        if Instant::now() >= next_search && searches < 3 {
            let _ = socket.send_to(msg.as_bytes(), "239.255.255.250:1900");
            searches += 1;
            next_search = Instant::now() + Duration::from_secs(timeout_seconds) / 4;
        }
        if let Ok(n) = socket.recv(&mut buffer) {
            {
                let text = String::from_utf8_lossy(&buffer[..n]);
                for line in text.lines() {
                    if line.to_lowercase().starts_with("location:") {
                        let location = line.split_once(':').map(|(_, v)| v.trim().to_string());
                        if let Some(location) = location {
                            if !location.is_empty() && !locations.contains(&location) {
                                locations.push(location);
                            }
                        }
                        break;
                    }
                }
            }
        }
    }
    locations
}

// ---------------------------------------------------------------------------
// UPnP / DLNA (caster_extras.py)
// ---------------------------------------------------------------------------

/// One discovered media renderer.
#[derive(Clone, Debug)]
pub struct CastDevice {
    pub name: String,
    pub control_url: String,
    pub manufacturer: String,
    pub sink_mimes: Vec<String>,
    pub kind: CastKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CastKind {
    Upnp,
    Roku,
    Chromecast,
}

fn tag_text(body: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(unescape_basic(body[start..end].trim()))
}

fn unescape_basic(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Minimal URL resolution for control URLs (absolute, root-relative,
/// or relative to the description URL's directory).
#[must_use]
pub fn url_join(base: &str, relative: &str) -> String {
    if relative.starts_with("http://") || relative.starts_with("https://") {
        return relative.to_string();
    }
    let Some((scheme, rest)) = base.split_once("://") else {
        return relative.to_string();
    };
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    if let Some(stripped) = relative.strip_prefix('/') {
        return format!("{scheme}://{authority}/{stripped}");
    }
    let path = &rest[authority_end..];
    let dir = path.rfind('/').map_or("/", |i| &path[..=i]);
    format!("{scheme}://{authority}{dir}{relative}")
}

/// Parse a device description into (name, `AVTransport` control URL,
/// `ConnectionManager` control URL, manufacturer), per
/// `_upnp_fetch_control`.
#[must_use]
pub fn parse_device_description(
    location: &str,
    body: &str,
) -> Option<(String, String, String, String)> {
    let name = tag_text(body, "friendlyName").unwrap_or_else(|| location.to_string());
    let maker = tag_text(body, "manufacturer").unwrap_or_default();
    let mut av_url = String::new();
    let mut cm_url = String::new();
    for service in body.split("<service>").skip(1) {
        let Some(end) = service.find("</service>") else {
            continue;
        };
        let service = &service[..end];
        let Some(control) = tag_text(service, "controlURL") else {
            continue;
        };
        let url = url_join(location, &control);
        if service.contains("AVTransport") && av_url.is_empty() {
            av_url = url;
        } else if service.contains("ConnectionManager") && cm_url.is_empty() {
            cm_url = url;
        }
    }
    if av_url.is_empty() {
        return None;
    }
    Some((name, av_url, cm_url, maker))
}

fn soap_envelope(inner: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body>{inner}</s:Body></s:Envelope>"
    )
}

fn soap_post(
    control_url: &str,
    service: &str,
    action: &str,
    inner: &str,
    timeout: Duration,
) -> Result<String, String> {
    let body = soap_envelope(inner);
    let _ = timeout; // per-call timeouts ride ureq defaults in this port
    let mut response = ureq::post(control_url)
        .header("Content-Type", "text/xml; charset=\"utf-8\"")
        .header(
            "SOAPACTION",
            &format!("\"urn:schemas-upnp-org:service:{service}:1#{action}\""),
        )
        .send(body.as_str())
        .map_err(|e| format!("Cast request failed: {e}"))?;
    response
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("Cast request failed: {e}"))
}

/// The renderer's accepted content types from `GetProtocolInfo` (port of
/// `upnp_sink_mimes`); empty means "did not say", not "nothing".
#[must_use]
pub fn upnp_sink_mimes(connection_manager_url: &str) -> Vec<String> {
    if connection_manager_url.is_empty() {
        return Vec::new();
    }
    let args = "<u:GetProtocolInfo xmlns:u=\"urn:schemas-upnp-org:service:ConnectionManager:1\"/>";
    let Ok(xml) = soap_post(
        connection_manager_url,
        "ConnectionManager",
        "GetProtocolInfo",
        args,
        Duration::from_secs(5),
    ) else {
        return Vec::new();
    };
    let Some(sink) = tag_text(&xml, "Sink") else {
        return Vec::new();
    };
    let mut mimes: Vec<String> = Vec::new();
    for entry in sink.split(',') {
        let parts: Vec<&str> = entry.trim().split(':').collect();
        if parts.len() >= 3 && parts[2].contains('/') {
            let mime = parts[2]
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_lowercase();
            if !mime.is_empty() && !mimes.contains(&mime) {
                mimes.push(mime);
            }
        }
    }
    mimes
}

/// Discover DLNA renderers: SSDP for `MediaRenderer`, then a description
/// fetch per location (port of `upnp_discover`).
#[must_use]
pub fn upnp_discover(timeout_seconds: u64) -> Vec<CastDevice> {
    let mut devices = Vec::new();
    for location in ssdp_search(
        "urn:schemas-upnp-org:device:MediaRenderer:1",
        timeout_seconds,
    ) {
        let Ok(mut response) = ureq::get(&location).call() else {
            continue;
        };
        let Ok(body) = response.body_mut().read_to_string() else {
            continue;
        };
        if let Some((name, av_url, cm_url, maker)) = parse_device_description(&location, &body) {
            let sink = upnp_sink_mimes(&cm_url);
            devices.push(CastDevice {
                name,
                control_url: av_url,
                manufacturer: maker,
                sink_mimes: sink,
                kind: CastKind::Upnp,
            });
        }
    }
    devices
}

/// The DIDL-Lite metadata for a cast (the `SetAVTransportURI` payload).
#[must_use]
pub fn upnp_didl(media_url: &str, title: &str, mime: &str, upnp_class: &str) -> String {
    let proto = if mime.is_empty() { "*" } else { mime };
    format!(
        "<DIDL-Lite xmlns=\"urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:upnp=\"urn:schemas-upnp-org:metadata-1-0/upnp/\"><item id=\"\" parentID=\"0\" restricted=\"1\"><dc:title>{}</dc:title><upnp:class>{upnp_class}</upnp:class><res protocolInfo=\"http-get:*:{proto}:DLNA.ORG_OP=01;DLNA.ORG_CI=0\">{}</res></item></DIDL-Lite>",
        xml_escape(title),
        xml_escape(media_url)
    )
}

/// Send `SetAVTransportURI` + Play (port of `upnp_play`).
///
/// # Errors
///
/// Returns the transport failure text.
/// Whether a device is a Sonos speaker (its DIDL differs: an
/// audio item class, no video).
#[must_use]
pub fn is_sonos(device: &CastDevice) -> bool {
    device.manufacturer.to_ascii_lowercase().contains("sonos")
        || device.name.to_ascii_lowercase().contains("sonos")
}

/// Send a stream to a `UPnP` `AVTransport` renderer.
///
/// # Errors
/// Returns the SOAP failure text when the device refuses.
pub fn upnp_play(
    control_url: &str,
    media_url: &str,
    title: &str,
    mime: &str,
) -> Result<(), String> {
    let didl = upnp_didl(media_url, title, mime, "object.item.videoItem");
    upnp_play_with_didl(control_url, media_url, &didl)
}

/// `SetAVTransportURI` + Play with a caller-built DIDL document
/// (Sonos uses its own shape).
///
/// # Errors
/// Returns the SOAP failure text when the device refuses.
pub fn upnp_play_with_didl(control_url: &str, media_url: &str, didl: &str) -> Result<(), String> {
    let escaped = xml_escape(media_url);
    let didl_escaped = xml_escape(didl);
    let uri_args = format!(
        "<u:SetAVTransportURI xmlns:u=\"urn:schemas-upnp-org:service:AVTransport:1\"><InstanceID>0</InstanceID><CurrentURI>{escaped}</CurrentURI><CurrentURIMetaData>{didl_escaped}</CurrentURIMetaData></u:SetAVTransportURI>"
    );
    soap_post(
        control_url,
        "AVTransport",
        "SetAVTransportURI",
        &uri_args,
        Duration::from_secs(10),
    )?;
    let play_args = "<u:Play xmlns:u=\"urn:schemas-upnp-org:service:AVTransport:1\"><InstanceID>0</InstanceID><Speed>1</Speed></u:Play>";
    soap_post(
        control_url,
        "AVTransport",
        "Play",
        play_args,
        Duration::from_secs(10),
    )?;
    Ok(())
}

/// Stop a renderer (port of `upnp_stop`; failures are ignored there too).
pub fn upnp_stop(control_url: &str) {
    let stop_args = "<u:Stop xmlns:u=\"urn:schemas-upnp-org:service:AVTransport:1\"><InstanceID>0</InstanceID></u:Stop>";
    let _ = soap_post(
        control_url,
        "AVTransport",
        "Stop",
        stop_args,
        Duration::from_secs(10),
    );
}

/// The renderer's `TransportState` (port of `upnp_state`).
///
/// # Errors
///
/// Returns the transport failure text.
pub fn upnp_state(control_url: &str) -> Result<String, String> {
    let args = "<u:GetTransportInfo xmlns:u=\"urn:schemas-upnp-org:service:AVTransport:1\"><InstanceID>0</InstanceID></u:GetTransportInfo>";
    let xml = soap_post(
        control_url,
        "AVTransport",
        "GetTransportInfo",
        args,
        Duration::from_secs(10),
    )?;
    Ok(tag_text(&xml, "CurrentTransportState").unwrap_or_else(|| "UNKNOWN".to_string()))
}

/// Set a renderer's volume through `RenderingControl` (port of
/// `upnp_set_volume`): the `AVTransport` control URL with the service
/// name swapped, as the reference does.
///
/// # Errors
///
/// Returns the transport failure text.
pub fn upnp_set_volume(control_url: &str, level: i64) -> Result<(), String> {
    let rendering_url = control_url.replace("AVTransport", "RenderingControl");
    let level = level.clamp(0, 100);
    let args = format!(
        "<u:SetVolume xmlns:u=\"urn:schemas-upnp-org:service:RenderingControl:1\"><InstanceID>0</InstanceID><Channel>Master</Channel><DesiredVolume>{level}</DesiredVolume></u:SetVolume>"
    );
    soap_post(
        &rendering_url,
        "RenderingControl",
        "SetVolume",
        &args,
        Duration::from_secs(6),
    )?;
    Ok(())
}

/// The host part of a control URL (port of `upnp_host`).
#[must_use]
pub fn upnp_host(control_url: &str) -> String {
    let rest = control_url.split("://").nth(1).unwrap_or(control_url);
    rest.split(['/', ':']).next().unwrap_or("").to_string()
}

// ---------------------------------------------------------------------------
// Roku ECP (caster_devices.py)
// ---------------------------------------------------------------------------

pub const ROKU_MEDIA_PLAYER: &str = "2213";

fn form_encode(value: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char);
            }
            b' ' => out.push('+'),
            _ => {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

/// The ECP launch URL for a cast (port of `roku_play`'s target).
#[must_use]
pub fn roku_play_url(base: &str, url: &str, mime: &str, title: &str) -> String {
    let audio = mime.starts_with("audio/");
    let mut params: Vec<(String, String)> = vec![
        ("t".to_string(), if audio { "a" } else { "v" }.to_string()),
        ("u".to_string(), url.to_string()),
    ];
    if audio {
        let format = match mime {
            "audio/wav" => "wav",
            _ => "mp3",
        };
        params.push(("audioName".to_string(), title.to_string()));
        params.push(("audioFormat".to_string(), format.to_string()));
    } else {
        let format = match mime {
            "application/vnd.apple.mpegurl" => "hls",
            _ => "mp4",
        };
        params.push(("videoName".to_string(), title.to_string()));
        params.push(("videoFormat".to_string(), format.to_string()));
    }
    let query: Vec<String> = params
        .iter()
        .map(|(k, v)| format!("{k}={}", form_encode(v)))
        .collect();
    format!("{base}/launch/{ROKU_MEDIA_PLAYER}?{}", query.join("&"))
}

fn http_post_empty(url: &str, timeout: Duration) -> Result<(), String> {
    let _ = timeout; // per-call timeouts ride ureq defaults in this port
    ureq::post(url)
        .send_empty()
        .map_err(|e| format!("Cast request failed: {e}"))?;
    Ok(())
}

/// Play on a Roku (port of `roku_play`).
///
/// # Errors
///
/// Returns the transport failure text.
pub fn roku_play(base: &str, url: &str, mime: &str, title: &str) -> Result<(), String> {
    http_post_empty(
        &roku_play_url(base, url, mime, title),
        Duration::from_secs(6),
    )
}

/// Leave the Roku media player (ECP has no stop; Home is the exit).
pub fn roku_stop(base: &str) {
    let _ = http_post_empty(&format!("{base}/keypress/Home"), Duration::from_secs(6));
}

/// Send one ECP keypress.
///
/// # Errors
///
/// Returns the transport failure text.
pub fn roku_key(base: &str, key: &str) -> Result<(), String> {
    http_post_empty(&format!("{base}/keypress/{key}"), Duration::from_secs(6))
}

/// Discover Rokus via SSDP (port of `roku_discover`): the base URL is
/// the description URL's origin; the name comes from its description.
#[must_use]
pub fn roku_discover(timeout_seconds: u64) -> Vec<CastDevice> {
    let mut devices = Vec::new();
    for location in ssdp_search("roku:ecp", timeout_seconds) {
        let base = match location.split("://").collect::<Vec<_>>().as_slice() {
            [scheme, rest] => {
                let authority = rest.split('/').next().unwrap_or(rest);
                format!("{scheme}://{authority}")
            }
            _ => continue,
        };
        let name = ureq::get(&location)
            .call()
            .ok()
            .and_then(|mut r| r.body_mut().read_to_string().ok())
            .and_then(|body| tag_text(&body, "friendlyName"))
            .unwrap_or_else(|| base.clone());
        devices.push(CastDevice {
            name,
            control_url: base,
            manufacturer: "Roku".to_string(),
            sink_mimes: Vec::new(),
            kind: CastKind::Roku,
        });
    }
    devices
}

// ---------------------------------------------------------------------------
// Kodi JSON-RPC and Sonos DIDL (caster_devices.py)
// ---------------------------------------------------------------------------

const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(data: &[u8]) -> String {
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            B64[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// One Kodi JSON-RPC call (port of `_kodi_rpc`): an in-band error is a
/// failure, not a result.
///
/// # Errors
///
/// Returns the transport failure or Kodi's refusal text.
pub fn kodi_rpc(
    base: &str,
    method: &str,
    params: &serde_json::Value,
    auth: (&str, &str),
) -> Result<serde_json::Value, String> {
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    });
    let mut request =
        ureq::post(&format!("{base}/jsonrpc")).header("Content-Type", "application/json");
    if !auth.0.is_empty() {
        let token = base64_encode(format!("{}:{}", auth.0, auth.1).as_bytes());
        request = request.header("Authorization", &format!("Basic {token}"));
    }
    let mut response = request
        .send(payload.to_string().as_str())
        .map_err(|e| format!("Kodi request failed: {e}"))?;
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("Kodi request failed: {e}"))?;
    let reply: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("Kodi request failed: {e}"))?;
    if let Some(error) = reply.get("error") {
        if !error.is_null() {
            let detail = error
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            return Err(format!(
                "Kodi refused {method}: {}",
                if detail.is_empty() {
                    error.to_string()
                } else {
                    detail.to_string()
                }
            ));
        }
    }
    Ok(reply)
}

/// Play a URL on Kodi (port of `kodi_play`).
///
/// # Errors
///
/// Returns the RPC failure text.
pub fn kodi_play(base: &str, url: &str, auth: (&str, &str)) -> Result<(), String> {
    kodi_rpc(
        base,
        "Player.Open",
        &serde_json::json!({"item": {"file": url}}),
        auth,
    )?;
    Ok(())
}

/// DIDL-Lite for a Sonos live stream (port of `sonos_didl`): tagged as
/// a plain music track on purpose (see the reference's note).
#[must_use]
pub fn sonos_didl(title: &str, url: &str, mime: &str) -> String {
    format!(
        "<DIDL-Lite xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:upnp=\"urn:schemas-upnp-org:metadata-1-0/upnp/\" xmlns=\"urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/\"><item id=\"1\" parentID=\"0\" restricted=\"1\"><dc:title>{}</dc:title><upnp:class>object.item.audioItem.musicTrack</upnp:class><res protocolInfo=\"http-get:*:{}:*\">{}</res></item></DIDL-Lite>",
        xml_escape(title),
        xml_escape(mime),
        xml_escape(url)
    )
}

// ---------------------------------------------------------------------------
// Stream proxy header normalization (stream_proxy.py)
// ---------------------------------------------------------------------------

const HEADER_NAME_MAP: &[(&str, &str)] = &[
    ("user-agent", "User-Agent"),
    ("ua", "User-Agent"),
    ("http-user-agent", "User-Agent"),
    ("referer", "Referer"),
    ("referrer", "Referer"),
    ("http-referrer", "Referer"),
    ("http-referer", "Referer"),
    ("origin", "Origin"),
    ("http-origin", "Origin"),
    ("cookie", "Cookie"),
    ("http-cookie", "Cookie"),
    ("authorization", "Authorization"),
    ("auth", "Authorization"),
    ("http-authorization", "Authorization"),
    ("accept", "Accept"),
    ("http-accept", "Accept"),
    ("range", "Range"),
    ("http-range", "Range"),
    ("host", "Host"),
    ("http-host", "Host"),
    ("x-forwarded-for", "X-Forwarded-For"),
    ("xff", "X-Forwarded-For"),
];

/// Canonicalize a header name (port of `_normalize_header_name`): the
/// alias map, else Title-Case per dash segment.
#[must_use]
pub fn normalize_header_name(name: &str) -> String {
    let key = name.trim().to_lowercase();
    if let Some((_, mapped)) = HEADER_NAME_MAP.iter().find(|(k, _)| *k == key) {
        return (*mapped).to_string();
    }
    name.trim()
        .split('-')
        .filter(|p| !p.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => {
                    first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase()
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join("-")
}

/// Normalize a headers map into ordered pairs (port of
/// `normalize_request_headers`): names canonicalized, extra `Name:
/// value` lines folded in without overwriting, and the default
/// upstream User-Agent added when missing and requested.
#[must_use]
pub fn normalize_request_headers(
    entries: &[(String, String)],
    extra_lines: &[String],
    add_default_user_agent: bool,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let set = |out: &mut Vec<(String, String)>, name: String, value: &str, overwrite: bool| {
        let value = value.trim();
        if name.is_empty() || value.is_empty() {
            return;
        }
        let lower = name.to_lowercase();
        if let Some(existing) = out.iter_mut().find(|(k, _)| k.to_lowercase() == lower) {
            if overwrite {
                existing.1 = value.to_string();
            }
            return;
        }
        out.push((name, value.to_string()));
    };
    for (name, value) in entries {
        if name.trim().to_lowercase() == "_extra" {
            continue;
        }
        let normalized = normalize_header_name(name);
        set(&mut out, normalized, value, true);
    }
    for line in extra_lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let normalized = normalize_header_name(name);
        set(&mut out, normalized, value, false);
    }
    if add_default_user_agent && !out.iter().any(|(k, _)| k.to_lowercase() == "user-agent") {
        out.push((
            "User-Agent".to_string(),
            crate::recording::DEFAULT_UPSTREAM_USER_AGENT.to_string(),
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// Chromecast (casting.py's `_scan_chromecast` / `_play_chromecast`):
// mDNS discovery of `_googlecast._tcp`, Cast V2 control through the
// `chromecast` crate (lib name `rust_cast`). The crate has no
// discovery module, so discovery is ours (mdns-sd).
// ---------------------------------------------------------------------------

/// The mDNS service type Chromecast devices advertise.
pub const GOOGLECAST_SERVICE: &str = "_googlecast._tcp.local.";

/// The port a Cast device listens on when the advertisement
/// does not say (Python's `info.port or 8009`).
pub const DEFAULT_CAST_PORT: u16 = 8009;

/// The host to dial for a resolved Cast service (port of
/// `mdns_host`): IPv4 first — the app's media server is IPv4 —
/// with a deterministic pick so repeat scans agree.
#[must_use]
pub fn pick_cast_host(addresses: &[String]) -> Option<String> {
    let mut v4: Vec<&String> = addresses
        .iter()
        .filter(|a| a.parse::<std::net::Ipv4Addr>().is_ok())
        .collect();
    v4.sort();
    if let Some(first) = v4.first() {
        return Some((*first).clone());
    }
    let mut all: Vec<&String> = addresses.iter().collect();
    all.sort();
    all.first().map(|first| (*first).clone())
}

/// A Cast service's display name: the TXT `fn` record, else
/// the service fullname's first label (Python's fallback).
#[must_use]
pub fn chromecast_display_name(fullname: &str, friendly: Option<&str>) -> String {
    match friendly {
        Some(name) if !name.trim().is_empty() => name.to_string(),
        _ => fullname.split('.').next().unwrap_or(fullname).to_string(),
    }
}

/// The device record for one resolved Cast service. The
/// control URL is the `host:port` endpoint the session dials.
#[must_use]
pub fn chromecast_device(name: String, host: &str, port: u16, model: &str) -> CastDevice {
    let endpoint = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    CastDevice {
        name,
        control_url: endpoint,
        manufacturer: if model.is_empty() {
            "Chromecast".to_string()
        } else {
            model.to_string()
        },
        sink_mimes: Vec::new(),
        kind: CastKind::Chromecast,
    }
}

/// Split a stored `host:port` endpoint back into its parts; a
/// bare host takes the default Cast port.
#[must_use]
pub fn parse_cast_endpoint(control_url: &str) -> Option<(String, u16)> {
    let text = control_url.trim();
    if text.is_empty() {
        return None;
    }
    if let Some(bracketed) = text.strip_prefix('[') {
        let (host, rest) = bracketed.split_once(']')?;
        let port = rest
            .strip_prefix(':')
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(DEFAULT_CAST_PORT);
        return Some((host.to_string(), port));
    }
    match text.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && port.parse::<u16>().is_ok() => Some((
            host.to_string(),
            port.parse::<u16>().unwrap_or(DEFAULT_CAST_PORT),
        )),
        _ => Some((text.to_string(), DEFAULT_CAST_PORT)),
    }
}

/// Discover Chromecast devices by browsing `_googlecast._tcp`
/// for the window (port of `_scan_chromecast`): each resolved
/// service becomes a device, keyed by endpoint, with a
/// same-name service disambiguated by host like the reference.
#[must_use]
pub fn chromecast_discover(timeout_seconds: u64) -> Vec<CastDevice> {
    let Ok(daemon) = mdns_sd::ServiceDaemon::new() else {
        return Vec::new();
    };
    let Ok(receiver) = daemon.browse(GOOGLECAST_SERVICE) else {
        return Vec::new();
    };
    let deadline = Instant::now() + Duration::from_secs(timeout_seconds);
    let mut devices: Vec<CastDevice> = Vec::new();
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        if remaining.is_zero() {
            break;
        }
        match receiver.recv_timeout(remaining) {
            Ok(mdns_sd::ServiceEvent::ServiceResolved(info)) => {
                let addresses: Vec<String> = info
                    .get_addresses()
                    .iter()
                    .map(ToString::to_string)
                    .collect();
                let Some(host) = pick_cast_host(&addresses) else {
                    continue;
                };
                let friendly = info.get_property_val_str("fn");
                let mut name = chromecast_display_name(info.get_fullname(), friendly);
                if devices.iter().any(|d| d.name == name) {
                    name = format!("{name} ({host})");
                }
                let model = info.get_property_val_str("md").unwrap_or("Chromecast");
                let device = chromecast_device(name, &host, info.get_port(), model);
                if !devices.iter().any(|d| d.control_url == device.control_url) {
                    devices.push(device);
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let _ = daemon.stop_browse(GOOGLECAST_SERVICE);
    let _ = daemon.shutdown();
    devices
}

/// A live Chromecast session, owned by the cast worker thread.
/// The `rust_cast` device is confined to that thread: its
/// message manager holds the stream lock across a blocking
/// read, so no second thread may ever read from it.
struct ChromecastSession {
    endpoint: String,
    device: rust_cast::CastDevice<'static>,
    transport_id: String,
    session_id: String,
    media_session_id: Option<i32>,
    last_upkeep: Instant,
}

/// Connect to a Cast endpoint and launch the Default Media
/// Receiver (the app Python's `CAST_APP_ID` names), returning
/// the ready session.
///
/// # Errors
///
/// Returns the transport failure text.
fn chromecast_connect(device: &CastDevice) -> Result<ChromecastSession, String> {
    let (host, port) = parse_cast_endpoint(&device.control_url)
        .ok_or_else(|| "Cast request failed: no address for this device".to_string())?;
    let conn = rust_cast::CastDevice::connect_without_host_verification(host, port)
        .map_err(|e| format!("Cast request failed: {e}"))?;
    conn.connection
        .connect("receiver-0")
        .map_err(|e| format!("Cast request failed: {e}"))?;
    let _ = conn.heartbeat.ping();
    let app = conn
        .receiver
        .launch_app(&rust_cast::channels::receiver::CastDeviceApp::DefaultMediaReceiver)
        .map_err(|e| format!("Cast request failed: {e}"))?;
    conn.connection
        .connect(app.transport_id.as_str())
        .map_err(|e| format!("Cast request failed: {e}"))?;
    Ok(ChromecastSession {
        endpoint: device.control_url.clone(),
        device: conn,
        transport_id: app.transport_id,
        session_id: app.session_id,
        media_session_id: None,
        last_upkeep: Instant::now(),
    })
}

/// Wait for the receiver to start playing (a bounded version
/// of Python's `_await_playing`): Playing settles it, an Idle
/// entry carrying an error/finish reason refuses it, and
/// Buffering counts as started once the wait runs out — a slow
/// live start is not a rejection.
///
/// # Errors
///
/// Returns Python's refusal sentence with the idle reason.
fn chromecast_await_playing(session: &ChromecastSession, name: &str) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut saw_buffering = false;
    loop {
        let status = session
            .device
            .media
            .get_status(session.transport_id.as_str(), None)
            .map_err(|e| format!("Cast request failed: {e}"))?;
        if let Some(entry) = status.entries.first() {
            match entry.player_state {
                rust_cast::channels::media::PlayerState::Playing => return Ok(()),
                rust_cast::channels::media::PlayerState::Idle => {
                    if let Some(idle) = &entry.idle_reason {
                        let reason = format!("{idle:?}").to_lowercase();
                        return Err(format!(
                            "{name} did not start playing the stream ({reason})."
                        ));
                    }
                }
                rust_cast::channels::media::PlayerState::Buffering
                | rust_cast::channels::media::PlayerState::Paused => {
                    saw_buffering = true;
                }
            }
        }
        if Instant::now() >= deadline {
            if saw_buffering {
                return Ok(());
            }
            return Err(format!(
                "{name} did not start playing the stream (no answer)."
            ));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Play on a Chromecast through the worker's session (port of
/// `_play_chromecast`'s load, minus the probe/relay ladder the
/// engine owns): connect or reuse the session, load with the
/// live stream type, flip the type once when the load is
/// refused (Python's final retry), and confirm playback.
///
/// # Errors
///
/// Returns the failure text; a transport failure also drops
/// the session so the next Play reconnects.
fn chromecast_play(
    session: &mut Option<ChromecastSession>,
    device: &CastDevice,
    url: &str,
    title: &str,
    mime: &str,
) -> Result<String, String> {
    if session
        .as_ref()
        .is_none_or(|live| live.endpoint != device.control_url)
    {
        *session = None;
        *session = Some(chromecast_connect(device)?);
    }
    let live = session.as_mut().expect("session just ensured");
    let mut stream_type = if mime == "application/vnd.apple.mpegurl" {
        StreamType::Live
    } else {
        StreamType::Buffered
    };
    let mut last_error = String::new();
    for _ in 0..2 {
        let media = Media {
            content_id: url.to_string(),
            stream_type,
            content_type: mime.to_string(),
            metadata: Some(Metadata::Generic(GenericMediaMetadata {
                title: Some(title.to_string()),
                subtitle: None,
                images: Vec::new(),
                release_date: None,
            })),
            duration: None,
        };
        let loaded =
            live.device
                .media
                .load(live.transport_id.as_str(), live.session_id.as_str(), &media);
        match loaded {
            Ok(status) => {
                live.media_session_id = status.entries.first().map(|entry| entry.media_session_id);
                match chromecast_await_playing(live, &device.name) {
                    Ok(()) => return Ok(device.name.clone()),
                    Err(error) => {
                        if error.starts_with("Cast request failed") {
                            *session = None;
                        }
                        return Err(error);
                    }
                }
            }
            Err(error) => {
                last_error = format!("Cast request failed: {error}");
                stream_type = match stream_type {
                    StreamType::Live => StreamType::Buffered,
                    _ => StreamType::Live,
                };
            }
        }
    }
    *session = None;
    Err(last_error)
}

/// Stop a Chromecast session (port of `_release_cast`: stop
/// the media, not the app, then close the connection).
fn chromecast_stop(session: &mut Option<ChromecastSession>, device: &CastDevice) {
    let Some(live) = session.take() else {
        return;
    };
    if live.endpoint != device.control_url {
        // A session to a different device is not this stop's
        // business; put it back.
        *session = Some(live);
        return;
    }
    if let Some(media_session_id) = live.media_session_id {
        let _ = live
            .device
            .media
            .stop(live.transport_id.as_str(), media_session_id);
    }
    let _ = live
        .device
        .connection
        .disconnect(live.transport_id.as_str());
    // Dropping the device closes the TLS stream.
}

/// Session upkeep between jobs (heartbeat): ping on a slow
/// cadence and answer the device's pings with pongs, exactly
/// the crate's documented receive pattern. Every receive here
/// is answered promptly in practice — our own ping guarantees
/// a pong is in flight — and a dead connection simply ends the
/// drain.
fn chromecast_upkeep(session: &mut Option<ChromecastSession>) {
    let Some(live) = session.as_mut() else {
        return;
    };
    if live.last_upkeep.elapsed() < Duration::from_secs(4) {
        return;
    }
    live.last_upkeep = Instant::now();
    if live.device.heartbeat.ping().is_err() {
        *session = None;
        return;
    }
    for _ in 0..4 {
        match live.device.receive() {
            Ok(rust_cast::ChannelMessage::Heartbeat(_)) => {
                let _ = live.device.heartbeat.pong();
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
}

// ---------------------------------------------------------------------------
// Cast worker
// ---------------------------------------------------------------------------

/// Jobs for the cast worker.
pub enum CastJob {
    Discover,
    Play {
        device: CastDevice,
        url: String,
        title: String,
        /// The media type probed from the original stream, when the
        /// playback URL is an opaque proxy URL (S04).
        mime: Option<String>,
    },
    Stop {
        device: CastDevice,
    },
}

/// Events from the cast worker.
#[derive(Clone, Debug)]
pub enum CastEvent {
    Devices(Vec<CastDevice>),
    Played(Result<String, String>),
    Stopped(String),
}

/// Background casting worker: discovery and device control off the UI
/// thread (SSDP windows run for seconds).
pub struct CastWorker {
    jobs: std::sync::mpsc::Sender<CastJob>,
    events: std::sync::Mutex<std::sync::mpsc::Receiver<CastEvent>>,
}

impl Default for CastWorker {
    fn default() -> Self {
        Self::new()
    }
}

impl CastWorker {
    #[must_use]
    pub fn new() -> Self {
        let (job_sender, job_receiver) = std::sync::mpsc::channel::<CastJob>();
        let (event_sender, event_receiver) = std::sync::mpsc::channel::<CastEvent>();
        std::thread::spawn(move || {
            // The Chromecast session lives here and only here:
            // its device handle is confined to this thread.
            let mut cc_session: Option<ChromecastSession> = None;
            loop {
                let job = match job_receiver.recv_timeout(Duration::from_millis(200)) {
                    Ok(job) => job,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        chromecast_upkeep(&mut cc_session);
                        continue;
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                };
                let event = match job {
                    CastJob::Discover => {
                        // Python's scan order: chromecast first.
                        let mut devices = chromecast_discover(4);
                        devices.extend(upnp_discover(4));
                        devices.extend(roku_discover(4));
                        CastEvent::Devices(devices)
                    }
                    CastJob::Play {
                        device,
                        url,
                        title,
                        mime,
                    } => {
                        let result = match device.kind {
                            CastKind::Upnp => {
                                let mime = effective_mime(mime.as_deref(), &url);
                                let played = if is_sonos(&device) {
                                    let didl = sonos_didl(&title, &url, &mime);
                                    upnp_play_with_didl(&device.control_url, &url, &didl)
                                } else {
                                    upnp_play(&device.control_url, &url, &title, &mime)
                                };
                                played.map(|()| device.name.clone())
                            }
                            CastKind::Roku => {
                                let mime = mime
                                    .as_deref()
                                    .map_or_else(|| roku_mime(&url), roku_mime_for_media);
                                roku_play(&device.control_url, &url, &mime, &title)
                                    .map(|()| device.name.clone())
                            }
                            CastKind::Chromecast => {
                                let mime = effective_mime(mime.as_deref(), &url);
                                chromecast_play(&mut cc_session, &device, &url, &title, &mime)
                            }
                        };
                        CastEvent::Played(result)
                    }
                    CastJob::Stop { device } => {
                        match device.kind {
                            CastKind::Upnp => upnp_stop(&device.control_url),
                            CastKind::Roku => roku_stop(&device.control_url),
                            CastKind::Chromecast => {
                                chromecast_stop(&mut cc_session, &device);
                            }
                        }
                        CastEvent::Stopped(device.name.clone())
                    }
                };
                if event_sender.send(event).is_err() {
                    break;
                }
            }
        });
        Self {
            jobs: job_sender,
            events: std::sync::Mutex::new(event_receiver),
        }
    }

    pub fn discover(&self) {
        let _ = self.jobs.send(CastJob::Discover);
    }

    /// Queue a cast. `mime` is the media type probed from the
    /// original stream; pass it whenever `url` is a proxy URL, whose
    /// opaque path cannot reveal the format.
    pub fn play(&self, device: CastDevice, url: String, title: String, mime: Option<String>) {
        let _ = self.jobs.send(CastJob::Play {
            device,
            url,
            title,
            mime,
        });
    }

    pub fn stop(&self, device: CastDevice) {
        let _ = self.jobs.send(CastJob::Stop { device });
    }

    /// The next cast event, when one is waiting.
    #[must_use]
    pub fn try_recv(&self) -> Option<CastEvent> {
        self.events.lock().ok()?.try_recv().ok()
    }
}

/// Keep the map type visible for callers building header lists.
pub type HeaderPairs = BTreeMap<String, String>;

#[cfg(test)]
mod tests {
    use super::{is_sonos, CastDevice, CastKind};

    #[test]
    fn sonos_detection_uses_manufacturer_or_name() {
        let sonos = CastDevice {
            name: "Living Room".into(),
            control_url: String::new(),
            manufacturer: "Sonos, Inc.".into(),
            sink_mimes: Vec::new(),
            kind: CastKind::Upnp,
        };
        assert!(is_sonos(&sonos));
        let tv = CastDevice {
            name: "TV".into(),
            manufacturer: "Samsung".into(),
            ..sonos.clone()
        };
        assert!(!is_sonos(&tv));
    }

    use super::*;

    #[test]
    fn mime_guesses_and_roku_labels() {
        assert_eq!(guess_mime("http://x/movie.mp4?tok=1"), "video/mp4");
        assert_eq!(guess_mime("http://x/stream"), "audio/mpeg");
        assert_eq!(guess_mime("http://x/aac"), "audio/aac");
        assert_eq!(
            roku_mime("http://x/list.m3u8"),
            "application/vnd.apple.mpegurl"
        );
        assert_eq!(roku_mime("http://x/song.wav"), "audio/wav");
        assert_eq!(roku_mime("http://x/movie.mkv"), "video/mp4");
    }

    #[test]
    fn probed_mime_survives_the_opaque_proxy_url() {
        // S04 regression: the proxy path `/s/<hash>` has no format
        // in it; guessing from it lost HLS/TS metadata (UPnP fell
        // back to audio/mpeg, Roku to video/mp4).
        let proxy_url = "http://192.168.1.20:8080/s/0123456789abcdef0123";
        let hls = effective_mime(Some("application/vnd.apple.mpegurl"), proxy_url);
        assert_eq!(hls, "application/vnd.apple.mpegurl");
        assert_eq!(roku_mime_for_media(&hls), "application/vnd.apple.mpegurl");
        let ts = effective_mime(Some("video/mp2t"), proxy_url);
        assert_eq!(ts, "video/mp2t");
        // Without a probed type the URL guess is the fallback, and
        // Roku's label for a probed type matches guessing from the
        // original URL.
        assert_eq!(effective_mime(None, proxy_url), "audio/mpeg");
        assert_eq!(effective_mime(Some(""), proxy_url), "audio/mpeg");
        for original in [
            "http://x/list.m3u8",
            "http://x/movie.mkv",
            "http://x/song.wav",
            "http://x/song.mp3",
            "http://x/movie.mp4?tok=1",
        ] {
            assert_eq!(
                roku_mime_for_media(&guess_mime(original)),
                roku_mime(original),
                "{original}"
            );
        }
    }

    #[test]
    fn youtube_ids_and_ts_detection() {
        assert_eq!(
            youtube_id("https://youtu.be/abcDEF123_-").as_deref(),
            Some("abcDEF123_-")
        );
        assert_eq!(
            youtube_id("https://www.youtube.com/watch?v=xyz123").as_deref(),
            Some("xyz123")
        );
        assert_eq!(youtube_id("https://example.com/"), None);
        let mut ts = vec![0_u8; 2048];
        for i in 0..10 {
            ts[i * 188] = 0x47;
        }
        assert!(looks_like_mpegts(&ts));
        // Mid-packet: sync at offset 5 with 188 stride.
        let mut mid = vec![0_u8; 2048];
        for n in 0..4 {
            mid[5 + n * 188] = 0x47;
        }
        assert!(looks_like_mpegts(&mid));
        assert!(!looks_like_mpegts(b"GXXXXXXXXXXXXXXXXXXXXXXX"));
    }

    #[test]
    fn description_parse_and_didl() {
        let body = "<device><friendlyName>Living Room TV</friendlyName><manufacturer>Acme</manufacturer><serviceList><service><serviceType>urn:schemas-upnp-org:service:AVTransport:1</serviceType><controlURL>/upnp/control/avt</controlURL></service><service><serviceType>urn:schemas-upnp-org:service:ConnectionManager:1</serviceType><controlURL>cm</controlURL></service></serviceList></device>";
        let (name, av, cm, maker) =
            parse_device_description("http://192.168.1.5:1400/desc.xml", body).expect("parse");
        assert_eq!(name, "Living Room TV");
        assert_eq!(av, "http://192.168.1.5:1400/upnp/control/avt");
        assert_eq!(cm, "http://192.168.1.5:1400/cm");
        assert_eq!(maker, "Acme");
        let didl = upnp_didl(
            "http://x/s?a=1&b=2",
            "A & B",
            "video/mp4",
            "object.item.videoItem",
        );
        assert!(didl.contains("http://x/s?a=1&amp;b=2"));
        assert!(didl.contains("<dc:title>A &amp; B</dc:title>"));
    }

    #[test]
    fn chromecast_host_pick_prefers_ipv4() {
        // Python's mdns_host: IPv4 first, deterministic among
        // equals, IPv6 still discoverable on its own.
        let mixed = vec![
            "fe80::1".to_string(),
            "192.168.1.30".to_string(),
            "192.168.1.20".to_string(),
        ];
        assert_eq!(pick_cast_host(&mixed).as_deref(), Some("192.168.1.20"));
        assert_eq!(
            pick_cast_host(&["fe80::1".to_string()]).as_deref(),
            Some("fe80::1")
        );
        assert_eq!(pick_cast_host(&[]), None);
    }

    #[test]
    fn chromecast_names_endpoints_and_records() {
        assert_eq!(
            chromecast_display_name("Living Room._googlecast._tcp.local.", Some("TV")),
            "TV"
        );
        assert_eq!(
            chromecast_display_name("Living Room._googlecast._tcp.local.", None),
            "Living Room"
        );
        assert_eq!(
            chromecast_display_name("Living Room._googlecast._tcp.local.", Some("  ")),
            "Living Room"
        );
        let device = chromecast_device("TV".to_string(), "192.168.1.20", 8009, "Chromecast Ultra");
        assert_eq!(device.kind, CastKind::Chromecast);
        assert_eq!(device.control_url, "192.168.1.20:8009");
        assert_eq!(device.manufacturer, "Chromecast Ultra");
        let v6 = chromecast_device("TV".to_string(), "fe80::1", 8009, "");
        assert_eq!(v6.control_url, "[fe80::1]:8009");
        assert_eq!(v6.manufacturer, "Chromecast");
        assert_eq!(
            parse_cast_endpoint("192.168.1.20:8009"),
            Some(("192.168.1.20".to_string(), 8009))
        );
        assert_eq!(
            parse_cast_endpoint("[fe80::1]:8009"),
            Some(("fe80::1".to_string(), 8009))
        );
        assert_eq!(
            parse_cast_endpoint("castbox.local"),
            Some(("castbox.local".to_string(), DEFAULT_CAST_PORT))
        );
        assert_eq!(parse_cast_endpoint("  "), None);
    }

    #[test]
    fn roku_launch_url_and_header_normalization() {
        let url = roku_play_url(
            "http://roku:8060",
            "http://x/list.m3u8",
            "application/vnd.apple.mpegurl",
            "Show",
        );
        assert!(url.starts_with("http://roku:8060/launch/2213?"));
        assert!(url.contains("videoFormat=hls"));
        let headers = normalize_request_headers(
            &[
                ("ua".to_string(), "Agent".to_string()),
                ("x-custom-thing".to_string(), "1".to_string()),
            ],
            &["Referer: http://r/".to_string()],
            true,
        );
        assert!(headers.contains(&("User-Agent".to_string(), "Agent".to_string())));
        assert!(headers.contains(&("X-Custom-Thing".to_string(), "1".to_string())));
        assert!(headers.contains(&("Referer".to_string(), "http://r/".to_string())));
    }
}
