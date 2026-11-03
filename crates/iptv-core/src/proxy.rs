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

/// A running proxy server on an ephemeral port.
pub struct ProxyServer {
    base: String,
    routes: Routes,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
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
        Ok(Self {
            base,
            routes,
            stop,
            handle: Some(handle),
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
    }
}

fn token_for(url: &str) -> String {
    let digest = Sha256::digest(url.as_bytes());
    format!("{digest:x}")[..20].to_string()
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
    let (Some("GET"), Some(path)) = (parts.next(), parts.next()) else {
        respond(&mut stream, 405, "text/plain", b"method not allowed");
        return;
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
    serve(&mut stream, &route, range.as_deref(), routes, cache, base);
}

fn serve(
    stream: &mut std::net::TcpStream,
    route: &Route,
    range: Option<&str>,
    routes: &Routes,
    cache: &SegmentCache,
    base: &str,
) {
    if range.is_none() && is_segment_url(&route.url) {
        if let Some((content_type, bytes)) = cache
            .lock()
            .ok()
            .and_then(|map| map.get(&route.url).cloned())
        {
            respond(stream, 200, &content_type, &bytes);
            return;
        }
    }
    let mut request = ureq::get(&route.url);
    for (name, value) in &route.headers {
        request = request.header(name.as_str(), value.as_str());
    }
    if let Some(range) = range {
        request = request.header("Range", range);
    }
    let mut response = match request.call() {
        Ok(response) => response,
        Err(ureq::Error::StatusCode(code)) => {
            respond(stream, code, "text/plain", b"upstream error");
            return;
        }
        Err(_) => {
            respond(stream, 502, "text/plain", b"upstream unreachable");
            return;
        }
    };
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    let is_playlist = content_type.contains("mpegurl")
        || route
            .url
            .split('?')
            .next()
            .is_some_and(|p| p.to_ascii_lowercase().ends_with(".m3u8"));
    if is_playlist {
        let mut text = String::new();
        let read = response.body_mut().as_reader().read_to_string(&mut text);
        if read.is_ok() {
            let rewritten = rewrite_playlist(&text, &route.url, base, routes, &route.headers);
            respond(stream, status, &content_type, rewritten.as_bytes());
            return;
        }
    }
    // Stream through, accumulating small segment bodies for the cache.
    let segment = range.is_none() && is_segment_url(&route.url);
    let mut head =
        format!("HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nConnection: close\r\n\r\n");
    if stream.write_all(head.as_bytes()).is_err() {
        return;
    }
    head.clear();
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
    use std::sync::{Arc, Mutex};

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
}
