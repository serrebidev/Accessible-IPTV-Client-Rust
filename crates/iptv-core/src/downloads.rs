//! Background file downloads with progress and cancellation: save a
//! resolved stream or archive URL to disk without touching the UI thread.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// One download request: URL, header pairs, destination path.
#[derive(Clone, Debug)]
pub struct DownloadRequest {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub destination: PathBuf,
}

/// Progress and completion reports from the download worker.
#[derive(Clone, Debug)]
pub enum DownloadEvent {
    Progress { written: u64, total: Option<u64> },
    Finished(Result<u64, String>),
}

enum DownloadJob {
    Start(DownloadRequest, Arc<AtomicBool>),
}

/// A single-download-at-a-time worker (starting a new job while one runs
/// queues behind it, like the other workers' channels).
pub struct DownloadWorker {
    jobs: std::sync::mpsc::Sender<DownloadJob>,
    events: std::sync::Mutex<std::sync::mpsc::Receiver<DownloadEvent>>,
    cancel: Arc<AtomicBool>,
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
        let cancel = Arc::new(AtomicBool::new(false));
        std::thread::spawn(move || {
            while let Ok(DownloadJob::Start(request, cancel)) = job_receiver.recv() {
                let result = run_download(&request, &cancel, &event_sender);
                if event_sender.send(DownloadEvent::Finished(result)).is_err() {
                    break;
                }
            }
        });
        Self {
            jobs: job_sender,
            events: std::sync::Mutex::new(event_receiver),
            cancel,
        }
    }

    /// Start downloading; the shared cancel flag resets for the new job.
    pub fn start(&self, request: DownloadRequest) {
        self.cancel.store(false, Ordering::Relaxed);
        let _ = self
            .jobs
            .send(DownloadJob::Start(request, Arc::clone(&self.cancel)));
    }

    /// Ask the running download to stop; the partial file is removed.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// The next download event, when one is waiting.
    #[must_use]
    pub fn try_recv(&self) -> Option<DownloadEvent> {
        self.events.lock().ok()?.try_recv().ok()
    }
}

fn run_download(
    request: &DownloadRequest,
    cancel: &AtomicBool,
    events: &std::sync::mpsc::Sender<DownloadEvent>,
) -> Result<u64, String> {
    let (clean_url, _) = crate::playback::split_stream_modifiers(&request.url);
    let mut builder = ureq::get(&clean_url);
    for (name, value) in &request.headers {
        builder = builder.header(name, value);
    }
    let mut response = builder
        .call()
        .map_err(|e| format!("Download failed: {e}"))?;
    let total: Option<u64> = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());
    let mut file =
        std::fs::File::create(&request.destination).map_err(|e| format!("Download failed: {e}"))?;
    let mut reader = response.body_mut().as_reader();
    let mut written: u64 = 0;
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        if cancel.load(Ordering::Relaxed) {
            drop(file);
            let _ = std::fs::remove_file(&request.destination);
            return Err("Download cancelled".to_string());
        }
        let read = reader
            .read(&mut buffer)
            .map_err(|e| format!("Download failed: {e}"))?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])
            .map_err(|e| format!("Download failed: {e}"))?;
        written += read as u64;
        let _ = events.send(DownloadEvent::Progress { written, total });
    }
    file.flush().map_err(|e| format!("Download failed: {e}"))?;
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
}
