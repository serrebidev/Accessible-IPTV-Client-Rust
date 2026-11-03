//! Local live buffer IO layer (issue #96): the disk spool,
//! loopback HTTP server, ffmpeg ingest and HLS input
//! resolution around `iptv_core::timeshift`'s bookkeeping —
//! the parts the core deliberately left as seams. Mirrors
//! Python's `live_buffer.py`: ffmpeg PUTs 2-second MPEG-TS
//! segments and the m3u8 manifest to a write-token path on
//! 127.0.0.1; the player and recordings GET view playlists
//! and segments from view-token paths; sessions are shared
//! by source key and reference-counted by leases.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iptv_core::playback::{split_stream_modifiers, StreamHeaders};
use iptv_core::timeshift::{BufferSettings, LiveBuffer};
use iptv_core::Channel;

/// One buffered source: core bookkeeping plus its spool
/// directory, loopback server and ingest process.
pub struct BufferSession {
    core: Mutex<LiveBuffer>,
    dir: PathBuf,
    /// `http://127.0.0.1:<port>` the server listens on.
    pub origin: String,
    write_token: String,
    view_token: String,
    seg_lock: Mutex<()>,
    activity: Mutex<(u32, Instant)>,
    closed: AtomicBool,
    process: Mutex<Option<Child>>,
    /// Configured retention (segment list sizing).
    seconds: i64,
    /// Configured byte cap (upload limit).
    max_bytes: u64,
}

static TOKEN_COUNTER: AtomicU64 = AtomicU64::new(0);

fn token() -> String {
    let n = TOKEN_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{nanos:016x}{n:08x}")
}

/// The registry key for a source (Python's `source_key`):
/// SHA-256 of the JSON pair [clean URL, sorted lowercased
/// header pairs] after stream modifiers are merged in.
#[must_use]
pub fn source_key(url: &str, headers: &StreamHeaders) -> String {
    use sha2::{Digest, Sha256};
    let (clean, modifiers) = split_stream_modifiers(url);
    let mut pairs: Vec<(String, String)> = headers
        .entries
        .iter()
        .map(|(k, v)| (k.to_lowercase(), v.clone()))
        .collect();
    for (k, v) in &modifiers.entries {
        let key = k.to_lowercase();
        pairs.retain(|(existing, _)| *existing != key);
        pairs.push((key, v.clone()));
    }
    pairs.sort();
    let payload = serde_json::json!([clean, pairs]).to_string();
    let digest = Sha256::digest(payload.as_bytes());
    digest.iter().fold(String::with_capacity(64), |mut acc, b| {
        use std::fmt::Write;
        let _ = write!(acc, "{b:02x}");
        acc
    })
}

impl BufferSession {
    /// Create the spool directory, core window and loopback
    /// server. Ingest starts separately (`start_ingest`).
    ///
    /// # Errors
    /// Returns a message when the directory or listener
    /// cannot be created, or the core rejects the settings.
    pub fn new(settings: &BufferSettings) -> Result<Arc<Self>, String> {
        let dir = std::env::temp_dir().join(format!("iptv-timeshift-{}", token()));
        std::fs::create_dir_all(&dir).map_err(|e| format!("buffer directory: {e}"))?;
        let core = LiveBuffer::new(settings).map_err(|e| e.to_string())?;
        let listener =
            TcpListener::bind("127.0.0.1:0").map_err(|e| format!("buffer server: {e}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("buffer server: {e}"))?;
        let port = listener
            .local_addr()
            .map_err(|e| format!("buffer server: {e}"))?
            .port();
        let session = Arc::new(Self {
            core: Mutex::new(core),
            dir,
            origin: format!("http://127.0.0.1:{port}"),
            write_token: token(),
            view_token: token(),
            seg_lock: Mutex::new(()),
            activity: Mutex::new((0, Instant::now())),
            closed: AtomicBool::new(false),
            process: Mutex::new(None),
            seconds: settings.seconds,
            max_bytes: settings.max_bytes(),
        });
        let server_session = Arc::clone(&session);
        std::thread::spawn(move || run_server(&server_session, listener));
        Ok(session)
    }

    /// The playlist URL for a view id handed out by `cursor`.
    #[must_use]
    pub fn view_url(&self, view_id: u64) -> String {
        format!("{}/{}/{view_id}/index.m3u8", self.origin, self.view_token)
    }

    /// Whether `url` is one of this session's view playlists.
    #[must_use]
    pub fn owns_view(&self, url: &str) -> bool {
        url.starts_with(&format!("{}/{}/", self.origin, self.view_token))
    }

    /// Take a lease (the owner holds lease 1 from the core's
    /// construction; playback and recordings add more).
    ///
    /// # Errors
    /// Mirrors the core's lease errors (closed session).
    pub fn acquire(&self) -> Result<u64, String> {
        self.core
            .lock()
            .map_err(|_| "buffer lock poisoned".to_string())?
            .acquire()
            .map_err(|e| e.to_string())
    }

    /// Release a lease; the last release closes the session.
    /// Returns true when this release closed it.
    pub fn release(&self, lease: u64) -> bool {
        let mut closed_now = false;
        if let Ok(mut core) = self.core.lock() {
            core.release_lease(lease);
            closed_now = core.is_closed();
        }
        if closed_now {
            self.close();
        }
        closed_now
    }

    /// Current lease count (the disable-dialog gate).
    #[must_use]
    pub fn lease_count(&self) -> usize {
        self.core.lock().map_or(0, |core| core.lease_count())
    }

    /// Open a view `behind` seconds behind live (0 = live
    /// edge). Returns (view URL, start position seconds).
    ///
    /// # Errors
    /// Mirrors the core's cursor errors (no history yet, view
    /// caps, invalid position).
    pub fn cursor(&self, behind: f64, protected: bool) -> Result<(String, f64), String> {
        let mut core = self
            .core
            .lock()
            .map_err(|_| "buffer lock poisoned".to_string())?;
        let cursor = core.cursor(behind, protected).map_err(|e| e.to_string())?;
        Ok((self.view_url(cursor.view_id), cursor.segment.start))
    }

    /// (first, last) retained programme times, if any.
    #[must_use]
    pub fn retained_span(&self) -> Option<(f64, f64)> {
        self.core.lock().ok()?.window().retained_span()
    }

    /// Seconds of retained history available for rewind.
    #[must_use]
    pub fn rewind_range(&self) -> Option<i64> {
        self.core.lock().ok()?.window().rewind_range()
    }

    /// The window's sticky error, if ingest or storage failed.
    #[must_use]
    pub fn window_error(&self) -> Option<String> {
        self.core.lock().ok()?.window().error().map(str::to_string)
    }

    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }

    /// Whether the ingest process has exited.
    #[must_use]
    pub fn process_exited(&self) -> bool {
        if let Ok(mut slot) = self.process.lock() {
            if let Some(child) = slot.as_mut() {
                return matches!(child.try_wait(), Ok(Some(_)));
            }
        }
        false
    }

    /// Start ffmpeg ingest for `url` with the channel's
    /// headers (Python's `LiveBufferSession.start`): stream
    /// modifiers are merged, HLS masters resolved to one
    /// variant, and segments PUT back to this session.
    ///
    /// # Errors
    /// Returns Python's buffer error strings for HLS
    /// resolution failures and spawn failures.
    pub fn start_ingest(
        self: &Arc<Self>,
        url: &str,
        headers: &StreamHeaders,
        cap_mbps: Option<f64>,
    ) -> Result<(), String> {
        let (clean, modifiers) = split_stream_modifiers(url);
        let mut merged = headers.clone();
        for (k, v) in modifiers.entries {
            let lower = k.to_lowercase();
            merged
                .entries
                .retain(|(existing, _)| existing.to_lowercase() != lower);
            merged.entries.push((k, v));
        }
        merged.extra.extend(modifiers.extra);
        let source = resolve_hls_input(&clean, &merged, cap_mbps)?;
        let dest = format!("{}/{}/", self.origin, self.write_token);
        let cmd = ingest_command(&source, &merged, &dest, self.seconds);
        let mut command = Command::new(&cmd[0]);
        command
            .args(&cmd[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = command
            .spawn()
            .map_err(|e| format!("could not start ffmpeg: {e}"))?;
        let stderr = child.stderr.take();
        *self.process.lock().map_err(|_| "lock".to_string())? = Some(child);
        if let Some(mut err) = stderr {
            let session = Arc::clone(self);
            std::thread::spawn(move || {
                let mut line = Vec::new();
                let mut byte = [0u8; 1];
                loop {
                    match err.read(&mut byte) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {
                            if byte[0] == b'\n' {
                                let text = String::from_utf8_lossy(&line).to_string();
                                line.clear();
                                if text.contains("muxed as a private data stream")
                                    || text.contains("not currently supported in container")
                                {
                                    if let Ok(mut core) = session.core.lock() {
                                        core.window_mut().set_error(
                                            "input codec cannot be transparently buffered",
                                        );
                                    }
                                }
                            } else {
                                line.push(byte[0]);
                            }
                        }
                    }
                }
            });
        }
        let session = Arc::clone(self);
        std::thread::spawn(move || {
            // Python's _finish_input: when ffmpeg exits, wait
            // for uploads to drain (0.25 s quiet, 10 s cap),
            // then mark the window ended.
            loop {
                std::thread::sleep(Duration::from_millis(100));
                if session.is_closed() {
                    return;
                }
                if session.process_exited() {
                    break;
                }
            }
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let (uploads, last) = session.activity.lock().map_or((0, Instant::now()), |a| *a);
                let quiet = uploads == 0 && last.elapsed() >= Duration::from_millis(250);
                if quiet || Instant::now() >= deadline || session.is_closed() {
                    if let Ok(mut core) = session.core.lock() {
                        if !quiet && !session.is_closed() {
                            core.window_mut().set_error("final uploads did not drain");
                        }
                        core.window_mut().mark_ended();
                    }
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        Ok(())
    }

    /// Wait until the buffer holds history (Python's
    /// `wait_ready`, 30 s), the window errors, or input ends.
    ///
    /// # Errors
    /// Python's "buffer input unavailable" / "input ended
    /// without history" strings.
    pub fn wait_ready(&self, timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        let mut ended_at: Option<Instant> = None;
        loop {
            if self.is_closed() {
                return Err("buffer input unavailable".to_string());
            }
            {
                let core = self.core.lock().map_err(|_| "lock".to_string())?;
                if core.window().error().is_some() {
                    return Err("buffer input unavailable".to_string());
                }
                if !core.window().snapshot().is_empty() {
                    return Ok(());
                }
            }
            if self.process_exited() {
                let since = *ended_at.get_or_insert_with(Instant::now);
                if since.elapsed() > Duration::from_secs(1) {
                    return Err("input ended without history".to_string());
                }
            }
            if Instant::now() >= deadline {
                return Err("buffer input unavailable".to_string());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Close the session: refuse uploads, kill ingest, drop
    /// the spool. Server threads notice `closed` and exit.
    pub fn close(&self) {
        if self.closed.swap(true, Ordering::Relaxed) {
            return;
        }
        if let Ok(mut core) = self.core.lock() {
            core.close();
        }
        if let Ok(mut slot) = self.process.lock() {
            if let Some(mut child) = slot.take() {
                let _ = child.kill();
            }
        }
        // Wake the accept loop so it can observe the flag.
        let _ = TcpStream::connect(self.origin.trim_start_matches("http://"));
        let dir = self.dir.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            let _ = std::fs::remove_dir_all(&dir);
        });
    }
}

/// Python's ffmpeg ingest command (the segment muxer
/// uploads to the loopback server).
#[must_use]
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)] // Python's math.ceil(seconds / 2) arithmetic, mirrored
pub fn ingest_command(
    source: &str,
    headers: &StreamHeaders,
    dest_base: &str,
    seconds: i64,
) -> Vec<String> {
    let mut cmd: Vec<String> = [
        "ffmpeg",
        "-hide_banner",
        "-loglevel",
        "warning",
        "-nostdin",
        "-rw_timeout",
        "15000000",
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    if source.starts_with("http://") || source.starts_with("https://") {
        for extra in [
            "-reconnect",
            "1",
            "-reconnect_streamed",
            "1",
            "-reconnect_delay_max",
            "5",
        ] {
            cmd.push(extra.to_string());
        }
        cmd.extend(iptv_core::recording::header_input_args(headers));
    } else if source.starts_with("rtsp://") {
        cmd.push("-rtsp_transport".to_string());
        cmd.push("tcp".to_string());
    }
    let list_size = ((seconds as f64 / 2.0).ceil() as i64 + 4).min(1000);
    for part in [
        "-i",
        source,
        "-map",
        "0:v?",
        "-map",
        "0:a?",
        "-map",
        "0:s?",
        "-map",
        "0:d?",
        "-c",
        "copy",
        "-f",
        "segment",
        "-segment_format",
        "mpegts",
        "-segment_time",
        "2",
        "-segment_list_size",
        &list_size.to_string(),
        "-segment_list_type",
        "m3u8",
        "-segment_list",
        &format!("{dest_base}index.m3u8"),
        &format!("{dest_base}seg-%09d.ts"),
    ] {
        cmd.push(part.to_string());
    }
    cmd
}

/// Resolve an HLS master playlist to a single variant URL
/// (Python's `resolve_hls_input`); non-m3u8 sources pass
/// through unchanged.
///
/// # Errors
/// Python's exact error strings for nesting, renditions and
/// unusable variants.
#[allow(clippy::cast_possible_truncation)] // Python int(cap * 1_000_000)
pub fn resolve_hls_input(
    source: &str,
    headers: &StreamHeaders,
    cap_mbps: Option<f64>,
) -> Result<String, String> {
    let path = source.split(['?', '#']).next().unwrap_or(source);
    if !path.to_lowercase().ends_with(".m3u8") {
        return Ok(source.to_string());
    }
    let mut current = source.to_string();
    for _ in 0..4 {
        if !(current.starts_with("http://") || current.starts_with("https://")) {
            return Err("invalid HLS master nesting".to_string());
        }
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(5)))
            .build()
            .new_agent();
        let mut request = agent.get(&current);
        for (name, value) in &headers.entries {
            request = request.header(name, value);
        }
        let mut response = request
            .call()
            .map_err(|_| "invalid HLS metadata".to_string())?;
        let base = response
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .map_or_else(|| current.clone(), ToString::to_string);
        let text = response
            .body_mut()
            .with_config()
            .limit(256 * 1024 + 1)
            .read_to_string()
            .map_err(|_| "oversized HLS metadata".to_string())?;
        if text.len() > 256 * 1024 {
            return Err("oversized HLS metadata".to_string());
        }
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        let lines: Vec<&str> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        if lines.first() != Some(&"#EXTM3U") {
            return Err("invalid HLS metadata".to_string());
        }
        if !lines.iter().any(|l| l.starts_with("#EXT-X-STREAM-INF:")) {
            return Ok(base);
        }
        if lines
            .iter()
            .any(|l| l.starts_with("#EXT-X-MEDIA:") && l.contains("URI="))
        {
            return Err("separate HLS renditions require direct playback".to_string());
        }
        let mut variants: Vec<(i64, String)> = Vec::new();
        let mut bandwidth: Option<i64> = None;
        for line in &lines {
            if let Some(rest) = line.strip_prefix("#EXT-X-STREAM-INF:") {
                bandwidth = Some(parse_bandwidth(rest));
            } else if !line.starts_with('#') {
                if let Some(bw) = bandwidth.take() {
                    let target = join_url(&base, line);
                    if !(target.starts_with("http://") || target.starts_with("https://")) {
                        return Err("invalid HLS variant protocol".to_string());
                    }
                    variants.push((bw, target));
                }
            }
        }
        if variants.is_empty() {
            return Err("no usable HLS variant".to_string());
        }
        let cap = cap_mbps.unwrap_or(0.0);
        let chosen = if cap > 0.0 {
            let limit = (cap * 1_000_000.0) as i64;
            let bounded: Vec<&(i64, String)> = variants
                .iter()
                .filter(|v| v.0 > 0 && v.0 <= limit)
                .collect();
            if let Some(best) = bounded.iter().max_by_key(|v| v.0) {
                (*best).clone()
            } else {
                variants.iter().min_by_key(|v| v.0).unwrap().clone()
            }
        } else {
            variants.iter().max_by_key(|v| v.0).unwrap().clone()
        };
        current = chosen.1;
        // Loop: the variant may itself be a master (Python
        // resolves up to 4 levels); a media playlist returns
        // at the STREAM-INF check above on the next pass —
        // but Python returns the variant directly when it is
        // not itself re-fetched... Python loops the fetch, so
        // mirror it: continue; the no-STREAM-INF branch
        // returns `base` (the fetched URL).
        if !current.to_lowercase().contains(".m3u8") {
            return Ok(current);
        }
    }
    Err("excessive HLS nesting".to_string())
}

fn parse_bandwidth(attributes: &str) -> i64 {
    for key in ["AVERAGE-BANDWIDTH=", "BANDWIDTH="] {
        if let Some(start) = attributes.find(key) {
            let digits: String = attributes[start + key.len()..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            if let Ok(value) = digits.parse() {
                return value;
            }
        }
    }
    0
}

/// Minimal relative URL join (Python's urljoin for the
/// shapes HLS playlists actually use).
fn join_url(base: &str, relative: &str) -> String {
    if relative.starts_with("http://") || relative.starts_with("https://") {
        return relative.to_string();
    }
    let Some(scheme_end) = base.find("://") else {
        return relative.to_string();
    };
    let authority_start = scheme_end + 3;
    if relative.starts_with('/') {
        let host_end = base[authority_start..]
            .find('/')
            .map_or(base.len(), |i| authority_start + i);
        return format!("{}{}", &base[..host_end], relative);
    }
    let dir_end = base.rfind('/').map_or(base.len(), |i| i + 1);
    format!("{}{}", &base[..dir_end], relative)
}

impl Drop for BufferSession {
    fn drop(&mut self) {
        // An unconsumed start outcome must not leak a running
        // ffmpeg or a spool directory.
        self.close();
    }
}

/// What the player is currently watching through the buffer.
pub struct BufferCurrent {
    pub session: Arc<BufferSession>,
    pub lease: u64,
    pub url: String,
    pub channel: Channel,
    pub position: f64,
    pub last_clock_ms: Option<i64>,
}

/// The shared buffer state (one registry per app run).
#[derive(Default)]
pub struct BufferRegistry {
    pub sessions: HashMap<String, Arc<BufferSession>>,
    pub current: Option<BufferCurrent>,
    /// A background start in flight; the worker writes its
    /// outcome into the slot (Arc/Mutex because the registry
    /// itself is UI-thread state), delivered on the player
    /// tick.
    pub pending: Option<Arc<Mutex<Option<BufferOutcome>>>>,
    /// Playback-session generation when the pending start was
    /// launched; a delivery whose generation has moved on (Stop
    /// or a newer channel choice) is dropped, never played.
    pub pending_generation: u64,
    /// Recording key → (session, lease) held while recording.
    pub recording_leases: HashMap<String, (Arc<BufferSession>, u64)>,
    /// Last buffered channel, for Return to Live's resume.
    pub last: Option<(Channel, String)>,
}

/// A completed background buffer start.
// One-shot hand-off from the starter thread; the size difference
// costs nothing at that rate.
#[allow(clippy::large_enum_variant)]
pub enum BufferOutcome {
    Ready {
        session: Arc<BufferSession>,
        key: String,
        lease: u64,
        url: String,
        position: f64,
        channel: Channel,
    },
    Failed,
}

impl BufferRegistry {
    /// Find a live session for a source, dropping closed ones.
    pub fn find(&mut self, url: &str, headers: &StreamHeaders) -> Option<Arc<BufferSession>> {
        let key = source_key(url, headers);
        let session = self.sessions.get(&key)?;
        if session.is_closed() {
            self.sessions.remove(&key);
            return None;
        }
        Some(Arc::clone(session))
    }

    /// Take a recording lease on the live session for `url` and
    /// open its protected live-edge view, storing the lease under
    /// `key`; returns the view URL to record from. One `&mut self`
    /// operation on purpose (audit R21): the adapter used to hold
    /// the registry's `RefCell` borrow from `find` across a second
    /// `borrow_mut` for the lease store, which panics on Rust 2021.
    pub fn recording_view(
        &mut self,
        key: &str,
        url: &str,
        headers: &StreamHeaders,
    ) -> Option<String> {
        let session = self.find(url, headers)?;
        let lease = session.acquire().ok()?;
        if let Ok((view_url, _)) = session.cursor(0.0, true) {
            self.recording_leases
                .insert(key.to_string(), (session, lease));
            Some(view_url)
        } else {
            session.release(lease);
            None
        }
    }

    /// Whether any live session owns this view URL.
    #[must_use]
    pub fn owns_view(&self, url: &str) -> bool {
        self.sessions.values().any(|s| s.owns_view(url))
    }

    /// Release the player's current lease (playback stop or
    /// channel change), closing the session at zero leases.
    pub fn release_current(&mut self) {
        if let Some(current) = self.current.take() {
            self.last = Some((current.channel.clone(), current.url.clone()));
            current.session.release(current.lease);
            if current.session.is_closed() {
                self.sessions.retain(|_, s| !s.is_closed());
            }
        }
    }

    /// Close every session (app shutdown).
    pub fn close_all(&mut self) {
        self.release_current();
        for session in self.sessions.values() {
            session.close();
        }
        self.sessions.clear();
        self.recording_leases.clear();
    }
}

// ----------------------------------------------------------------
// Loopback server
// ----------------------------------------------------------------

#[allow(clippy::needless_pass_by_value)] // the accept loop owns the listener
fn run_server(session: &Arc<BufferSession>, listener: TcpListener) {
    loop {
        if session.is_closed() {
            return;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                let session = Arc::clone(session);
                std::thread::spawn(move || handle_connection(&session, stream));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

fn reply(stream: &mut TcpStream, code: u32, body: &[u8], content_type: &str, head_only: bool) {
    let reason = match code {
        200 => "OK",
        404 => "Not Found",
        410 => "Gone",
        413 => "Payload Too Large",
        _ => "Error",
    };
    let header = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    if !head_only {
        let _ = stream.write_all(body);
    }
}

fn handle_connection(session: &Arc<BufferSession>, mut stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let Ok(request) = read_request_head(&mut stream) else {
        return;
    };
    let mut parts = request.request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let raw_path = parts.next().unwrap_or("");
    let path = raw_path.split('?').next().unwrap_or("");
    let segments: Vec<&str> = path.split('/').collect();
    // Write side: /<write_token>/<name>
    if (method == "PUT" || method == "POST")
        && segments.len() == 3
        && segments[1] == session.write_token
    {
        handle_upload(session, &mut stream, &request, segments[2]);
        return;
    }
    // View side: /<view_token>/<view_id>/<name>
    if (method == "GET" || method == "HEAD")
        && segments.len() == 4
        && segments[1] == session.view_token
    {
        let Ok(view_id) = segments[2].parse::<u64>() else {
            reply(&mut stream, 404, b"", "text/plain", false);
            return;
        };
        handle_view(session, &mut stream, view_id, segments[3], method == "HEAD");
        return;
    }
    reply(&mut stream, 404, b"", "text/plain", false);
}

struct RequestHead {
    request_line: String,
    content_length: Option<u64>,
    chunked: bool,
}

fn read_request_head(stream: &mut TcpStream) -> Result<RequestHead, ()> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).map_err(|_| ())? == 0 {
            return Err(());
        }
        buf.push(byte[0]);
        if buf.len() > 16 * 1024 {
            return Err(());
        }
    }
    let text = String::from_utf8_lossy(&buf).to_string();
    let mut lines = text.lines();
    let request_line = lines.next().unwrap_or("").to_string();
    let mut content_length = None;
    let mut chunked = false;
    for line in lines {
        let lower = line.to_lowercase();
        if let Some(value) = lower.strip_prefix("content-length:") {
            content_length = value.trim().parse().ok();
        }
        if lower.starts_with("transfer-encoding:") && lower.contains("chunked") {
            chunked = true;
        }
    }
    Ok(RequestHead {
        request_line,
        content_length,
        chunked,
    })
}

/// Stream the request body to `sink` in ≤64 KiB pieces,
/// enforcing `limit` (Python's chunk reader rules).
fn read_body(
    stream: &mut TcpStream,
    head: &RequestHead,
    limit: u64,
    sink: &mut dyn FnMut(&[u8]) -> Result<(), String>,
) -> Result<(), String> {
    if head.chunked {
        let mut reader = ChunkReader { stream };
        let mut total: u64 = 0;
        loop {
            let line = reader.read_line(128)?;
            let size_text = line.split(';').next().unwrap_or("").trim();
            let size = u64::from_str_radix(size_text, 16).map_err(|_| "invalid chunk header")?;
            if size == 0 {
                let trailer = reader.read_line(128)?;
                if trailer != "\r\n" && !trailer.is_empty() {
                    // A bare CRLF ends the body; other trailers
                    // are not expected from ffmpeg.
                }
                return Ok(());
            }
            total += size;
            if total > limit {
                return Err("upload exceeds quota".to_string());
            }
            let mut remaining = size;
            while remaining > 0 {
                let want = remaining.min(65_536) as usize;
                let data = reader.read_exact_vec(want)?;
                sink(&data)?;
                remaining -= data.len() as u64;
            }
            let ending = reader.read_exact_vec(2)?;
            if ending != b"\r\n" {
                return Err("invalid chunk ending".to_string());
            }
        }
    }
    let Some(mut left) = head.content_length else {
        return Err("invalid body length".to_string());
    };
    if left > limit {
        return Err("upload exceeds quota".to_string());
    }
    let mut buffer = vec![0u8; 65_536];
    while left > 0 {
        let want = (left.min(65_536)) as usize;
        let n = stream
            .read(&mut buffer[..want])
            .map_err(|_| "truncated upload".to_string())?;
        if n == 0 {
            return Err("truncated upload".to_string());
        }
        sink(&buffer[..n])?;
        left -= n as u64;
    }
    Ok(())
}

struct ChunkReader<'a> {
    stream: &'a mut TcpStream,
}

impl ChunkReader<'_> {
    fn read_line(&mut self, max: usize) -> Result<String, String> {
        let mut line = Vec::new();
        let mut byte = [0u8; 1];
        while !line.ends_with(b"\r\n") {
            if line.len() > max {
                return Err("invalid chunk header".to_string());
            }
            let n = self
                .stream
                .read(&mut byte)
                .map_err(|_| "truncated upload".to_string())?;
            if n == 0 {
                return Err("truncated upload".to_string());
            }
            line.push(byte[0]);
        }
        Ok(String::from_utf8_lossy(&line).to_string())
    }

    fn read_exact_vec(&mut self, count: usize) -> Result<Vec<u8>, String> {
        let mut out = vec![0u8; count];
        self.stream
            .read_exact(&mut out)
            .map_err(|_| "truncated upload".to_string())?;
        Ok(out)
    }
}

fn handle_upload(
    session: &Arc<BufferSession>,
    stream: &mut TcpStream,
    head: &RequestHead,
    name: &str,
) {
    {
        let mut activity = session.activity.lock().unwrap();
        activity.0 += 1;
        activity.1 = Instant::now();
    }
    let code = upload_inner(session, stream, head, name);
    {
        let mut activity = session.activity.lock().unwrap();
        activity.0 -= 1;
        activity.1 = Instant::now();
    }
    reply(stream, code, b"", "text/plain", false);
}

fn upload_inner(
    session: &Arc<BufferSession>,
    stream: &mut TcpStream,
    head: &RequestHead,
    name: &str,
) -> u32 {
    if name == "index.m3u8" {
        let mut body = Vec::new();
        let result = read_body(stream, head, 256 * 1024, &mut |data| {
            body.extend_from_slice(data);
            Ok(())
        });
        if let Err(error) = result {
            set_window_error(session, &error);
            return 413;
        }
        let text = String::from_utf8_lossy(&body).to_string();
        return match session.core.lock() {
            Ok(mut core) => match core.window_mut().publish(&text) {
                Ok(()) => {
                    prune_spool(session);
                    200
                }
                Err(error) => {
                    set_window_error(session, &error.to_string());
                    413
                }
            },
            Err(_) => 413,
        };
    }
    if !is_segment_name(name) {
        return 404;
    }
    let _guard = session.seg_lock.lock().unwrap();
    let path = session.dir.join(name);
    let mut file = match std::fs::File::create(&path) {
        Ok(file) => file,
        Err(error) => {
            set_window_error(session, &format!("storage failed: {error}"));
            return 413;
        }
    };
    let begin = session
        .core
        .lock()
        .map_err(|_| ())
        .and_then(|mut core| core.window_mut().begin_segment(name).map_err(|_| ()));
    if begin.is_err() {
        return 413;
    }
    let mut failure: Option<String> = None;
    let result = read_body(stream, head, session_max_bytes(session), &mut |data| {
        if failure.is_some() {
            return Ok(());
        }
        if let Err(error) = file.write_all(data) {
            failure = Some(format!("storage failed: {error}"));
            return Ok(());
        }
        if let Ok(mut core) = session.core.lock() {
            if let Err(error) = core.window_mut().append_chunk(data.len() as u64) {
                failure = Some(error.to_string());
            }
        }
        Ok(())
    });
    let failure = match result {
        Err(error) => Some(error),
        Ok(()) => failure,
    };
    match failure {
        None => {
            if let Ok(mut core) = session.core.lock() {
                let _ = core.window_mut().finish_segment();
            }
            prune_spool(session);
            200
        }
        Some(error) => {
            abort_segment(session, &path);
            set_window_error(session, &error);
            413
        }
    }
}

fn session_max_bytes(session: &BufferSession) -> u64 {
    session.max_bytes
}

fn set_window_error(session: &BufferSession, message: &str) {
    if let Ok(mut core) = session.core.lock() {
        core.window_mut().set_error(message);
    }
}

fn abort_segment(session: &BufferSession, path: &PathBuf) {
    if let Ok(mut core) = session.core.lock() {
        core.window_mut().abort_segment();
    }
    let _ = std::fs::remove_file(path);
}

/// Delete spool files the window has evicted.
fn prune_spool(session: &BufferSession) {
    let keep: std::collections::HashSet<String> = match session.core.lock() {
        Ok(core) => core
            .window()
            .snapshot()
            .iter()
            .map(|row| row.name.clone())
            .collect(),
        Err(_) => return,
    };
    if let Ok(entries) = std::fs::read_dir(&session.dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if is_segment_name(&name) && !keep.contains(&name) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

fn is_segment_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("seg-") else {
        return false;
    };
    let Some(digits) = rest.strip_suffix(".ts") else {
        return false;
    };
    !digits.is_empty() && digits.len() <= 12 && digits.chars().all(|c| c.is_ascii_digit())
}

fn handle_view(
    session: &Arc<BufferSession>,
    stream: &mut TcpStream,
    view_id: u64,
    name: &str,
    head_only: bool,
) {
    let Ok(mut core) = session.core.lock() else {
        reply(stream, 404, b"", "text/plain", head_only);
        return;
    };
    let Some(first) = core.view_first_sequence(view_id) else {
        reply(stream, 404, b"", "text/plain", head_only);
        return;
    };
    if name == "index.m3u8" {
        let initial = if head_only {
            core.view_is_initial(view_id)
        } else {
            core.take_initial(view_id)
        };
        let body = render_playlist(core.window(), first, initial);
        drop(core);
        reply(
            stream,
            200,
            body.as_bytes(),
            "application/vnd.apple.mpegurl",
            head_only,
        );
        return;
    }
    if !is_segment_name(name) {
        reply(stream, 404, b"", "text/plain", head_only);
        return;
    }
    let row_ok = core
        .window()
        .snapshot()
        .iter()
        .any(|row| row.name == name && row.sequence >= first);
    drop(core);
    if !row_ok {
        reply(stream, 410, b"", "text/plain", head_only);
        return;
    }
    let path = session.dir.join(name);
    let Ok(data) = std::fs::read(&path) else {
        reply(stream, 410, b"", "text/plain", head_only);
        return;
    };
    reply(stream, 200, &data, "video/mp2t", head_only);
}

/// Render one view's playlist (Python's `playlist()` shape).
fn render_playlist(
    window: &iptv_core::timeshift::BufferWindow,
    first: u64,
    initial: bool,
) -> String {
    let mut rows: Vec<&iptv_core::timeshift::Segment> = window
        .snapshot()
        .iter()
        .filter(|row| row.sequence >= first)
        .collect();
    if initial && !window.is_ended() {
        rows.truncate(3);
    }
    if rows.is_empty() {
        return "#EXTM3U\n#EXT-X-TARGETDURATION:2\n".to_string();
    }
    let mut out = String::from("#EXTM3U\n#EXT-X-VERSION:3\n");
    let _ = writeln!(
        out,
        "#EXT-X-TARGETDURATION:{}",
        window.target_duration().unwrap_or(2)
    );
    let _ = writeln!(out, "#EXT-X-MEDIA-SEQUENCE:{}", rows[0].sequence);
    out.push_str("#EXT-X-START:TIME-OFFSET=0,PRECISE=YES\n");
    for row in rows {
        if row.discontinuity {
            out.push_str("#EXT-X-DISCONTINUITY\n");
        }
        let _ = writeln!(out, "#EXTINF:{:.6},\n{}", row.duration, row.name);
    }
    if window.is_ended() {
        out.push_str("#EXT-X-ENDLIST\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn session_with_media() -> Arc<BufferSession> {
        let settings = BufferSettings {
            enabled: true,
            ..BufferSettings::default()
        };
        let session = BufferSession::new(&settings).expect("session");
        {
            let mut core = session.core.lock().expect("core");
            let window = core.window_mut();
            window.begin_segment("seg-000000000.ts").expect("begin");
            window.append_chunk(1024).expect("append");
            window.finish_segment().expect("finish");
            // A cursor needs a published timeline over the segment.
            window
                .publish("#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:0\n#EXTINF:2,\nseg-000000000.ts\n")
                .expect("publish");
        }
        session
    }

    /// Audit R21 regression: acquiring a recording view through
    /// the shared `RefCell` (the exact adapter shape `start_recording`
    /// uses) must store the lease without an overlapping borrow
    /// panic, and must not open a second provider connection (the
    /// returned URL is the session's own loopback view).
    #[test]
    fn recording_view_stores_lease_through_refcell_without_panic() {
        let url = "http://example.com/live";
        let headers = StreamHeaders::default();
        let session = session_with_media();
        let leases_before = session.lease_count();
        let mut registry = BufferRegistry::default();
        registry
            .sessions
            .insert(source_key(url, &headers), Arc::clone(&session));
        let registry = Rc::new(RefCell::new(registry));

        let view = registry
            .borrow_mut()
            .recording_view("rec-key", url, &headers)
            .expect("view");
        assert!(view.starts_with("http://127.0.0.1:"));
        let registry_ref = registry.borrow();
        let (stored_session, _) = registry_ref
            .recording_leases
            .get("rec-key")
            .expect("lease stored");
        assert!(Arc::ptr_eq(stored_session, &session));
        assert_eq!(session.lease_count(), leases_before + 1);
    }

    /// When the cursor cannot open (no retained media yet), the
    /// lease is handed back and nothing is stored.
    #[test]
    fn recording_view_releases_lease_when_cursor_fails() {
        let url = "http://example.com/empty";
        let headers = StreamHeaders::default();
        let settings = BufferSettings {
            enabled: true,
            ..BufferSettings::default()
        };
        let session = BufferSession::new(&settings).expect("session");
        let leases_before = session.lease_count();
        let mut registry = BufferRegistry::default();
        registry
            .sessions
            .insert(source_key(url, &headers), Arc::clone(&session));
        assert!(registry.recording_view("k", url, &headers).is_none());
        assert!(registry.recording_leases.is_empty());
        assert_eq!(session.lease_count(), leases_before);
    }
}
