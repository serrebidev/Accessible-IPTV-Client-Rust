//! The recording relay: hand the built-in player the stream a
//! recording is already reading (port of Python's
//! `recording_relay.py`, second-round audit N13).
//!
//! Many providers allow one connection per account. Watching a
//! channel while recording it used to open two: libVLC for the
//! picture and ffmpeg for the file. Instead, the recording's
//! ffmpeg writes a second, untouched MPEG-TS copy of what it
//! reads to its stdout, and this relay serves that copy to the
//! player over local HTTP — one provider connection.
//!
//! The pump reads the pipe no matter what: if ffmpeg ever
//! blocked on a full pipe the recording itself would stall, so
//! a missing or slow player simply misses data. A player that
//! joins starts from a short backlog, so it finds a keyframe
//! quickly instead of waiting for the next one.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::process::ChildStdout;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const CHUNK_BYTES: usize = 64 * 1024;
/// What a player joining mid-stream is handed first.
const BACKLOG_BYTES: usize = 2 * 1024 * 1024;
const BACKLOG_SECONDS: f64 = 2.0;
/// Per-player slack before chunks are dropped for it; the
/// recording never waits.
const CLIENT_QUEUE_CHUNKS: usize = 256;

type Chunk = Arc<Vec<u8>>;

struct Client {
    chunks: VecDeque<(Instant, Chunk)>,
    stream: TcpStream,
}

#[derive(Default)]
struct RelayState {
    backlog: VecDeque<(Instant, Chunk)>,
    backlog_bytes: usize,
    clients: Vec<Arc<Mutex<Client>>>,
    /// The pump reached EOF: everything left will be delivered,
    /// then each client sees EOF.
    closed: bool,
    /// `close()` has begun: the backlog stops trimming by age so
    /// a lagging player receives the tail.
    closing: bool,
    drained: bool,
}

struct Shared {
    state: Mutex<RelayState>,
    cond: Condvar,
}

static TOKEN_COUNTER: AtomicU64 = AtomicU64::new(0);

fn token() -> String {
    let n = TOKEN_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{nanos:016x}{n:08x}")
}

fn trim_backlog(state: &mut RelayState, now: Instant) {
    while !state.backlog.is_empty()
        && (state.backlog_bytes > BACKLOG_BYTES
            || now.duration_since(state.backlog[0].0).as_secs_f64() > BACKLOG_SECONDS)
    {
        if let Some((_, chunk)) = state.backlog.pop_front() {
            state.backlog_bytes -= chunk.len();
        }
    }
}

/// Serve one pipe of MPEG-TS to local players at `url`.
pub struct RecordingRelay {
    /// The loopback URL the player opens (token path included).
    pub url: String,
    shared: Arc<Shared>,
    server_closed: Arc<AtomicBool>,
}

impl RecordingRelay {
    /// Start the relay over the recording's stdout pipe.
    ///
    /// # Errors
    ///
    /// Returns the bind failure when no loopback port is free.
    pub fn new(source: ChildStdout) -> Result<Self, String> {
        let listener =
            TcpListener::bind("127.0.0.1:0").map_err(|e| format!("recording relay: {e}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("recording relay: {e}"))?;
        let port = listener
            .local_addr()
            .map_err(|e| format!("recording relay: {e}"))?
            .port();
        let path = format!("/{}/live.ts", token());
        let url = format!("http://127.0.0.1:{port}{path}");
        let shared = Arc::new(Shared {
            state: Mutex::new(RelayState::default()),
            cond: Condvar::new(),
        });
        let server_closed = Arc::new(AtomicBool::new(false));
        {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || pump(source, &shared));
        }
        {
            let shared = Arc::clone(&shared);
            let server_closed = Arc::clone(&server_closed);
            let path = path.clone();
            std::thread::spawn(move || accept_loop(&listener, &shared, &server_closed, &path));
        }
        Ok(Self {
            url,
            shared,
            server_closed,
        })
    }

    /// Stop serving. Players still connected get what is left,
    /// then EOF. Normally the pump has already drained (ffmpeg
    /// exited first); the wait is bounded for the pipe that
    /// never reaches EOF, after which client sockets are shut
    /// down so blocked writers do not outlive the relay.
    pub fn close(&self) {
        let mut connections: Vec<TcpStream> = Vec::new();
        {
            let Ok(mut state) = self.shared.state.lock() else {
                return;
            };
            state.closing = true;
            self.shared.cond.notify_all();
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                if state.drained || state.closed {
                    break;
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                let (guard, _) = self
                    .shared
                    .cond
                    .wait_timeout(state, remaining)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state = guard;
            }
            if !state.drained {
                state.closed = true;
                self.shared.cond.notify_all();
                connections = state
                    .clients
                    .iter()
                    .filter_map(|client| {
                        client
                            .lock()
                            .ok()
                            .and_then(|client| client.stream.try_clone().ok())
                    })
                    .collect();
            }
        }
        for connection in connections {
            let _ = connection.shutdown(Shutdown::Both);
        }
        self.server_closed.store(true, Ordering::Relaxed);
    }
}

fn pump(mut source: ChildStdout, shared: &Shared) {
    let mut buffer = vec![0u8; CHUNK_BYTES];
    loop {
        match source.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                let chunk: Chunk = Arc::new(buffer[..read].to_vec());
                if let Ok(mut state) = shared.state.lock() {
                    let now = Instant::now();
                    state.backlog_bytes += chunk.len();
                    state.backlog.push_back((now, Arc::clone(&chunk)));
                    if !state.closing {
                        trim_backlog(&mut state, now);
                    }
                    for client in &state.clients {
                        if let Ok(mut client) = client.lock() {
                            client.chunks.push_back((now, Arc::clone(&chunk)));
                            while client.chunks.len() > CLIENT_QUEUE_CHUNKS {
                                client.chunks.pop_front();
                            }
                        }
                    }
                    shared.cond.notify_all();
                }
            }
        }
    }
    if let Ok(mut state) = shared.state.lock() {
        state.drained = true;
        state.closed = true;
        shared.cond.notify_all();
    }
}

fn accept_loop(
    listener: &TcpListener,
    shared: &Arc<Shared>,
    server_closed: &AtomicBool,
    path: &str,
) {
    while !server_closed.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                let shared = Arc::clone(shared);
                let path = path.to_string();
                std::thread::spawn(move || handle(stream, &shared, &path));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

fn handle(mut stream: TcpStream, shared: &Shared, path: &str) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut request = Vec::new();
    let mut byte = [0u8; 1];
    while request.len() < 8192 && !request.ends_with(b"\r\n\r\n") {
        match stream.read(&mut byte) {
            Ok(0) | Err(_) => return,
            Ok(_) => request.push(byte[0]),
        }
    }
    let text = String::from_utf8_lossy(&request);
    let mut parts = text.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");
    let target_path = target.split('?').next().unwrap_or("");
    if target_path != path {
        let _ = stream.write_all(b"HTTP/1.0 404 Not Found\r\nContent-Length: 0\r\n\r\n");
        return;
    }
    let _ = stream.write_all(
        b"HTTP/1.0 200 OK\r\nContent-Type: video/mp2t\r\nCache-Control: no-cache\r\n\r\n",
    );
    if method != "GET" {
        return;
    }
    let _ = stream.set_read_timeout(None);
    let client = {
        let Ok(mut state) = shared.state.lock() else {
            return;
        };
        // A player joining while the recording ends must not
        // lose the tail; once the stream is over, a late join
        // is still trimmed.
        if !state.closing || state.closed {
            trim_backlog(&mut state, Instant::now());
        }
        let Ok(stream_clone) = stream.try_clone() else {
            return;
        };
        let client = Arc::new(Mutex::new(Client {
            chunks: state.backlog.iter().cloned().collect(),
            stream: stream_clone,
        }));
        state.clients.push(Arc::clone(&client));
        client
    };
    serve(&mut stream, shared, &client);
    if let Ok(mut state) = shared.state.lock() {
        state.clients.retain(|c| !Arc::ptr_eq(c, &client));
    }
}

fn serve(stream: &mut TcpStream, shared: &Shared, client: &Arc<Mutex<Client>>) {
    loop {
        let next: Option<Chunk>;
        {
            let Ok(mut state) = shared.state.lock() else {
                return;
            };
            loop {
                let ready = client.lock().is_ok_and(|client| !client.chunks.is_empty());
                if ready || state.closed {
                    break;
                }
                let (guard, _) = shared
                    .cond
                    .wait_timeout(state, Duration::from_secs(1))
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state = guard;
            }
            if !state.closed && !state.closing {
                let now = Instant::now();
                if let Ok(mut client) = client.lock() {
                    while client.chunks.front().is_some_and(|(at, _)| {
                        now.duration_since(*at).as_secs_f64() > BACKLOG_SECONDS
                    }) {
                        client.chunks.pop_front();
                    }
                }
            }
            next = client
                .lock()
                .ok()
                .and_then(|mut client| client.chunks.pop_front().map(|(_, chunk)| chunk));
            if next.is_none() && state.closed {
                return;
            }
        }
        match next {
            Some(chunk) => {
                if stream
                    .write_all(&chunk)
                    .and_then(|()| stream.flush())
                    .is_err()
                {
                    // The player went away; the recording
                    // carries on.
                    return;
                }
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}
