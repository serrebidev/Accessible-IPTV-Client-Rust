//! Fast direct-download URLs for catch-up programmes, ported
//! from Python's `catchup_direct.py` (second-round audit N13).
//!
//! Many backends expose a catch-up programme not only as an HLS
//! playlist but as a progressive `index-<utc>-<duration>.mp4`
//! file next to it; the file downloads as fast as HTTP allows
//! while the HLS pipeline paces itself segment by segment. The
//! candidate builder is pure string work. The probe never
//! follows a redirect and never opens the media itself until a
//! candidate is verified: on a one-stream-per-account provider,
//! opening the programme's stream makes the very next request —
//! the real download — come back 403. When the probe did touch
//! the media, the caller waits out a short settle before ffmpeg
//! asks, for the same reason.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::playback::StreamHeaders;

/// How long a one-stream provider holds a closed media request
/// against the account (Python's `_MEDIA_SESSION_SETTLE_SECONDS`).
pub const MEDIA_SESSION_SETTLE_SECONDS: f64 = 2.5;

/// Redirect chains are short; anything longer is a loop.
const MAX_REDIRECT_HOPS: usize = 5;

const REJECT_CONTENT_TYPES: [&str; 4] = [
    "application/json",
    "application/xhtml+xml",
    "application/xml",
    "text/html",
];

/// Direct `.mp4` URLs worth probing for one catch-up playlist
/// URL, most likely first, deduplicated (Python's
/// `candidate_direct_urls`). When the playlist name itself is
/// date-stamped (`index-<utc>-<dur>.m3u8`), the server's own
/// numbers are authoritative and come first; the caller's
/// window follows unless identical; a third candidate looks
/// under the first path segment only, where some backends host
/// the files after a redirect.
#[must_use]
pub fn candidate_direct_urls(url: &str, utc: i64, duration: i64) -> Vec<String> {
    let Some(parts) = split_url(url) else {
        return Vec::new();
    };
    if parts.scheme.is_empty() || parts.netloc.is_empty() {
        return Vec::new();
    }
    let filename = parts.path.rsplit('/').next().unwrap_or("");
    let directory = &parts.path[..parts.path.len() - filename.len()];
    let build = |dir: &str, file: &str| {
        let query = if parts.query.is_empty() {
            String::new()
        } else {
            format!("?{}", parts.query)
        };
        format!("{}://{}{dir}{file}{query}", parts.scheme, parts.netloc)
    };
    let mut out: Vec<String> = Vec::new();
    let dated = parse_dated_index(filename);
    if let Some((dated_utc, dated_dur)) = dated {
        out.push(build(
            directory,
            &format!("index-{dated_utc}-{dated_dur}.mp4"),
        ));
    }
    if dated != Some((utc, duration)) {
        out.push(build(directory, &format!("index-{utc}-{duration}.mp4")));
    }
    let segments: Vec<&str> = parts.path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() >= 2 {
        out.push(build(
            &format!("/{}/", segments[0]),
            &format!("index-{utc}-{duration}.mp4"),
        ));
    }
    let mut unique: Vec<String> = Vec::new();
    for candidate in out {
        if !unique.contains(&candidate) {
            unique.push(candidate);
        }
    }
    unique
}

/// `index-<utc>-<duration>.m3u8` with Python's digit shapes
/// (utc 9–12 digits, duration 2–7), case-insensitive suffix.
fn parse_dated_index(filename: &str) -> Option<(i64, i64)> {
    let lower = filename.to_lowercase();
    let stem = lower.strip_suffix(".m3u8")?.strip_prefix("index-")?;
    let (utc, dur) = stem.split_once('-')?;
    let digits = |s: &str, min: usize, max: usize| {
        (min..=max).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit())
    };
    if !digits(utc, 9, 12) || !digits(dur, 2, 7) {
        return None;
    }
    Some((utc.parse().ok()?, dur.parse().ok()?))
}

struct UrlParts {
    scheme: String,
    netloc: String,
    path: String,
    query: String,
}

fn split_url(url: &str) -> Option<UrlParts> {
    let (scheme, rest) = url.split_once("://")?;
    let (authority, tail) = match rest.find(['/', '?', '#']) {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, ""),
    };
    let (path, query) = match tail.split_once('?') {
        Some((path, query)) => (path, query.split('#').next().unwrap_or(query)),
        None => (tail.split('#').next().unwrap_or(tail), ""),
    };
    Some(UrlParts {
        scheme: scheme.to_string(),
        netloc: authority.to_string(),
        path: path.to_string(),
        query: query.to_string(),
    })
}

/// Resolve a redirect `Location` against the URL that produced
/// it (the `urljoin` shapes redirectors actually send: absolute,
/// scheme-relative, root-relative, and directory-relative with
/// dot segments).
#[must_use]
pub fn join_url(base: &str, location: &str) -> String {
    let location = location.trim();
    if location.contains("://") {
        return location.to_string();
    }
    let Some(parts) = split_url(base) else {
        return location.to_string();
    };
    if let Some(rest) = location.strip_prefix("//") {
        return format!("{}://{}", parts.scheme, rest);
    }
    if location.starts_with('/') {
        let (path, query) = split_location(location);
        let query = if query.is_empty() {
            String::new()
        } else {
            format!("?{query}")
        };
        return format!(
            "{}://{}{}{}",
            parts.scheme,
            parts.netloc,
            normalize_path(path),
            query
        );
    }
    if location.is_empty() {
        return base.to_string();
    }
    let directory = parts
        .path
        .rsplit_once('/')
        .map_or("/", |(dir, _)| dir)
        .to_string();
    let (rel_path, query) = split_location(location);
    let query = if query.is_empty() {
        String::new()
    } else {
        format!("?{query}")
    };
    format!(
        "{}://{}{}{}",
        parts.scheme,
        parts.netloc,
        normalize_path(&format!("{directory}/{rel_path}")),
        query
    )
}

fn split_location(location: &str) -> (&str, &str) {
    match location.split_once('?') {
        Some((path, query)) => (path, query.split('#').next().unwrap_or(query)),
        None => (location.split('#').next().unwrap_or(location), ""),
    }
}

fn normalize_path(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    let mut normalized = String::from("/");
    normalized.push_str(&out.join("/"));
    if (path.ends_with('/') || path.ends_with("/.") || path.ends_with("/.."))
        && !normalized.ends_with('/')
    {
        normalized.push('/');
    }
    normalized
}

/// Whether two URLs name the same host: names case-insensitive,
/// the port part of the host, a scheme's default port filled in.
/// An unparseable URL counts as the same host, so a malformed
/// URL does not strip the headers a request needs (Python's
/// `_same_host`).
#[must_use]
pub fn same_host(base_url: &str, target_url: &str) -> bool {
    let host_port = |url: &str| -> Option<(String, u32)> {
        let parts = split_url(url)?;
        let authority = parts.netloc.rsplit('@').next().unwrap_or(&parts.netloc);
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) if port.bytes().all(|b| b.is_ascii_digit()) && !port.is_empty() => {
                (host, port.parse().unwrap_or(0))
            }
            _ => (authority, 0),
        };
        let port = if port == 0 {
            match parts.scheme.to_lowercase().as_str() {
                "https" => 443,
                _ => 80,
            }
        } else {
            port
        };
        Some((host.to_lowercase(), port))
    };
    match (host_port(base_url), host_port(target_url)) {
        (Some(base), Some(target)) => base == target,
        _ => true,
    }
}

/// The headers that carry the account's identity.
const CREDENTIAL_HEADERS: [&str; 4] = [
    "authorization",
    "cookie",
    "proxy-authorization",
    "x-api-key",
];

/// The channel's headers for a request to `target_url`: a
/// catch-up server can redirect anywhere, and the provider's
/// credentials — including its custom headers, which are
/// credentials too — must not follow to a host the app never
/// chose (Python's `headers_for_host`).
#[must_use]
pub fn headers_for_host(
    headers: &StreamHeaders,
    base_url: &str,
    target_url: &str,
) -> StreamHeaders {
    if same_host(base_url, target_url) {
        return headers.clone();
    }
    let extra_names: Vec<String> = headers
        .extra
        .iter()
        .filter_map(|line| line.split(':').next().map(|n| n.trim().to_lowercase()))
        .collect();
    let dropped = |name: &str| {
        let lower = name.to_lowercase();
        CREDENTIAL_HEADERS.contains(&lower.as_str()) || extra_names.contains(&lower)
    };
    StreamHeaders {
        entries: headers
            .entries
            .iter()
            .filter(|(name, _)| !dropped(name))
            .cloned()
            .collect(),
        extra: Vec::new(),
    }
}

/// Whether response headers look like a downloadable media
/// file: no type at all is acceptable; a media or octet-stream
/// type is; error-page and data types are not (Python's
/// `_headers_look_like_media`).
#[must_use]
pub fn headers_look_like_media(content_type: Option<&str>) -> bool {
    let Some(raw) = content_type else {
        return true;
    };
    let ctype = raw.split(';').next().unwrap_or("").trim().to_lowercase();
    if ctype.is_empty() {
        return true;
    }
    (ctype.starts_with("video/")
        || ctype.starts_with("audio/")
        || ctype == "application/octet-stream")
        && !REJECT_CONTENT_TYPES.contains(&ctype.as_str())
}

// ---------------------------------------------------------------------
// Settle bookkeeping: one-stream providers count a media request
// as a session for a moment after it closes.

/// Record that a background media request to `url`'s server has
/// just closed, so the next open of that server waits out the
/// settle (Python's `note_media_released`).
pub fn note_media_released(url: &str) {
    let host = host_key(url);
    if host.is_empty() {
        return;
    }
    if let Ok(mut map) = released_hosts().lock() {
        map.insert(host, Instant::now());
    }
}

/// Seconds still to wait before opening `url`, given
/// `note_media_released` (Python's `media_settle_remaining`).
#[must_use]
pub fn media_settle_remaining(url: &str) -> f64 {
    let host = host_key(url);
    let Ok(map) = released_hosts().lock() else {
        return 0.0;
    };
    match map.get(&host) {
        Some(released) => {
            let elapsed = released.elapsed().as_secs_f64();
            (MEDIA_SESSION_SETTLE_SECONDS - elapsed).max(0.0)
        }
        None => 0.0,
    }
}

/// Wait until a one-stream provider has let go of a media
/// request. Call on a worker thread, after anything that
/// requested the media itself and before ffmpeg asks for it
/// (Python's `settle_media_session`).
pub fn settle_media_session() {
    std::thread::sleep(Duration::from_secs_f64(MEDIA_SESSION_SETTLE_SECONDS));
}

fn released_hosts() -> &'static Mutex<HashMap<String, Instant>> {
    static HOSTS: std::sync::OnceLock<Mutex<HashMap<String, Instant>>> = std::sync::OnceLock::new();
    HOSTS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn host_key(url: &str) -> String {
    split_url(url).map_or_else(|| url.to_lowercase(), |parts| parts.netloc.to_lowercase())
}

// ---------------------------------------------------------------------
// The probe itself. Transport is a trait so the walk is testable
// without a network; the production transport is ureq with
// redirects refused and every status surfaced.

/// One HTTP answer for the probe: any status counts as an
/// answer; transport failures never reach this type.
#[derive(Clone, Debug, Default)]
pub struct ProbeResponse {
    pub status: u16,
    pub content_type: Option<String>,
    pub content_length: Option<String>,
    pub location: Option<String>,
}

/// The probe's network surface: HEAD, and GET with an optional
/// 1-byte Range. Implementations must not follow redirects.
pub trait ProbeTransport {
    /// One request; `None` on a transport failure.
    fn request(
        &self,
        method: &str,
        url: &str,
        headers: &StreamHeaders,
        range_first_byte: bool,
    ) -> Option<ProbeResponse>;
}

/// True when `url` answers like a real downloadable media file
/// (Python's `probe_direct_url`). The probe never follows a
/// redirect: following one rewrites HEAD into GET on some
/// stacks and opens the programme's stream, holding the
/// account's one stream. Only a 405/501 — "HEAD is not
/// supported" — earns the 1-byte ranged GET; a 403 is a busy
/// account and a 404 an ordinary miss.
#[must_use]
pub fn probe_direct_url(
    transport: &dyn ProbeTransport,
    url: &str,
    headers: &StreamHeaders,
) -> bool {
    match transport.request("HEAD", url, headers, false) {
        Some(response) if (200..300).contains(&response.status) => {
            headers_look_like_media(response.content_type.as_deref())
                && response.content_length.as_deref() != Some("0")
        }
        Some(response) if response.status == 405 || response.status == 501 => {
            match transport.request("GET", url, headers, true) {
                Some(get) if (200..300).contains(&get.status) => {
                    headers_look_like_media(get.content_type.as_deref())
                }
                _ => false,
            }
        }
        _ => false,
    }
}

/// `(where url redirects to, whether the media itself answered)`
/// — one hop only, and the target is never requested (Python's
/// `_next_hop`). GET is tried only when HEAD is refused.
fn next_hop(
    transport: &dyn ProbeTransport,
    url: &str,
    headers: &StreamHeaders,
) -> (Option<String>, bool) {
    for method in ["HEAD", "GET"] {
        match transport.request(method, url, headers, false) {
            Some(response) if (200..300).contains(&response.status) => {
                return (None, true);
            }
            Some(response) if (300..400).contains(&response.status) => {
                let target = response
                    .location
                    .as_deref()
                    .map(|location| join_url(url, location));
                return (target, false);
            }
            Some(response)
                if method == "HEAD" && (response.status == 405 || response.status == 501) =>
            {
                // HEAD refused: fall through to the GET attempt.
            }
            _ => return (None, false),
        }
    }
    (None, false)
}

/// The redirect walk and candidate probing, without the final
/// settle sleep: returns the verified direct URL and whether
/// any request touched the media itself. Every window is probed
/// at each hop before the next hop is taken — probing them one
/// call at a time let the first window's walk open the archive
/// stream, and that open stream made the provider refuse the
/// programme-exact file with 403 (Python's
/// `direct_download_url`, minus the settle).
#[must_use]
pub fn probe_direct_download(
    transport: &dyn ProbeTransport,
    url: &str,
    windows: &[(i64, i64)],
    headers: &StreamHeaders,
) -> (Option<String>, bool) {
    let windows: Vec<(i64, i64)> = windows
        .iter()
        .copied()
        .filter(|(utc, dur)| *utc != 0 && *dur > 0)
        .collect();
    if url.is_empty() || windows.is_empty() {
        return (None, false);
    }
    let mut tried: Vec<String> = Vec::new();
    let probe_around =
        |base: &str, base_headers: &StreamHeaders, tried: &mut Vec<String>| -> Option<String> {
            for (utc, duration) in &windows {
                for candidate in candidate_direct_urls(base, *utc, *duration) {
                    if tried.contains(&candidate) || candidate == base {
                        continue;
                    }
                    tried.push(candidate.clone());
                    if probe_direct_url(transport, &candidate, base_headers) {
                        return Some(candidate);
                    }
                }
            }
            None
        };
    let mut current = url.to_string();
    let mut touched_media = false;
    let mut seen: Vec<String> = vec![url.to_string()];
    let mut found = None;
    for _hop in 0..MAX_REDIRECT_HOPS {
        let hop_headers = headers_for_host(headers, url, &current);
        let (target, is_media) = next_hop(transport, &current, &hop_headers);
        touched_media = touched_media || is_media;
        let Some(target) = target else { break };
        if seen.contains(&target) {
            break;
        }
        seen.push(target.clone());
        let target_headers = headers_for_host(headers, url, &target);
        found = probe_around(&target, &target_headers, &mut tried);
        if found.is_some() {
            break;
        }
        current = target;
    }
    if found.is_none() {
        found = probe_around(url, headers, &mut tried);
    }
    (found, touched_media)
}

/// A verified fast direct URL for this catch-up programme, or
/// None — with the settle waited out when the probe touched the
/// media or verified a file (a HEAD on the file is itself a
/// media request the provider counts). Network work; keep it
/// off the GUI thread (Python's `direct_download_url`).
#[must_use]
pub fn direct_download_url(
    transport: &dyn ProbeTransport,
    url: &str,
    windows: &[(i64, i64)],
    headers: &StreamHeaders,
) -> Option<String> {
    let (found, touched_media) = probe_direct_download(transport, url, windows, headers);
    if found.is_some() || touched_media {
        settle_media_session();
    }
    found
}

/// The production transport: ureq, redirects refused, every
/// HTTP status surfaced as an answer (Python's `_NoRedirect`
/// opener).
pub struct UreqProbeTransport {
    agent: ureq::Agent,
}

impl Default for UreqProbeTransport {
    fn default() -> Self {
        Self::new(Duration::from_secs(6))
    }
}

impl UreqProbeTransport {
    #[must_use]
    pub fn new(timeout: Duration) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(timeout))
            .build()
            .into();
        Self { agent }
    }
}

impl ProbeTransport for UreqProbeTransport {
    fn request(
        &self,
        method: &str,
        url: &str,
        headers: &StreamHeaders,
        range_first_byte: bool,
    ) -> Option<ProbeResponse> {
        let mut request = match method {
            "HEAD" => self.agent.head(url),
            _ => self.agent.get(url),
        };
        for (name, value) in &headers.entries {
            request = request.header(name, value);
        }
        for line in &headers.extra {
            if let Some((name, value)) = line.split_once(':') {
                request = request.header(name.trim(), value.trim());
            }
        }
        if range_first_byte {
            request = request.header("Range", "bytes=0-0");
        }
        let response = request.call().ok()?;
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        };
        let answer = ProbeResponse {
            status: response.status().as_u16(),
            content_type: header("content-type"),
            content_length: header("content-length"),
            location: header("location"),
        };
        // The response drops here unread: a GET answer's body is
        // media bytes on the one-stream provider's counter, and
        // closing without reading is what Python's probe does.
        drop(response);
        Some(answer)
    }
}

// ---------------------------------------------------------------------
// The worker: the probe (plus its settle) runs on its own
// thread, like the other core workers.

/// One probe request: the catch-up URL, its candidate windows
/// (most wanted first), and the channel's headers.
#[derive(Clone, Debug)]
pub struct DirectProbeRequest {
    pub key: String,
    pub url: String,
    pub windows: Vec<(i64, i64)>,
    pub headers: StreamHeaders,
}

/// A finished probe: the verified direct URL when one exists.
#[derive(Clone, Debug)]
pub struct DirectProbeResult {
    pub key: String,
    pub direct_url: Option<String>,
}

/// A one-job-at-a-time probe worker.
pub struct DirectProbeWorker {
    jobs: std::sync::mpsc::Sender<DirectProbeRequest>,
    results: Mutex<std::sync::mpsc::Receiver<DirectProbeResult>>,
}

impl Default for DirectProbeWorker {
    fn default() -> Self {
        Self::new()
    }
}

impl DirectProbeWorker {
    #[must_use]
    pub fn new() -> Self {
        let (job_sender, job_receiver) = std::sync::mpsc::channel::<DirectProbeRequest>();
        let (result_sender, result_receiver) = std::sync::mpsc::channel::<DirectProbeResult>();
        std::thread::spawn(move || {
            let transport = UreqProbeTransport::default();
            while let Ok(request) = job_receiver.recv() {
                let direct_url = direct_download_url(
                    &transport,
                    &request.url,
                    &request.windows,
                    &request.headers,
                );
                if result_sender
                    .send(DirectProbeResult {
                        key: request.key,
                        direct_url,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            jobs: job_sender,
            results: Mutex::new(result_receiver),
        }
    }

    /// Queue one probe.
    pub fn start(&self, request: DirectProbeRequest) {
        let _ = self.jobs.send(request);
    }

    /// The next finished probe, when one is waiting.
    #[must_use]
    pub fn try_recv(&self) -> Option<DirectProbeResult> {
        self.results.lock().ok()?.try_recv().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn candidates_follow_the_playlist_layout() {
        let url = "http://prov.example/archive/show/index-1700000000-3600.m3u8?token=abc";
        // Dated playlist: the server's numbers first, then the
        // caller's when they differ, then the first-segment home.
        assert_eq!(
            candidate_direct_urls(url, 1_700_000_100, 1800),
            vec![
                "http://prov.example/archive/show/index-1700000000-3600.mp4?token=abc",
                "http://prov.example/archive/show/index-1700000100-1800.mp4?token=abc",
                "http://prov.example/archive/index-1700000100-1800.mp4?token=abc",
            ]
        );
        // Same window as the playlist's own stamp: no duplicate.
        assert_eq!(candidate_direct_urls(url, 1_700_000_000, 3600).len(), 2);
        // An undated playlist name: caller window, then segment home.
        assert_eq!(
            candidate_direct_urls("http://h/a/b/index.m3u8", 100, 60),
            vec![
                "http://h/a/b/index-100-60.mp4",
                "http://h/a/index-100-60.mp4",
            ]
        );
        assert!(candidate_direct_urls("not a url", 100, 60).is_empty());
    }

    #[test]
    fn credentials_do_not_follow_a_redirect_to_another_host() {
        let headers = StreamHeaders {
            entries: vec![
                ("User-Agent".to_string(), "Agent".to_string()),
                ("Authorization".to_string(), "Bearer secret".to_string()),
                ("Cookie".to_string(), "session=abc".to_string()),
            ],
            extra: vec!["X-Custom: v".to_string()],
        };
        let scoped = headers_for_host(
            &headers,
            "http://prov.example/x.m3u8",
            "http://cdn.other.example/f.mp4",
        );
        assert_eq!(
            scoped.entries,
            vec![("User-Agent".to_string(), "Agent".to_string())]
        );
        assert!(scoped.extra.is_empty());
        // Same host (default port spelled out): untouched.
        let same = headers_for_host(
            &headers,
            "http://prov.example/x.m3u8",
            "http://prov.example:80/f.mp4",
        );
        assert_eq!(same.entries.len(), 3);
        assert_eq!(same.extra.len(), 1);
        // Another port is another service.
        assert!(!same_host(
            "http://prov.example/x",
            "http://prov.example:8080/x"
        ));
    }

    #[test]
    fn media_type_gate() {
        assert!(headers_look_like_media(None));
        assert!(headers_look_like_media(Some("video/mp4")));
        assert!(headers_look_like_media(Some(
            "application/octet-stream; charset=binary"
        )));
        assert!(!headers_look_like_media(Some("text/html")));
        assert!(!headers_look_like_media(Some("application/json")));
    }

    #[test]
    fn join_handles_redirect_shapes() {
        let base = "http://h/a/b/index.m3u8";
        assert_eq!(join_url(base, "http://other/x.mp4"), "http://other/x.mp4");
        assert_eq!(join_url(base, "/play/123"), "http://h/play/123");
        assert_eq!(join_url(base, "next.ts"), "http://h/a/b/next.ts");
        assert_eq!(join_url(base, "../up.ts"), "http://h/a/up.ts");
        assert_eq!(join_url(base, "//cdn.example/f"), "http://cdn.example/f");
    }

    /// A scripted transport: answers by URL from a table, and
    /// records every request so the walk's shape is assertable.
    struct FakeTransport {
        answers: HashMap<String, ProbeResponse>,
        requests: RefCell<Vec<(String, String)>>,
    }

    impl FakeTransport {
        fn new(answers: Vec<(&str, ProbeResponse)>) -> Self {
            Self {
                answers: answers
                    .into_iter()
                    .map(|(url, resp)| (url.to_string(), resp))
                    .collect(),
                requests: RefCell::new(Vec::new()),
            }
        }
    }

    impl ProbeTransport for FakeTransport {
        fn request(
            &self,
            method: &str,
            url: &str,
            _headers: &StreamHeaders,
            _range_first_byte: bool,
        ) -> Option<ProbeResponse> {
            self.requests
                .borrow_mut()
                .push((method.to_string(), url.to_string()));
            self.answers.get(url).cloned()
        }
    }

    fn redirect(location: &str) -> ProbeResponse {
        ProbeResponse {
            status: 302,
            location: Some(location.to_string()),
            ..ProbeResponse::default()
        }
    }

    fn media_ok() -> ProbeResponse {
        ProbeResponse {
            status: 200,
            content_type: Some("video/mp4".to_string()),
            content_length: Some("1000".to_string()),
            ..ProbeResponse::default()
        }
    }

    #[test]
    fn walk_probes_around_the_redirect_target_without_opening_it() {
        // The playlist redirects to the archive stream; the .mp4
        // next to the target verifies. The target itself must
        // never be GET/HEAD-opened beyond the one hop request.
        let start = "http://prov.example/play/show.m3u8";
        let target = "http://prov.example/archive/timeshift-abs-1700000000.ts";
        let file = "http://prov.example/archive/index-1700000000-3600.mp4";
        let transport = FakeTransport::new(vec![
            (start, redirect(target)),
            (target, redirect("/elsewhere")),
            (file, media_ok()),
        ]);
        let (found, touched) = probe_direct_download(
            &transport,
            start,
            &[(1_700_000_000, 3600)],
            &StreamHeaders::default(),
        );
        assert_eq!(found.as_deref(), Some(file));
        assert!(!touched, "the archive stream must not be opened");
    }

    #[test]
    fn probe_refuses_error_pages_and_zero_length() {
        let file = "http://h/a/index-100-60.mp4";
        let html = FakeTransport::new(vec![(
            file,
            ProbeResponse {
                status: 200,
                content_type: Some("text/html".to_string()),
                content_length: Some("500".to_string()),
                ..ProbeResponse::default()
            },
        )]);
        assert!(!probe_direct_url(&html, file, &StreamHeaders::default()));
        let empty = FakeTransport::new(vec![(
            file,
            ProbeResponse {
                status: 200,
                content_type: Some("video/mp4".to_string()),
                content_length: Some("0".to_string()),
                ..ProbeResponse::default()
            },
        )]);
        assert!(!probe_direct_url(&empty, file, &StreamHeaders::default()));
        // A 403 is a busy account: no ranged-GET second touch.
        let busy = FakeTransport::new(vec![(
            file,
            ProbeResponse {
                status: 403,
                ..ProbeResponse::default()
            },
        )]);
        assert!(!probe_direct_url(&busy, file, &StreamHeaders::default()));
        assert_eq!(busy.requests.borrow().len(), 1);
    }

    #[test]
    fn head_refusal_falls_back_to_a_ranged_get() {
        let file = "http://h/a/index-100-60.mp4";
        let transport = FakeTransport::new(vec![(
            file,
            ProbeResponse {
                status: 405,
                ..ProbeResponse::default()
            },
        )]);
        // HEAD gets 405, the ranged GET gets the same 405 from
        // this fake — the point is the second request happens.
        assert!(!probe_direct_url(
            &transport,
            file,
            &StreamHeaders::default()
        ));
        assert_eq!(transport.requests.borrow().len(), 2);
        assert_eq!(transport.requests.borrow()[1].0, "GET");
    }
}
