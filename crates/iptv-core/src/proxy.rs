//! Local stream proxy (port of the Python stream proxy): serves
//! header-carrying streams to devices that cannot send headers
//! (cast targets), rewrites HLS playlists so segments also come
//! through the proxy, and caches segment responses in memory.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};
use ureq::ResponseExt;

const MAX_SEGMENT_CACHE_BYTES: usize = 64 * 1024 * 1024;
const MAX_SEGMENTS: usize = 64;

#[derive(Clone)]
struct Route {
    url: String,
    headers: Vec<(String, String)>,
}

type Routes = Arc<Mutex<HashMap<String, Route>>>;
type SegmentCache = Arc<Mutex<HashMap<String, (String, Vec<u8>)>>>;

/// The primary LAN IPv4 address of this machine, if one can be
/// found (port of `StreamProxy._get_local_ip`'s connected-socket
/// method): a UDP socket "connected" to a public address reveals
/// the source address the OS would route from, without sending
/// anything. Loopback and unspecified results do not count.
#[must_use]
pub fn lan_ip() -> Option<std::net::IpAddr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    let addr = socket.local_addr().ok()?.ip();
    if addr.is_loopback() || addr.is_unspecified() {
        None
    } else {
        Some(addr)
    }
}

/// The host the proxy binds and advertises: the LAN address when
/// one exists (a cast target on another host must be able to reach
/// the proxy — a loopback URL points the TV at itself), else
/// loopback for same-machine use.
#[must_use]
pub fn advertised_host(lan: Option<std::net::IpAddr>) -> String {
    lan.map_or_else(|| "127.0.0.1".to_string(), |ip| ip.to_string())
}

/// The Windows firewall rule name for one proxy port.
#[must_use]
pub fn firewall_rule_name(port: u16) -> String {
    format!("IPTV Proxy ({port})")
}

/// Windows: let devices on the local network reach the proxy
/// (port of `stream_proxy.py`'s `_ensure_firewall_rule`): a
/// per-port inbound allow rule for the private/domain
/// profiles, deleted and re-added so a stale rule never
/// shadows the new port. Best effort — like the reference,
/// every failure is ignored. Returns the rule name to remove
/// on shutdown.
#[cfg(windows)]
#[allow(clippy::unnecessary_wraps)] // shared signature: non-Windows returns None
fn ensure_firewall_rule(port: u16) -> Option<String> {
    use std::os::windows::process::CommandExt as _;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let name = firewall_rule_name(port);
    let name_arg = format!("name={name}");
    let port_arg = format!("localport={port}");
    let delete: &[&str] = &["advfirewall", "firewall", "delete", "rule", &name_arg];
    let add: &[&str] = &[
        "advfirewall",
        "firewall",
        "add",
        "rule",
        &name_arg,
        "dir=in",
        "action=allow",
        "protocol=TCP",
        &port_arg,
        "profile=private,domain",
    ];
    for args in [delete, add] {
        let _ = std::process::Command::new("netsh")
            .args(args)
            .creation_flags(CREATE_NO_WINDOW)
            .output();
    }
    Some(name)
}

#[cfg(not(windows))]
fn ensure_firewall_rule(_port: u16) -> Option<String> {
    None
}

/// Remove the rule `ensure_firewall_rule` created (port of
/// `_remove_firewall_rule`); failures ignored.
#[cfg(windows)]
fn remove_firewall_rule(name: &str) {
    use std::os::windows::process::CommandExt as _;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let name_arg = format!("name={name}");
    let _ = std::process::Command::new("netsh")
        .args(["advfirewall", "firewall", "delete", "rule", &name_arg])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
}

#[cfg(not(windows))]
fn remove_firewall_rule(_name: &str) {}

/// A running proxy server on an ephemeral port.
pub struct ProxyServer {
    base: String,
    routes: Routes,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
    firewall_rule: Option<String>,
}

impl ProxyServer {
    /// Start the proxy on the LAN address (falling back to
    /// loopback) with an ephemeral port.
    ///
    /// # Errors
    /// Returns the OS error text when the listener cannot bind.
    pub fn start() -> Result<Self, String> {
        Self::start_on(&advertised_host(lan_ip()))
    }

    /// Start the proxy on a specific host with an ephemeral port.
    ///
    /// # Errors
    /// Returns the OS error text when the listener cannot bind.
    pub fn start_on(host: &str) -> Result<Self, String> {
        let listener = TcpListener::bind((host, 0)).map_err(|e| e.to_string())?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let routes: Routes = Arc::new(Mutex::new(HashMap::new()));
        let cache: SegmentCache = Arc::new(Mutex::new(HashMap::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let base = format!("http://{host}:{port}");
        let thread_routes = Arc::clone(&routes);
        let thread_cache = Arc::clone(&cache);
        let thread_stop = Arc::clone(&stop);
        let thread_base = base.clone();
        let handle = std::thread::spawn(move || {
            while !thread_stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let routes = Arc::clone(&thread_routes);
                        let cache = Arc::clone(&thread_cache);
                        let base = thread_base.clone();
                        std::thread::spawn(move || {
                            handle_connection(stream, &routes, &cache, &base);
                        });
                    }
                    Err(_) => std::thread::sleep(std::time::Duration::from_millis(25)),
                }
            }
        });
        // Python opens the firewall for the proxy port when
        // the proxy starts, so cast devices can reach it.
        let firewall_rule = ensure_firewall_rule(port);
        Ok(Self {
            base,
            routes,
            stop,
            handle: Some(handle),
            firewall_rule,
        })
    }

    /// The proxy URL for a target, registering its headers.
    #[must_use]
    pub fn url_for(&self, target: &str, headers: &[(String, String)]) -> String {
        let token = token_for(target);
        self.routes.lock().map_or_else(
            |_| (),
            |mut routes| {
                routes.insert(
                    token.clone(),
                    Route {
                        url: target.to_string(),
                        headers: headers.to_vec(),
                    },
                );
            },
        );
        format!("{}/s/{token}", self.base)
    }

    /// The server's base URL.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base
    }
}

impl Drop for ProxyServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        if let Some(name) = self.firewall_rule.take() {
            remove_firewall_rule(&name);
        }
    }
}

fn token_for(url: &str) -> String {
    let digest = Sha256::digest(url.as_bytes());
    let hex = crate::hex_lower(&digest);
    hex[..20].to_string()
}

fn handle_connection(
    mut stream: std::net::TcpStream,
    routes: &Routes,
    cache: &SegmentCache,
    base: &str,
) {
    let _ = stream.set_nonblocking(false);
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 8192];
    while buffer.len() < 65536 {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buffer.extend_from_slice(&chunk[..n]);
                if buffer.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            Err(_) => return,
        }
    }
    let request = String::from_utf8_lossy(&buffer).into_owned();
    let mut parts = request.split_whitespace();
    let (Some(method), Some(path)) = (parts.next(), parts.next()) else {
        respond(&mut stream, 405, "text/plain", b"method not allowed");
        return;
    };
    let head = match method {
        "GET" => false,
        "HEAD" => true,
        _ => {
            respond(&mut stream, 405, "text/plain", b"method not allowed");
            return;
        }
    };
    let range = request.lines().find_map(|line| {
        line.to_ascii_lowercase()
            .starts_with("range:")
            .then(|| line[6..].trim().to_string())
    });
    let Some(token) = path.strip_prefix("/s/") else {
        respond(&mut stream, 404, "text/plain", b"not found");
        return;
    };
    let route = routes.lock().ok().and_then(|map| map.get(token).cloned());
    let Some(route) = route else {
        respond(&mut stream, 404, "text/plain", b"unknown stream");
        return;
    };
    serve(
        &mut stream,
        &route,
        range.as_deref(),
        head,
        routes,
        cache,
        base,
    );
}

#[allow(clippy::too_many_lines)] // one request path: cache, fetch, playlist, headers, stream
fn serve(
    stream: &mut std::net::TcpStream,
    route: &Route,
    range: Option<&str>,
    head: bool,
    routes: &Routes,
    cache: &SegmentCache,
    base: &str,
) {
    if !head && range.is_none() && is_segment_url(&route.url) {
        if let Some((content_type, bytes)) = cache
            .lock()
            .ok()
            .and_then(|map| map.get(&route.url).cloned())
        {
            respond(stream, 200, &content_type, &bytes);
            return;
        }
    }
    let mut request = if head {
        ureq::head(&route.url)
    } else {
        ureq::get(&route.url)
    };
    let mut has_user_agent = false;
    for (name, value) in &route.headers {
        if name.eq_ignore_ascii_case("user-agent") {
            has_user_agent = true;
        }
        request = request.header(name.as_str(), value.as_str());
    }
    if !has_user_agent {
        request = request.header("User-Agent", crate::recording::DEFAULT_UPSTREAM_USER_AGENT);
    }
    if let Some(range) = range {
        request = request.header("Range", range);
    }
    let mut response = match request.call() {
        Ok(response) => response,
        Err(ureq::Error::StatusCode(code)) => {
            if head {
                respond_headers_only(stream, code);
            } else {
                respond(stream, code, "text/plain", b"upstream error");
            }
            return;
        }
        Err(_) => {
            respond(stream, 502, "text/plain", b"upstream unreachable");
            return;
        }
    };
    let status = response.status().as_u16();
    let final_url = response.get_uri().to_string();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    let content_type_lower = content_type.to_ascii_lowercase();
    let path_lower = final_url
        .split('?')
        .next()
        .unwrap_or(final_url.as_str())
        .to_ascii_lowercase();
    // path_lower is already lowercased, so these suffix checks are case-insensitive.
    #[allow(clippy::case_sensitive_file_extension_comparisons)]
    let is_playlist = content_type_lower.contains("mpegurl")
        || path_lower.ends_with(".m3u8")
        || path_lower.ends_with(".m3u");
    if is_playlist && !head {
        let mut text = String::new();
        let read = response.body_mut().as_reader().read_to_string(&mut text);
        if read.is_ok() {
            let rewritten = rewrite_playlist(&text, &final_url, base, routes, &route.headers);
            respond(stream, status, &content_type, rewritten.as_bytes());
            return;
        }
    }
    // Stream through, accumulating small segment bodies for the cache.
    let segment = !head && range.is_none() && is_segment_url(&route.url);
    let mut head_text =
        format!("HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nConnection: close\r\n");
    for name in [
        "Content-Length",
        "Content-Range",
        "Accept-Ranges",
        "Last-Modified",
        "ETag",
    ] {
        if let Some(value) = response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
        {
            head_text.push_str(name);
            head_text.push_str(": ");
            head_text.push_str(value);
            head_text.push_str("\r\n");
        }
    }
    head_text.push_str("\r\n");
    if stream.write_all(head_text.as_bytes()).is_err() {
        return;
    }
    if head {
        return;
    }
    let mut reader = response.body_mut().as_reader();
    let mut buffer = vec![0_u8; 65536];
    let mut captured: Vec<u8> = Vec::new();
    loop {
        match reader.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if stream.write_all(&buffer[..n]).is_err() {
                    return;
                }
                if segment && captured.len() + n <= MAX_SEGMENT_CACHE_BYTES / 4 {
                    captured.extend_from_slice(&buffer[..n]);
                }
            }
        }
    }
    if segment && !captured.is_empty() {
        if let Ok(mut map) = cache.lock() {
            let total: usize = map.values().map(|(_, bytes)| bytes.len()).sum();
            if map.len() < MAX_SEGMENTS && total + captured.len() <= MAX_SEGMENT_CACHE_BYTES {
                map.insert(route.url.clone(), (content_type, captured));
            }
        }
    }
}

fn respond_headers_only(stream: &mut std::net::TcpStream, status: u16) {
    let head = format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    let _ = stream.write_all(head.as_bytes());
}

fn respond(stream: &mut std::net::TcpStream, status: u16, content_type: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    if stream.write_all(head.as_bytes()).is_ok() {
        let _ = stream.write_all(body);
    }
}

fn is_segment_url(url: &str) -> bool {
    let path = url.split('?').next().unwrap_or(url).to_ascii_lowercase();
    [".ts", ".m4s", ".aac", ".mp4", ".vtt"]
        .iter()
        .any(|ext| path.ends_with(ext))
}

/// Resolve a possibly-relative playlist entry against its
/// playlist URL.
#[must_use]
pub fn absolutize(base: &str, entry: &str) -> String {
    if entry.starts_with("http://") || entry.starts_with("https://") {
        return entry.to_string();
    }
    if let Some(rest) = entry.strip_prefix("//") {
        let scheme = base.split("://").next().unwrap_or("http");
        return format!("{scheme}://{rest}");
    }
    if entry.starts_with('/') {
        if let Some(index) = base.find("://") {
            let after = &base[index + 3..];
            let host_end = after.find('/').map_or(base.len(), |i| index + 3 + i);
            return format!("{}{}", &base[..host_end], entry);
        }
        return entry.to_string();
    }
    match base.rfind('/') {
        Some(index) => format!("{}{}", &base[..=index], entry),
        None => entry.to_string(),
    }
}

/// Rewrite playlist entries (lines and URI="..." attributes) to
/// proxy URLs carrying the same headers.
fn rewrite_playlist(
    text: &str,
    base_url: &str,
    proxy_base: &str,
    routes: &Routes,
    headers: &[(String, String)],
) -> String {
    let proxy_url = |target: &str| {
        let token = token_for(target);
        routes.lock().map_or_else(
            |_| (),
            |mut map| {
                map.insert(
                    token.clone(),
                    Route {
                        url: target.to_string(),
                        headers: headers.to_vec(),
                    },
                );
            },
        );
        format!("{proxy_base}/s/{token}")
    };
    let mut out = String::with_capacity(text.len() + 256);
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            out.push_str(line);
        } else if trimmed.starts_with('#') {
            out.push_str(&rewrite_tag_line(line, base_url, &proxy_url));
        } else {
            let absolute = absolutize(base_url, trimmed);
            out.push_str(&proxy_url(&absolute));
        }
        out.push('\n');
    }
    out
}

fn rewrite_tag_line(line: &str, base_url: &str, proxy_url: &impl Fn(&str) -> String) -> String {
    let mut out = String::with_capacity(line.len() + 64);
    let mut rest = line;
    while let Some(start) = rest.find("URI=\"") {
        let after = &rest[start + 5..];
        let Some(end) = after.find('"') else {
            break;
        };
        let absolute = absolutize(base_url, &after[..end]);
        out.push_str(&rest[..start + 5]);
        out.push_str(&proxy_url(&absolute));
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::{absolutize, rewrite_playlist, ProxyServer, Routes};
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::Duration;

    use crate::recording::DEFAULT_UPSTREAM_USER_AGENT;

    fn raw_exchange(url: &str, method: &str, headers: &[(&str, &str)]) -> (String, Vec<u8>) {
        let rest = url.strip_prefix("http://").expect("http url");
        let (authority, path) = rest
            .split_once('/')
            .map_or_else(|| (rest, "/".to_string()), |(a, p)| (a, format!("/{p}")));
        let mut stream = std::net::TcpStream::connect(authority).expect("connect proxy");
        let mut request = format!("{method} {path} HTTP/1.1\r\nHost: {authority}\r\n");
        for (name, value) in headers {
            request.push_str(name);
            request.push_str(": ");
            request.push_str(value);
            request.push_str("\r\n");
        }
        request.push_str("Connection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).expect("send request");
        let mut response = Vec::new();
        stream.read_to_end(&mut response).expect("read response");
        let split = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map_or(response.len(), |index| index + 4);
        (
            String::from_utf8_lossy(&response[..split]).into_owned(),
            response[split..].to_vec(),
        )
    }

    fn capture_upstream(response: &'static [u8]) -> (std::net::SocketAddr, mpsc::Receiver<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buffer = [0_u8; 4096];
                let n = stream.read(&mut buffer).unwrap_or(0);
                let _ = tx.send(String::from_utf8_lossy(&buffer[..n]).into_owned());
                let _ = stream.write_all(response);
            }
        });
        (addr, rx)
    }

    #[test]
    fn absolutize_resolves_relative_entries() {
        let base = "http://example.com/live/list.m3u8";
        assert_eq!(
            absolutize(base, "seg1.ts"),
            "http://example.com/live/seg1.ts"
        );
        assert_eq!(absolutize(base, "/root.ts"), "http://example.com/root.ts");
        assert_eq!(
            absolutize(base, "https://other.com/x.ts"),
            "https://other.com/x.ts"
        );
        assert_eq!(absolutize(base, "//cdn.com/y.ts"), "http://cdn.com/y.ts");
    }

    #[test]
    fn playlist_entries_are_rewritten() {
        let routes: Routes = Arc::new(Mutex::new(HashMap::new()));
        let text = "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\"\nseg1.ts\nhttp://other.com/seg2.ts\n";
        let out = rewrite_playlist(
            text,
            "http://example.com/list.m3u8",
            "http://127.0.0.1:9",
            &routes,
            &[],
        );
        assert!(out.contains("URI=\"http://127.0.0.1:9/s/"), "{out}");
        assert!(out
            .lines()
            .any(|line| line.starts_with("http://127.0.0.1:9/s/")));
        assert_eq!(routes.lock().unwrap().len(), 3);
    }

    #[test]
    fn advertised_host_prefers_the_lan_address() {
        use super::{advertised_host, lan_ip};
        let lan: std::net::IpAddr = "192.168.1.20".parse().expect("ip");
        assert_eq!(advertised_host(Some(lan)), "192.168.1.20");
        assert_eq!(advertised_host(None), "127.0.0.1");
        // Whatever this machine reports, it is never loopback.
        if let Some(ip) = lan_ip() {
            assert!(!ip.is_loopback());
        }
    }

    #[test]
    fn proxy_binds_and_advertises_the_given_host() {
        // A non-loopback bind address stands in for the LAN address:
        // the advertised base and the stream URLs handed to a cast
        // target must carry it, not 127.0.0.1 (the S03 regression:
        // the TV connected to itself).
        let host = super::lan_ip().map_or_else(|| "127.0.0.1".to_string(), |ip| ip.to_string());
        let proxy = ProxyServer::start_on(&host).unwrap();
        assert!(
            proxy.base_url().starts_with(&format!("http://{host}:")),
            "{}",
            proxy.base_url()
        );
        let url = proxy.url_for("http://example.com/seg.ts", &[]);
        assert!(url.starts_with(&format!("http://{host}:")), "{url}");
        assert!(url.contains("/s/"));
    }

    #[test]
    fn proxy_serves_registered_stream() {
        // Upstream: one-shot server returning fixed bytes.
        let upstream = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream_addr = upstream.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = upstream.accept() {
                let mut sink = [0_u8; 1024];
                let _ = stream.read(&mut sink);
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: video/mp2t\r\nContent-Length: 5\r\n\r\nHELLO");
            }
        });
        // Serving is exercised over loopback; the LAN binding that
        // start() picks since S03 is covered by the S03 tests, and a
        // sandbox without a routable LAN address cannot fetch its own
        // advertised address.
        let proxy = ProxyServer::start_on("127.0.0.1").unwrap();
        let url = proxy.url_for(
            &format!("http://{upstream_addr}/seg.ts"),
            &[("User-Agent".to_string(), "test".to_string())],
        );
        let mut response = ureq::get(&url).call().unwrap();
        let body = response.body_mut().read_to_string().unwrap();
        assert_eq!(body, "HELLO");
    }

    #[test]
    fn head_is_forwarded_with_headers_and_no_body() {
        let (addr, requests) = capture_upstream(
            b"HTTP/1.1 200 OK\r\nContent-Type: video/mp2t\r\nContent-Length: 1234\r\nAccept-Ranges: bytes\r\n\r\n",
        );
        let proxy = ProxyServer::start_on("127.0.0.1").unwrap();
        let url = proxy.url_for(&format!("http://{addr}/movie.ts"), &[]);
        let (head, body) = raw_exchange(&url, "HEAD", &[]);
        let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(request.starts_with("HEAD /movie.ts "), "{request}");
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        let lower = head.to_ascii_lowercase();
        assert!(lower.contains("content-length: 1234"), "{head}");
        assert!(lower.contains("accept-ranges: bytes"), "{head}");
        assert!(body.is_empty(), "{body:?}");
    }

    #[test]
    fn redirected_playlist_resolves_against_final_url() {
        let original = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let original_addr = original.local_addr().unwrap();
        let final_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let final_addr = final_listener.local_addr().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = original.accept() {
                let mut buffer = [0_u8; 2048];
                let _ = stream.read(&mut buffer);
                let response = format!(
                    "HTTP/1.1 302 Found\r\nLocation: http://{final_addr}/live/list.m3u8\r\nContent-Length: 0\r\n\r\n"
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        std::thread::spawn(move || {
            for _ in 0..2 {
                let Ok((mut stream, _)) = final_listener.accept() else {
                    return;
                };
                let mut buffer = [0_u8; 4096];
                let n = stream.read(&mut buffer).unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..n]).into_owned();
                if request.starts_with("GET /live/list.m3u8 ") {
                    let body = b"#EXTM3U\nseg1.ts\n";
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/vnd.apple.mpegurl\r\nContent-Length: {}\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(body);
                } else {
                    let _ = tx.send(request);
                    let _ = stream.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: video/mp2t\r\nContent-Length: 3\r\n\r\nSEG",
                    );
                }
            }
        });
        let proxy = ProxyServer::start_on("127.0.0.1").unwrap();
        let url = proxy.url_for(&format!("http://{original_addr}/start.m3u8"), &[]);
        let mut response = ureq::get(&url).call().unwrap();
        let text = response.body_mut().read_to_string().unwrap();
        let segment_url = text
            .lines()
            .find(|line| {
                let line = line.trim();
                !line.is_empty() && !line.starts_with('#')
            })
            .expect("rewritten segment");
        assert!(segment_url.starts_with(proxy.base_url()), "{text}");
        let mut segment = ureq::get(segment_url).call().unwrap();
        let body = segment.body_mut().read_to_string().unwrap();
        assert_eq!(body, "SEG");
        let request = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(request.starts_with("GET /live/seg1.ts "), "{request}");
    }

    #[test]
    fn range_headers_reach_client_and_range_reaches_upstream() {
        let (addr, requests) = capture_upstream(
            b"HTTP/1.1 206 Partial Content\r\nContent-Type: video/mp2t\r\nContent-Length: 3\r\nContent-Range: bytes 0-2/10\r\nAccept-Ranges: bytes\r\nLast-Modified: Wed, 21 Oct 2015 07:28:00 GMT\r\nETag: \"abc\"\r\n\r\nSEG",
        );
        let proxy = ProxyServer::start_on("127.0.0.1").unwrap();
        let url = proxy.url_for(&format!("http://{addr}/seg.ts"), &[]);
        let (head, body) = raw_exchange(&url, "GET", &[("Range", "bytes=0-2")]);
        let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            request.to_ascii_lowercase().contains("range: bytes=0-2"),
            "{request}"
        );
        assert!(head.starts_with("HTTP/1.1 206"), "{head}");
        let lower = head.to_ascii_lowercase();
        for expected in [
            "content-type: video/mp2t",
            "content-length: 3",
            "content-range: bytes 0-2/10",
            "accept-ranges: bytes",
            "last-modified: wed, 21 oct 2015 07:28:00 gmt",
            "etag: \"abc\"",
        ] {
            assert!(lower.contains(expected), "missing {expected}: {head}");
        }
        assert_eq!(body.as_slice(), b"SEG");
    }

    #[test]
    fn uppercase_playlist_mime_is_rewritten() {
        let (addr, _requests) = capture_upstream(
            b"HTTP/1.1 200 OK\r\nContent-Type: APPLICATION/X-MPEGURL\r\nContent-Length: 16\r\n\r\n#EXTM3U\nseg1.ts\n",
        );
        let proxy = ProxyServer::start_on("127.0.0.1").unwrap();
        let url = proxy.url_for(&format!("http://{addr}/list"), &[]);
        let mut response = ureq::get(&url).call().unwrap();
        let text = response.body_mut().read_to_string().unwrap();
        assert!(text.contains("/s/"), "{text}");
        assert!(!text.lines().any(|line| line.trim() == "seg1.ts"), "{text}");
        assert!(
            text.lines().any(|line| line.starts_with(proxy.base_url())),
            "{text}"
        );
    }

    #[test]
    fn default_user_agent_reaches_upstream() {
        let (addr, requests) = capture_upstream(
            b"HTTP/1.1 200 OK\r\nContent-Type: video/mp2t\r\nContent-Length: 0\r\n\r\n",
        );
        let proxy = ProxyServer::start_on("127.0.0.1").unwrap();
        let url = proxy.url_for(&format!("http://{addr}/seg.ts"), &[]);
        let _ = ureq::get(&url).call().unwrap();
        let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
        let expected = format!(
            "user-agent: {}",
            DEFAULT_UPSTREAM_USER_AGENT.to_ascii_lowercase()
        );
        assert!(
            request.to_ascii_lowercase().contains(&expected),
            "{request}"
        );
    }

    #[test]
    fn caller_supplied_user_agent_is_preserved() {
        let (addr, requests) = capture_upstream(
            b"HTTP/1.1 200 OK\r\nContent-Type: video/mp2t\r\nContent-Length: 0\r\n\r\n",
        );
        let proxy = ProxyServer::start_on("127.0.0.1").unwrap();
        let url = proxy.url_for(
            &format!("http://{addr}/seg.ts"),
            &[("User-Agent".to_string(), "Caller-UA".to_string())],
        );
        let _ = ureq::get(&url).call().unwrap();
        let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
        let lower = request.to_ascii_lowercase();
        assert!(lower.contains("user-agent: caller-ua"), "{request}");
        assert!(
            !lower.contains(&DEFAULT_UPSTREAM_USER_AGENT.to_ascii_lowercase()),
            "{request}"
        );
    }
}
