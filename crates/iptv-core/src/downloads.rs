//! Background file downloads with progress and cancellation: save a
//! resolved stream or archive URL to disk without touching the UI thread.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Total attempts for one download, including the first try (R05).
pub const MAX_ATTEMPTS: u32 = 3;

/// One download request: caller-chosen id, URL, header pairs,
/// destination path.
#[derive(Clone, Debug)]
pub struct DownloadRequest {
    pub id: u64,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub destination: PathBuf,
}

/// Progress and completion reports from the download worker.
#[derive(Clone, Debug)]
pub enum DownloadEvent {
    Progress {
        id: u64,
        written: u64,
        total: Option<u64>,
    },
    Finished {
        id: u64,
        result: Result<u64, String>,
    },
}

enum DownloadJob {
    Start(DownloadRequest, Arc<AtomicBool>),
}

type CancelFlags = Arc<Mutex<HashMap<u64, Arc<AtomicBool>>>>;

/// A single-download-at-a-time worker (starting a new job while one runs
/// queues behind it, like the other workers' channels). Each job carries
/// its own cancel flag, keyed by the request id.
pub struct DownloadWorker {
    jobs: std::sync::mpsc::Sender<DownloadJob>,
    events: Mutex<std::sync::mpsc::Receiver<DownloadEvent>>,
    flags: CancelFlags,
}

impl Default for DownloadWorker {
    fn default() -> Self {
        Self::new()
    }
}

impl DownloadWorker {
    #[must_use]
    pub fn new() -> Self {
        let (job_sender, job_receiver) = std::sync::mpsc::channel::<DownloadJob>();
        let (event_sender, event_receiver) = std::sync::mpsc::channel::<DownloadEvent>();
        let flags: CancelFlags = Arc::new(Mutex::new(HashMap::new()));
        let thread_flags = Arc::clone(&flags);
        std::thread::spawn(move || {
            while let Ok(DownloadJob::Start(request, cancel)) = job_receiver.recv() {
                let id = request.id;
                let result = run_download(&request, &cancel, &event_sender);
                // The job is finished: drop its cancel flag, unless a
                // newer job with the same id has already replaced it.
                if let Ok(mut map) = thread_flags.lock() {
                    if map.get(&id).is_some_and(|f| Arc::ptr_eq(f, &cancel)) {
                        map.remove(&id);
                    }
                }
                if event_sender
                    .send(DownloadEvent::Finished { id, result })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            jobs: job_sender,
            events: Mutex::new(event_receiver),
            flags,
        }
    }

    /// Start downloading. Only this job's cancel flag is created; other
    /// queued or running jobs keep their own flags untouched.
    pub fn start(&self, request: DownloadRequest) {
        let flag = Arc::new(AtomicBool::new(false));
        if let Ok(mut map) = self.flags.lock() {
            map.insert(request.id, Arc::clone(&flag));
        }
        let _ = self.jobs.send(DownloadJob::Start(request, flag));
    }

    /// Ask one job (queued or running) to stop; its staged `.part` file
    /// is removed and no partial file is left at the destination.
    pub fn cancel(&self, id: u64) {
        if let Ok(map) = self.flags.lock() {
            if let Some(flag) = map.get(&id) {
                flag.store(true, Ordering::Relaxed);
            }
        }
    }

    /// Ask every queued or running job to stop.
    pub fn cancel_all(&self) {
        if let Ok(map) = self.flags.lock() {
            for flag in map.values() {
                flag.store(true, Ordering::Relaxed);
            }
        }
    }

    /// The next download event, when one is waiting.
    #[must_use]
    pub fn try_recv(&self) -> Option<DownloadEvent> {
        self.events.lock().ok()?.try_recv().ok()
    }
}

/// The staging path for a download: the destination file name with
/// `.part` appended (R02/R05).
#[must_use]
pub fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.as_os_str().to_owned();
    name.push(".part");
    PathBuf::from(name)
}

/// True when `bytes` look like an M3U playlist: after an optional UTF-8
/// BOM and any leading whitespace, they start with `#EXTM3U`.
#[must_use]
pub fn sniff_is_playlist(bytes: &[u8]) -> bool {
    let after_bom = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    let mut index = 0;
    while index < after_bom.len() && after_bom[index].is_ascii_whitespace() {
        index += 1;
    }
    after_bom[index..].starts_with(b"#EXTM3U")
}

/// True when the URL path (query ignored) ends with `.m3u8` or `.m3u`,
/// case-insensitively.
#[must_use]
pub fn url_looks_like_playlist(url: &str) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let lower = path.to_lowercase();
    let extension = std::path::Path::new(&lower)
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("");
    matches!(extension, "m3u8" | "m3u")
}

/// How an archive URL must be saved: copied directly, or captured
/// through ffmpeg because the URL is a playlist (R02).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArchivePlan {
    Direct,
    FfmpegCapture,
}

/// Decide the archive plan from the URL and, when known, the response
/// content type. HLS playlists need an ffmpeg capture; everything else
/// downloads directly.
#[must_use]
pub fn archive_plan(url: &str, content_type: Option<&str>) -> ArchivePlan {
    if url_looks_like_playlist(url) {
        return ArchivePlan::FfmpegCapture;
    }
    if let Some(content_type) = content_type {
        let mime = content_type
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_lowercase();
        if matches!(
            mime.as_str(),
            "application/vnd.apple.mpegurl"
                | "application/x-mpegurl"
                | "application/mpegurl"
                | "audio/mpegurl"
                | "audio/x-mpegurl"
        ) {
            return ArchivePlan::FfmpegCapture;
        }
    }
    ArchivePlan::Direct
}

struct AttemptFailure {
    message: String,
    retryable: bool,
    cancelled: bool,
}

impl AttemptFailure {
    fn retryable(message: String) -> Self {
        Self {
            message,
            retryable: true,
            cancelled: false,
        }
    }
    fn fatal(message: String) -> Self {
        Self {
            message,
            retryable: false,
            cancelled: false,
        }
    }
    fn cancelled() -> Self {
        Self {
            message: "Download cancelled".to_string(),
            retryable: false,
            cancelled: true,
        }
    }
}

fn sleep_cancellable(cancel: &AtomicBool, duration: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + duration;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return true;
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            return false;
        }
        std::thread::sleep(std::cmp::min(
            deadline - now,
            std::time::Duration::from_millis(10),
        ));
    }
}

fn run_download(
    request: &DownloadRequest,
    cancel: &AtomicBool,
    events: &std::sync::mpsc::Sender<DownloadEvent>,
) -> Result<u64, String> {
    let part = part_path(&request.destination);
    let delays = [
        std::time::Duration::from_millis(200),
        std::time::Duration::from_millis(800),
    ];
    let mut attempt = 0;
    loop {
        attempt += 1;
        if cancel.load(Ordering::Relaxed) {
            let _ = std::fs::remove_file(&part);
            return Err("Download cancelled".to_string());
        }
        match run_download_once(request, &part, cancel, events) {
            Ok(written) => {
                if let Err(e) = std::fs::rename(&part, &request.destination) {
                    let _ = std::fs::remove_file(&part);
                    return Err(format!("Download failed: {e}"));
                }
                return Ok(written);
            }
            Err(failure) => {
                let _ = std::fs::remove_file(&part);
                if failure.cancelled {
                    return Err(failure.message);
                }
                if !failure.retryable || attempt >= MAX_ATTEMPTS {
                    return Err(failure.message);
                }
                let delay = delays
                    .get((attempt - 1) as usize)
                    .copied()
                    .unwrap_or(std::time::Duration::from_millis(800));
                if sleep_cancellable(cancel, delay) {
                    let _ = std::fs::remove_file(&part);
                    return Err("Download cancelled".to_string());
                }
            }
        }
    }
}

fn run_download_once(
    request: &DownloadRequest,
    part: &Path,
    cancel: &AtomicBool,
    events: &std::sync::mpsc::Sender<DownloadEvent>,
) -> Result<u64, AttemptFailure> {
    if cancel.load(Ordering::Relaxed) {
        return Err(AttemptFailure::cancelled());
    }
    let (clean_url, _) = crate::playback::split_stream_modifiers(&request.url);
    let mut builder = ureq::get(&clean_url);
    for (name, value) in &request.headers {
        builder = builder.header(name, value);
    }
    let mut response = builder.call().map_err(|e| match e {
        ureq::Error::StatusCode(code) if (500..=599).contains(&code) => {
            AttemptFailure::retryable(format!("Download failed: {e}"))
        }
        ureq::Error::StatusCode(_) => AttemptFailure::fatal(format!("Download failed: {e}")),
        _ => AttemptFailure::retryable(format!("Download failed: {e}")),
    })?;
    let total: Option<u64> = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());
    let mut file = std::fs::File::create(part)
        .map_err(|e| AttemptFailure::retryable(format!("Download failed: {e}")))?;
    let mut reader = response.body_mut().as_reader();
    let mut written: u64 = 0;
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(AttemptFailure::cancelled());
        }
        let read = reader.read(&mut buffer).map_err(|e| {
            if total.is_some_and(|t| written < t) {
                AttemptFailure::retryable("Download ended early".to_string())
            } else {
                AttemptFailure::retryable(format!("Download failed: {e}"))
            }
        })?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])
            .map_err(|e| AttemptFailure::retryable(format!("Download failed: {e}")))?;
        written += read as u64;
        let _ = events.send(DownloadEvent::Progress {
            id: request.id,
            written,
            total,
        });
    }
    file.flush()
        .map_err(|e| AttemptFailure::retryable(format!("Download failed: {e}")))?;
    if cancel.load(Ordering::Relaxed) {
        return Err(AttemptFailure::cancelled());
    }
    if total.is_some_and(|t| written < t) {
        return Err(AttemptFailure::retryable(
            "Download ended early".to_string(),
        ));
    }
    Ok(written)
}

/// Header pairs for a channel's stream request (User-Agent, Referer,
/// Cookie, Authorization and the extra header lines), for downloads and
/// recordings to send exactly what playback sends.
#[must_use]
pub fn channel_headers(channel: &crate::Channel) -> Vec<(String, String)> {
    let headers = crate::playback::StreamHeaders::for_channel(channel);
    let mut pairs: Vec<(String, String)> = headers.entries.clone();
    for line in &headers.extra {
        if let Some((name, value)) = line.split_once(':') {
            pairs.push((name.trim().to_string(), value.trim().to_string()));
        }
    }
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::net::TcpListener;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn headers_come_from_channel_attributes_and_modifiers() {
        let mut attrs = BTreeMap::new();
        attrs.insert("http-user-agent".to_string(), "Agent/1.0".to_string());
        let channel = crate::Channel {
            name: "C".to_string(),
            group: String::new(),
            url: "http://example.com/s|Referer=http://ref.example/".to_string(),
            tvg_id: String::new(),
            tvg_name: String::new(),
            tvg_logo: String::new(),
            unknown_attributes: attrs,
        };
        let pairs = channel_headers(&channel);
        assert!(pairs.contains(&("User-Agent".to_string(), "Agent/1.0".to_string())));
        assert!(pairs.contains(&("Referer".to_string(), "http://ref.example/".to_string())));
    }

    // ----- local HTTP test server helpers -----

    fn read_request(stream: &mut std::net::TcpStream) {
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 64 * 1024 {
                        break;
                    }
                }
            }
        }
    }

    fn write_response(
        stream: &mut std::net::TcpStream,
        status: &str,
        content_length: usize,
        body: &[u8],
    ) {
        use std::io::Write as _;
        let head = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {content_length}\r\nConnection: close\r\n\r\n"
        );
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(body);
        let _ = stream.flush();
    }

    fn start_server<F>(handler: F) -> String
    where
        F: FnMut(usize, &mut std::net::TcpStream) + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let mut handler = handler;
        std::thread::spawn(move || {
            let mut count = 0_usize;
            for stream in listener.incoming() {
                match stream {
                    Ok(mut stream) => {
                        read_request(&mut stream);
                        handler(count, &mut stream);
                        count += 1;
                    }
                    Err(_) => break,
                }
            }
        });
        format!("http://{addr}/file")
    }

    fn temp_dest(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "iptv-dl-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let _ = std::fs::create_dir_all(&dir);
        dir.join(name)
    }

    fn collect_finished(worker: &DownloadWorker, want: usize) -> Vec<(u64, Result<u64, String>)> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut finished = Vec::new();
        while finished.len() < want {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {want} finished events, got {finished:?}"
            );
            match worker.try_recv() {
                Some(DownloadEvent::Finished { id, result }) => finished.push((id, result)),
                Some(DownloadEvent::Progress { .. }) => {}
                None => std::thread::sleep(std::time::Duration::from_millis(5)),
            }
        }
        finished
    }

    fn request(id: u64, url: &str, destination: PathBuf) -> DownloadRequest {
        DownloadRequest {
            id,
            url: url.to_string(),
            headers: Vec::new(),
            destination,
        }
    }

    // ----- R05: staging, success, retry, early EOF -----

    #[test]
    fn download_success_writes_final_file_and_removes_part() {
        let body = b"hello media bytes";
        let url = start_server(|_n, stream| {
            write_response(stream, "200 OK", body.len(), body);
        });
        let dest = temp_dest("ok.ts");
        let worker = DownloadWorker::new();
        worker.start(request(1, &url, dest.clone()));
        let finished = collect_finished(&worker, 1);
        assert_eq!(finished.len(), 1);
        assert_eq!(finished[0].0, 1);
        assert_eq!(finished[0].1, Ok(body.len() as u64));
        assert_eq!(std::fs::read(&dest).expect("final file"), body.to_vec());
        assert!(!part_path(&dest).exists(), ".part must not remain");
        let _ = std::fs::remove_dir_all(dest.parent().expect("parent"));
    }

    #[test]
    fn download_retries_503_then_succeeds() {
        let body = b"after retry";
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_server = Arc::clone(&hits);
        let url = start_server(move |_n, stream| {
            let hit = hits_server.fetch_add(1, Ordering::SeqCst);
            if hit == 0 {
                write_response(stream, "503 Service Unavailable", 0, b"");
            } else {
                write_response(stream, "200 OK", body.len(), body);
            }
        });
        let dest = temp_dest("retry.ts");
        let worker = DownloadWorker::new();
        worker.start(request(7, &url, dest.clone()));
        let finished = collect_finished(&worker, 1);
        assert_eq!(finished[0].1, Ok(body.len() as u64));
        assert_eq!(hits.load(Ordering::SeqCst), 2, "one 503 + one 200");
        assert_eq!(std::fs::read(&dest).expect("final file"), body.to_vec());
        assert!(!part_path(&dest).exists());
        let _ = std::fs::remove_dir_all(dest.parent().expect("parent"));
    }

    #[test]
    fn download_early_eof_fails_without_final_or_part_file() {
        let url = start_server(|_n, stream| {
            // Declare 100 bytes, deliver 5, then close cleanly.
            write_response(stream, "200 OK", 100, b"short");
        });
        let dest = temp_dest("early.ts");
        let worker = DownloadWorker::new();
        worker.start(request(3, &url, dest.clone()));
        let finished = collect_finished(&worker, 1);
        assert_eq!(finished[0].0, 3);
        assert!(finished[0].1.is_err(), "early EOF must fail: {finished:?}");
        assert!(!dest.exists(), "no partial final file");
        assert!(!part_path(&dest).exists(), "no .part left behind");
        let _ = std::fs::remove_dir_all(dest.parent().expect("parent"));
    }

    #[test]
    fn download_404_is_not_retried() {
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_server = Arc::clone(&hits);
        let url = start_server(move |_n, stream| {
            hits_server.fetch_add(1, Ordering::SeqCst);
            write_response(stream, "404 Not Found", 0, b"");
        });
        let dest = temp_dest("missing.ts");
        let worker = DownloadWorker::new();
        worker.start(request(4, &url, dest.clone()));
        let finished = collect_finished(&worker, 1);
        assert!(finished[0].1.is_err());
        assert_eq!(hits.load(Ordering::SeqCst), 1, "4xx must not retry");
        assert!(!dest.exists());
        assert!(!part_path(&dest).exists());
        let _ = std::fs::remove_dir_all(dest.parent().expect("parent"));
    }

    // ----- R06 support: per-id cancel -----

    #[test]
    fn cancel_queued_job_by_id_spares_running_and_later_jobs() {
        use std::io::Write as _;
        let fast_body = b"fast";
        let url = start_server(move |n, stream| {
            if n == 0 {
                // Slow dripping first response: 12 bytes, ~50 ms apart.
                let head = "HTTP/1.1 200 OK\r\nContent-Length: 12\r\nConnection: close\r\n\r\n";
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.flush();
                for _ in 0..12 {
                    let _ = stream.write_all(b"x");
                    let _ = stream.flush();
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            } else {
                write_response(stream, "200 OK", fast_body.len(), fast_body);
            }
        });
        let dir = temp_dest("dir");
        let _ = std::fs::create_dir_all(&dir);
        let dest1 = dir.join("one.ts");
        let dest2 = dir.join("two.ts");
        let dest3 = dir.join("three.ts");
        let worker = DownloadWorker::new();
        worker.start(request(1, &url, dest1.clone()));
        worker.start(request(2, &url, dest2.clone()));
        // Give job 1 a moment to start, then cancel only queued job 2.
        std::thread::sleep(std::time::Duration::from_millis(30));
        worker.cancel(2);
        worker.start(request(3, &url, dest3.clone()));

        let finished = collect_finished(&worker, 3);
        assert_eq!(finished[0].0, 1);
        assert_eq!(finished[0].1, Ok(12));
        assert_eq!(finished[1].0, 2);
        assert!(
            finished[1].1.is_err(),
            "canceled job must fail: {finished:?}"
        );
        assert_eq!(finished[2].0, 3);
        assert_eq!(finished[2].1, Ok(fast_body.len() as u64));
        assert!(dest1.exists());
        assert!(!dest2.exists(), "canceled job leaves no final file");
        assert!(!part_path(&dest2).exists());
        assert_eq!(
            std::fs::read(&dest3).expect("third file"),
            fast_body.to_vec()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancel_all_stops_running_job_without_final_file() {
        use std::io::Write as _;
        let url = start_server(|_n, stream| {
            let head = "HTTP/1.1 200 OK\r\nContent-Length: 600\r\nConnection: close\r\n\r\n";
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.flush();
            for _ in 0..60 {
                if stream.write_all(b"0123456789").is_err() {
                    break;
                }
                let _ = stream.flush();
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        });
        let dest = temp_dest("cancel-all.ts");
        let worker = DownloadWorker::new();
        worker.start(request(9, &url, dest.clone()));
        std::thread::sleep(std::time::Duration::from_millis(60));
        worker.cancel_all();
        let finished = collect_finished(&worker, 1);
        assert!(finished[0].1.is_err());
        assert!(!dest.exists());
        assert!(!part_path(&dest).exists());
        let _ = std::fs::remove_dir_all(dest.parent().expect("parent"));
    }

    // ----- R02 helpers -----

    #[test]
    fn part_path_appends_part_to_file_name() {
        assert_eq!(
            part_path(Path::new("/tmp/show.ts")),
            PathBuf::from("/tmp/show.ts.part")
        );
        assert_eq!(part_path(Path::new("a.m3u8")), PathBuf::from("a.m3u8.part"));
    }

    #[test]
    fn sniff_is_playlist_cases() {
        assert!(sniff_is_playlist(b"#EXTM3U\n#EXTINF:-1,x\n"));
        assert!(sniff_is_playlist(b"\xEF\xBB\xBF#EXTM3U\n"));
        assert!(sniff_is_playlist(b"  \r\n\t#EXTM3U"));
        assert!(sniff_is_playlist(b"\xEF\xBB\xBF  #EXTM3U"));
        assert!(!sniff_is_playlist(b"#EXTINF:-1,x\n"));
        assert!(!sniff_is_playlist(b"not a playlist"));
        assert!(!sniff_is_playlist(b""));
        assert!(!sniff_is_playlist(b"#extm3u"));
    }

    #[test]
    fn url_looks_like_playlist_cases() {
        assert!(url_looks_like_playlist("http://x/live.m3u8"));
        assert!(url_looks_like_playlist("http://x/live.M3U8?token=1"));
        assert!(url_looks_like_playlist("http://x/list.m3u"));
        assert!(url_looks_like_playlist("http://x/list.M3U?a=b&c=d"));
        assert!(!url_looks_like_playlist("http://x/movie.ts"));
        assert!(!url_looks_like_playlist("http://x/movie.mp4?f=.m3u8"));
        assert!(!url_looks_like_playlist("http://x/m3u8"));
    }

    #[test]
    fn archive_plan_cases() {
        assert_eq!(
            archive_plan("http://x/live.m3u8", None),
            ArchivePlan::FfmpegCapture
        );
        assert_eq!(
            archive_plan("http://x/live.m3u8?token=1", Some("video/mp2t")),
            ArchivePlan::FfmpegCapture
        );
        assert_eq!(
            archive_plan("http://x/archive", Some("application/vnd.apple.mpegurl")),
            ArchivePlan::FfmpegCapture
        );
        assert_eq!(
            archive_plan(
                "http://x/archive",
                Some("application/x-mpegurl; charset=utf-8")
            ),
            ArchivePlan::FfmpegCapture
        );
        assert_eq!(
            archive_plan("http://x/archive", Some("audio/mpegurl")),
            ArchivePlan::FfmpegCapture
        );
        assert_eq!(
            archive_plan("http://x/movie.ts", Some("video/mp2t")),
            ArchivePlan::Direct
        );
        assert_eq!(
            archive_plan("http://x/movie.mp4", None),
            ArchivePlan::Direct
        );
    }
}
