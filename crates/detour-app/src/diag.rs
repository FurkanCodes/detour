//! Measurements shown on the Connect page. Nothing here runs in the
//! background on its own accord: latency is one TCP handshake every few
//! seconds while the window is visible, speed tests run only when asked, and
//! the site check runs once after connecting. All work happens on short-lived
//! threads, never on the UI thread.
//!
//! The speed test works like fast.com: several parallel transfers run for a
//! fixed time, throughput is sampled four times a second for the live graph,
//! and the result is the average after a short warm-up (TCP slow start).

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const HOST: &str = "speed.cloudflare.com";
/// Cloudflare refuses single requests above about 50 MB, so each stream
/// repeats this request until the time is up.
const DOWN_URL: &str = "https://speed.cloudflare.com/__down?bytes=50000000";
const UP_URL: &str = "https://speed.cloudflare.com/__up";
const DOWN_STREAMS: usize = 4;
const UP_STREAMS: usize = 3;
pub const DOWN_SECS: f32 = 10.0;
pub const UP_SECS: f32 = 8.0;
const WARMUP_SECS: f32 = 1.5;
const SAMPLE_EVERY: Duration = Duration::from_millis(250);
/// Safety net: a transfer process never outlives this, even if Detour is
/// killed mid-test.
const CURL_MAX_SECS: &str = "20";
const PROBE_EVERY: Duration = Duration::from_secs(5);
const LATENCY_HISTORY: usize = 24;
const SAMPLE_HISTORY: usize = 64;

pub type Repaint = Arc<dyn Fn() + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Download,
    Upload,
    Sites,
}

#[derive(Debug)]
pub enum Event {
    Latency(f32),
    Phase(Phase),
    DownloadSample(f32),
    UploadSample(f32),
    DownloadResult(Result<f32, &'static str>),
    UploadResult(Result<f32, &'static str>),
    Sites(Vec<(String, bool)>),
    ProbeDone,
}

fn curl() -> Command {
    let mut c = Command::new("curl");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // no console window
    }
    c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    c
}

fn null_device() -> &'static str {
    if cfg!(windows) {
        "NUL"
    } else {
        "/dev/null"
    }
}

/// Megabits per second.
pub fn mbps(bytes: u64, secs: f32) -> f32 {
    if secs <= 0.0 {
        return 0.0;
    }
    (bytes as f64 * 8.0 / 1e6 / f64::from(secs)) as f32
}

/// Milliseconds for one TCP handshake.
pub fn latency(addr: SocketAddr) -> Option<f32> {
    let start = Instant::now();
    TcpStream::connect_timeout(&addr, Duration::from_secs(3)).ok()?;
    Some(start.elapsed().as_secs_f32() * 1000.0)
}

/// A site counts as reachable when a full HTTPS request completes.
pub fn check_sites(hosts: &[String]) -> Vec<(String, bool)> {
    hosts
        .iter()
        .map(|host| {
            let ok = curl()
                .stdout(Stdio::piped())
                .args(["-s", "-o", null_device(), "--max-time", "8", "-w", "%{http_code}"])
                .arg(format!("https://{host}"))
                .output()
                .is_ok_and(|o| o.status.success() && o.stdout != b"000");
            (host.clone(), ok)
        })
        .collect()
}

type Slot = Arc<Mutex<Option<Child>>>;

/// curl's exit code for an HTTP error status when run with `--fail`.
const CURL_HTTP_ERROR: i32 = 22;

pub const RATE_LIMITED: &str =
    "The speed-test server is limiting requests from your connection. Try again in a few minutes.";
pub const TEST_FAILED: &str = "Could not reach the speed-test server.";

fn kill(slot: &Slot) {
    if let Some(child) = slot.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Waits for the transfer to end and returns curl's exit code.
fn exit_code(slot: &Slot) -> Option<i32> {
    let mut guard = slot.lock().unwrap_or_else(|e| e.into_inner());
    guard.as_mut()?.wait().ok()?.code()
}

/// State shared by the streams of one measurement.
struct Shared {
    counter: AtomicU64,
    done: AtomicBool,
    /// The server answered with an HTTP error (usually 429 Too Many Requests).
    refused: AtomicBool,
}

/// One download stream: repeats the request until `done`, adding every
/// received byte to the counter. `--fail` keeps error responses out of the
/// byte count.
fn download_stream(shared: Arc<Shared>, slot: Slot) {
    while !shared.done.load(Ordering::Acquire) {
        let child = curl()
            .stdout(Stdio::piped())
            .args(["-s", "--fail", "--max-time", CURL_MAX_SECS, DOWN_URL])
            .spawn();
        let Ok(mut child) = child else { return };
        let Some(mut out) = child.stdout.take() else { return };
        *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
        let mut buf = vec![0u8; 64 * 1024];
        let mut got_any = false;
        while let Ok(n) = out.read(&mut buf) {
            if n == 0 || shared.done.load(Ordering::Acquire) {
                break;
            }
            got_any = true;
            shared.counter.fetch_add(n as u64, Ordering::Relaxed);
        }
        if shared.done.load(Ordering::Acquire) {
            break;
        }
        if exit_code(&slot) == Some(CURL_HTTP_ERROR) {
            shared.refused.store(true, Ordering::Release);
            return;
        }
        if !got_any {
            return; // failing endpoint: do not spin
        }
    }
    kill(&slot);
}

/// One upload stream: feeds zeros to a streaming upload until `done`.
/// Writes block when the network is slower than the pipe, so the count of
/// bytes written tracks what was sent.
fn upload_stream(shared: Arc<Shared>, slot: Slot) {
    let child = curl()
        .stdin(Stdio::piped())
        .args(["-s", "--fail", "-o", null_device(), "--max-time", CURL_MAX_SECS, "-X", "POST", "-T", "-"])
        .args(["-H", "Content-Type: application/octet-stream", UP_URL])
        .spawn();
    let Ok(mut child) = child else { return };
    let Some(mut input) = child.stdin.take() else { return };
    *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
    let buf = vec![0u8; 64 * 1024];
    while !shared.done.load(Ordering::Acquire) {
        match input.write(&buf) {
            Ok(n) => {
                shared.counter.fetch_add(n as u64, Ordering::Relaxed);
            }
            Err(_) => break,
        }
    }
    drop(input);
    if !shared.done.load(Ordering::Acquire) && exit_code(&slot) == Some(CURL_HTTP_ERROR) {
        shared.refused.store(true, Ordering::Release);
        return;
    }
    kill(&slot);
}

/// Runs `streams` transfers for `secs`, reporting Mbps samples, and returns
/// the average after the warm-up. If the server refuses any request the
/// numbers would be wrong, so that is reported as an error instead.
fn measure(
    streams: usize,
    secs: f32,
    upload: bool,
    stop: &AtomicBool,
    mut on_sample: impl FnMut(f32),
) -> Result<f32, &'static str> {
    let shared = Arc::new(Shared {
        counter: AtomicU64::new(0),
        done: AtomicBool::new(false),
        refused: AtomicBool::new(false),
    });
    let slots: Vec<Slot> = (0..streams).map(|_| Arc::new(Mutex::new(None))).collect();
    let workers: Vec<_> = slots
        .iter()
        .map(|slot| {
            let (sh, s) = (shared.clone(), slot.clone());
            std::thread::spawn(move || {
                if upload {
                    upload_stream(sh, s)
                } else {
                    download_stream(sh, s)
                }
            })
        })
        .collect();
    let counter = &shared.counter;
    let done = &shared.done;

    let start = Instant::now();
    let (mut last_bytes, mut last_t) = (0u64, 0f32);
    let mut warm: Option<(u64, f32)> = None;
    loop {
        std::thread::sleep(SAMPLE_EVERY);
        let t = start.elapsed().as_secs_f32();
        let bytes = counter.load(Ordering::Relaxed);
        on_sample(mbps(bytes - last_bytes, t - last_t));
        (last_bytes, last_t) = (bytes, t);
        if warm.is_none() && t >= WARMUP_SECS {
            warm = Some((bytes, t));
        }
        if t >= secs || stop.load(Ordering::Acquire) || shared.refused.load(Ordering::Acquire) {
            break;
        }
    }
    done.store(true, Ordering::Release);
    for slot in &slots {
        kill(slot);
    }
    for w in workers {
        let _ = w.join();
    }
    if shared.refused.load(Ordering::Acquire) {
        return Err(RATE_LIMITED);
    }
    let (b0, t0) = warm.unwrap_or((0, 0.0));
    let (bytes, t) = (last_bytes - b0, last_t - t0);
    if bytes > 0 && t > 0.0 {
        Ok(mbps(bytes, t))
    } else {
        Err(TEST_FAILED)
    }
}

pub struct Monitor {
    tx: Sender<Event>,
    rx: Receiver<Event>,
    /// Live samples of the latest test, for the graphs.
    pub download_samples: VecDeque<f32>,
    pub upload_samples: VecDeque<f32>,
    pub latency: VecDeque<f32>,
    /// Final results of the latest test.
    pub download: Option<f32>,
    pub upload: Option<f32>,
    /// Why the latest speed test has no result, if it failed.
    pub speed_error: Option<&'static str>,
    pub sites: Option<Vec<(String, bool)>>,
    pub phase: Phase,
    pub phase_started: Option<Instant>,
    probing: bool,
    last_probe: Option<Instant>,
    addr: Option<SocketAddr>,
    stop: Arc<AtomicBool>,
}

fn push(history: &mut VecDeque<f32>, v: f32, cap: usize) {
    if history.len() >= cap {
        history.pop_front();
    }
    history.push_back(v);
}

impl Monitor {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            tx,
            rx,
            download_samples: VecDeque::new(),
            upload_samples: VecDeque::new(),
            latency: VecDeque::new(),
            download: None,
            upload: None,
            speed_error: None,
            sites: None,
            phase: Phase::Idle,
            phase_started: None,
            probing: false,
            last_probe: None,
            addr: None,
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn testing(&self) -> bool {
        self.phase != Phase::Idle
    }

    /// Applies finished measurements. Call once per frame.
    pub fn poll(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                Event::Latency(v) => push(&mut self.latency, v, LATENCY_HISTORY),
                Event::Phase(p) => {
                    self.phase = p;
                    self.phase_started = Some(Instant::now());
                }
                Event::DownloadSample(v) => push(&mut self.download_samples, v, SAMPLE_HISTORY),
                Event::UploadSample(v) => push(&mut self.upload_samples, v, SAMPLE_HISTORY),
                Event::DownloadResult(v) | Event::UploadResult(v) => {
                    let slot = if matches!(event, Event::DownloadResult(_)) {
                        &mut self.download
                    } else {
                        &mut self.upload
                    };
                    match v {
                        Ok(mbps) => *slot = Some(mbps),
                        Err(why) => self.speed_error = Some(why),
                    }
                }
                Event::Sites(s) => self.sites = Some(s),
                Event::ProbeDone => self.probing = false,
            }
        }
    }

    /// Starts a latency probe if one is due. Call only while the window is
    /// visible.
    pub fn probe_if_due(&mut self, repaint: &Repaint) {
        if self.probing || self.last_probe.is_some_and(|t| t.elapsed() < PROBE_EVERY) {
            return;
        }
        self.last_probe = Some(Instant::now());
        self.probing = true;
        let (tx, repaint, cached) = (self.tx.clone(), repaint.clone(), self.addr);
        std::thread::spawn(move || {
            let addr = cached.or_else(|| (HOST, 443).to_socket_addrs().ok()?.next());
            if let Some(ms) = addr.and_then(latency) {
                let _ = tx.send(Event::Latency(ms));
            }
            let _ = tx.send(Event::ProbeDone);
            repaint();
        });
        if self.addr.is_none() {
            self.addr = (HOST, 443).to_socket_addrs().ok().and_then(|mut a| a.next());
        }
    }

    pub fn check_sites_later(&self, hosts: Vec<String>, delay: Duration, repaint: &Repaint) {
        let (tx, repaint) = (self.tx.clone(), repaint.clone());
        std::thread::spawn(move || {
            std::thread::sleep(delay);
            let _ = tx.send(Event::Sites(check_sites(&hosts)));
            repaint();
        });
    }

    /// Download for `DOWN_SECS`, upload for `UP_SECS`, then the site check.
    pub fn run_speed_test(&mut self, hosts: Vec<String>, repaint: &Repaint) {
        if self.testing() {
            return;
        }
        self.download_samples.clear();
        self.upload_samples.clear();
        self.download = None;
        self.upload = None;
        self.speed_error = None;
        self.phase = Phase::Download;
        self.phase_started = Some(Instant::now());
        let (tx, repaint, stop) = (self.tx.clone(), repaint.clone(), self.stop.clone());
        std::thread::spawn(move || {
            let send = |e: Event| {
                let _ = tx.send(e);
                repaint();
            };
            send(Event::Phase(Phase::Download));
            let down = measure(DOWN_STREAMS, DOWN_SECS, false, &stop, |v| send(Event::DownloadSample(v)));
            let refused = down == Err(RATE_LIMITED);
            send(Event::DownloadResult(down));
            // A refused download means the upload would be refused too.
            if !stop.load(Ordering::Acquire) && !refused {
                send(Event::Phase(Phase::Upload));
                let up = measure(UP_STREAMS, UP_SECS, true, &stop, |v| send(Event::UploadSample(v)));
                send(Event::UploadResult(up));
            }
            if !stop.load(Ordering::Acquire) {
                send(Event::Phase(Phase::Sites));
                send(Event::Sites(check_sites(&hosts)));
            }
            send(Event::Phase(Phase::Idle));
        });
    }

    pub fn forget_sites(&mut self) {
        self.sites = None;
    }
}

impl Drop for Monitor {
    /// Ends a running speed test (and its transfer processes) with the app.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn converts_bytes_to_mbps() {
        assert!((mbps(12_500_000, 1.0) - 100.0).abs() < 1e-3);
        assert!((mbps(12_500_000, 2.0) - 50.0).abs() < 1e-3);
        assert_eq!(mbps(1000, 0.0), 0.0);
    }

    #[test]
    fn measures_tcp_latency() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let ms = latency(listener.local_addr().unwrap()).unwrap();
        assert!((0.0..1000.0).contains(&ms));
        let port = listener.local_addr().unwrap();
        drop(listener);
        assert!(latency(port).is_none());
    }

    #[test]
    fn monitor_applies_events_and_caps_history() {
        let mut m = Monitor::new();
        m.phase = Phase::Download;
        for i in 0..(SAMPLE_HISTORY + 5) {
            m.tx.send(Event::DownloadSample(i as f32)).unwrap();
        }
        m.tx.send(Event::DownloadResult(Ok(321.0))).unwrap();
        m.tx.send(Event::UploadResult(Err(RATE_LIMITED))).unwrap();
        m.tx.send(Event::Latency(12.0)).unwrap();
        m.tx.send(Event::Sites(vec![("a.com".into(), true), ("b.com".into(), false)])).unwrap();
        m.tx.send(Event::Phase(Phase::Idle)).unwrap();
        m.poll();
        assert_eq!(m.download_samples.len(), SAMPLE_HISTORY);
        assert_eq!(*m.download_samples.back().unwrap(), (SAMPLE_HISTORY + 4) as f32);
        assert_eq!(m.download, Some(321.0));
        assert_eq!(m.upload, None);
        assert_eq!(m.speed_error, Some(RATE_LIMITED));
        assert_eq!(m.latency.len(), 1);
        assert_eq!(m.sites.as_ref().unwrap().len(), 2);
        assert!(!m.testing());
        m.forget_sites();
        assert!(m.sites.is_none());
    }

    #[test]
    fn probes_are_rate_limited() {
        let mut m = Monitor::new();
        m.last_probe = Some(Instant::now());
        let repaint: Repaint = Arc::new(|| {});
        m.probe_if_due(&repaint);
        assert!(!m.probing, "a probe ran before the interval elapsed");
    }

    #[test]
    #[ignore = "uses the network: about 10 s and several hundred MB"]
    fn live_download_measurement() {
        let stop = AtomicBool::new(false);
        let mut samples = Vec::new();
        match measure(DOWN_STREAMS, 5.0, false, &stop, |v| samples.push(v)) {
            Ok(result) => {
                assert!(samples.len() >= 15, "about four samples a second");
                assert!(result > 1.0, "{result} Mbps");
            }
            // A refusal must be reported as such, quickly, not as a speed.
            Err(why) => {
                assert_eq!(why, RATE_LIMITED);
                eprintln!("server refused the test after {} samples", samples.len());
            }
        }
    }
}
