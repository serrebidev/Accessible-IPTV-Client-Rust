//! Casting protocol cores, ported from the Python Caster stack
//! (`caster_engine.py`, `caster_devices.py`, `caster_extras.py`): media
//! helpers, SSDP discovery, UPnP/DLNA control, Roku ECP, Kodi JSON-RPC,
//! the Sonos DIDL builder, and the stream proxy's header normalization.

use std::collections::BTreeMap;
use std::net::UdpSocket;
use std::time::{Duration, Instant};

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
        if mime == "audio/x-wav" {
            return "audio/wav".to_string();
        }
        if mime.starts_with("audio/") {
            return mime.to_string();
        }
    }
    "video/mp4".to_string()
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
pub fn upnp_play(
    control_url: &str,
    media_url: &str,
    title: &str,
    mime: &str,
) -> Result<(), String> {
    let escaped = xml_escape(media_url);
    let didl = upnp_didl(media_url, title, mime, "object.item.videoItem");
    let didl_escaped = xml_escape(&didl);
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
// Cast worker
// ---------------------------------------------------------------------------

/// Jobs for the cast worker.
pub enum CastJob {
    Discover,
    Play {
        device: CastDevice,
        url: String,
        title: String,
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
            while let Ok(job) = job_receiver.recv() {
                let event = match job {
                    CastJob::Discover => {
                        let mut devices = upnp_discover(4);
                        devices.extend(roku_discover(4));
                        CastEvent::Devices(devices)
                    }
                    CastJob::Play { device, url, title } => {
                        let result = match device.kind {
                            CastKind::Upnp => {
                                let mime = guess_mime(&url);
                                upnp_play(&device.control_url, &url, &title, &mime)
                                    .map(|()| device.name.clone())
                            }
                            CastKind::Roku => {
                                let mime = roku_mime(&url);
                                roku_play(&device.control_url, &url, &mime, &title)
                                    .map(|()| device.name.clone())
                            }
                        };
                        CastEvent::Played(result)
                    }
                    CastJob::Stop { device } => {
                        match device.kind {
                            CastKind::Upnp => upnp_stop(&device.control_url),
                            CastKind::Roku => roku_stop(&device.control_url),
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

    pub fn play(&self, device: CastDevice, url: String, title: String) {
        let _ = self.jobs.send(CastJob::Play { device, url, title });
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
